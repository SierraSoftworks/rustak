//! Keeping the public certificate current.
//!
//! Hourly rather than daily, and not because a 90-day certificate needs
//! checking twenty-four times as often: the first order on a fresh
//! installation is placed by this job, and an operator who has just pointed
//! DNS at their server should not wait until tomorrow to find out whether it
//! worked. Every run after that is one cheap row read —
//! [`decide`](crate::pki::acme::decide) answers
//! `Wait` without touching the network.
//!
//! # Failure is not the job's failure
//!
//! An order that the authority refuses is recorded on the row, audited, and
//! logged with the authority's own words; the run then returns `Ok`. Returning
//! the error would hand the retry to the job host's own back-off, which knows
//! nothing about certificate authority rate limits — and
//! [`renew::backoff`](crate::pki::acme::backoff) does. An hour later the
//! schedule comes round again and the row decides whether it is time.

use chrono::TimeDelta;

use crate::pki::acme;
use crate::prelude::*;
use crate::register_job;

/// The queue partition this job owns.
pub const ACME_RENEW_PARTITION: &str = "housekeeping/acme-renew";

/// The idempotency key a renewal somebody asked for is queued under.
///
/// Distinct from the schedule's, so that "renew now" is not swallowed by the
/// pending hourly message — and shared between clicks, so that leaning on the
/// button does not place ten orders.
pub const ACME_RENEW_FORCED_KEY: &str = "housekeeping/acme-renew/forced";

/// How often the certificate is checked.
///
/// Not configurable; `[acme] renew_before` is, and that is the number an
/// operator has an opinion about.
pub const RENEW_INTERVAL: TimeDelta = TimeDelta::hours(1);

/// The message this job runs on.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcmeRenewTask {
    /// Whether to order regardless of the schedule and the back-off.
    ///
    /// Set by `POST /api/v1/settings/tls/renew`, and by nothing else: a forced
    /// order spends real rate limit against the authority.
    #[serde(default)]
    pub forced: bool,
}

/// Orders and renews the public listener's certificate.
pub struct AcmeRenewJob;

register_job!(AcmeRenewJob);

impl Job for AcmeRenewJob {
    type JobType = AcmeRenewTask;

    fn partition() -> &'static str {
        ACME_RENEW_PARTITION
    }

    fn propagate_parent() -> bool {
        false
    }

    /// How long one run may take before the message is offered to somebody
    /// else.
    ///
    /// Generous: an order waits for an authority to validate a name, which
    /// means polling with back-off, and the default would hand the message to
    /// a second worker while the first was still mid-order.
    fn timeout(&self) -> TimeDelta {
        TimeDelta::minutes(10)
    }

    /// Arms the schedule to run at once, when ACME is switched on.
    ///
    /// Immediately, because this is what places the first order: an
    /// installation that has just been configured for ACME has no certificate
    /// at all until this runs.
    async fn setup(&self, services: impl Services + Send + Sync + 'static) -> Result<(), Error> {
        if !services.config().acme.enabled {
            debug!("ACME is switched off, so no renewal is scheduled.");

            return Ok(());
        }

        Self::dispatch(
            AcmeRenewTask::default(),
            Some(ACME_RENEW_PARTITION.into()),
            &services,
        )
        .await
    }

    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), Error> {
        let services = ctx.services();

        // Re-armed before the work, so an order that fails is one missed check
        // rather than the end of the schedule. A forced run does not re-arm:
        // it is an extra message beside the schedule, not the schedule itself.
        if !job.forced && services.config().acme.enabled {
            Self::dispatch_delayed(
                AcmeRenewTask::default(),
                Some(ACME_RENEW_PARTITION.into()),
                RENEW_INTERVAL,
                &services,
            )
            .await?;
        }

        if !services.config().acme.enabled {
            return Ok(());
        }

        // `run` records the failure on the row, audits it and logs it with the
        // authority's own words; the back-off it sets is what the next run
        // consults. Returning it here would replace that with the job host's.
        match acme::run(&services, acme::resolver(), job.forced).await {
            Ok(state) => {
                debug!(?state, "The ACME certificate was checked.");

                Ok(())
            }
            Err(err) => {
                warn!(error = %err, "The ACME renewal did not complete.");

                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::{AcmeDirectory, TlsMode};
    use crate::jobs::JobRunnable;

    /// A context with ACME switched on, pointed at a directory that is not
    /// there: enough to exercise the scheduling without an authority.
    async fn configured() -> AppContext {
        AppContext::new_mock(|config| {
            config.web.public.tls.mode = TlsMode::Acme;
            config.acme.enabled = true;
            config.acme.accept_tos = true;
            config.acme.directory = AcmeDirectory::Url("http://127.0.0.1:1/directory".to_string());
            config.acme.domains = vec!["tak.example.com".to_string()];
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn an_installation_without_acme_schedules_nothing() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        Job::setup(&AcmeRenewJob, context.clone()).await.unwrap();

        assert!(
            context
                .queue()
                .peek::<_, AcmeRenewTask>(ACME_RENEW_PARTITION, 10)
                .await
                .unwrap()
                .is_empty(),
        );
    }

    #[tokio::test]
    async fn setting_up_arms_the_first_order_to_run_at_once() {
        let context = configured().await;

        Job::setup(&AcmeRenewJob, context.clone()).await.unwrap();

        let due = context
            .queue()
            .try_dequeue_any(TimeDelta::seconds(30))
            .await
            .unwrap()
            .expect("the first order should be due immediately");

        assert_eq!(due.partition, ACME_RENEW_PARTITION);
    }

    #[tokio::test]
    async fn an_order_that_cannot_reach_the_authority_re_arms_rather_than_giving_up() {
        // The failure that matters most: a server started before its DNS was
        // ready must keep trying, and must not take the job host down with it.
        let context = configured().await;

        JobRunnable::handle(
            &AcmeRenewJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &serde_json::json!({}),
        )
        .await
        .expect("a failed order is not a failed job");

        let armed = context
            .queue()
            .peek::<_, AcmeRenewTask>(ACME_RENEW_PARTITION, 10)
            .await
            .unwrap();

        assert_eq!(armed.len(), 1, "the schedule survives a failed order");
        assert!(armed[0].hidden_until > chrono::Utc::now() + TimeDelta::minutes(50));
    }

    #[tokio::test]
    async fn a_failed_order_is_recorded_so_the_back_off_applies_to_the_next_one() {
        let context = configured().await;

        Job::handle(
            &AcmeRenewJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &AcmeRenewTask::default(),
        )
        .await
        .unwrap();

        let stored = acme::store::load(context.db(), &["tak.example.com".to_string()])
            .await
            .unwrap()
            .expect("a failed order still reserves the row it failed on");

        assert_eq!(stored.attempts, 1);
        assert!(stored.last_error.is_some());
    }

    #[tokio::test]
    async fn a_run_somebody_asked_for_does_not_re_arm_the_schedule() {
        // Otherwise every click would add an hourly schedule of its own.
        let context = configured().await;

        Job::handle(
            &AcmeRenewJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &AcmeRenewTask { forced: true },
        )
        .await
        .unwrap();

        assert!(
            context
                .queue()
                .peek::<_, AcmeRenewTask>(ACME_RENEW_PARTITION, 10)
                .await
                .unwrap()
                .is_empty(),
        );
    }
}
