//! Dropping CloudTAK hand-over bundles nobody came back for.
//!
//! [`identity::cloudtak`](crate::identity::cloudtak) is the one flow in which
//! this server generates and holds a client's private key, and it is held for
//! ten minutes. That window is enforced on *read* — an expired bundle is never
//! served, whatever else happens — and swept again whenever the next hand-over
//! is prepared. Both of those are properties of a request, which is the gap
//! this job closes: an installation that onboards CloudTAK once and never again
//! makes no further request, so the last sealed, passphrase-protected keystore
//! would sit in the key/value store for the life of the volume, backed up with
//! it and restored with it.
//!
//! Nothing can open that row without the passphrase, which was shown once and
//! written nowhere — but "unreadable" is not "gone", and key material whose
//! purpose has expired should not be waiting for a request that may never come.
//!
//! The work is [`identity::cloudtak::sweep`](crate::identity::cloudtak::sweep);
//! this is the schedule around it, registered like every other job so that it
//! is visible in the queue and can be run early by hand.

use chrono::{TimeDelta, Utc};

use crate::identity::cloudtak;
use crate::prelude::*;
use crate::register_job;

/// The queue partition this job owns.
pub const CLOUDTAK_SWEEP_PARTITION: &str = "housekeeping/cloudtak-sweep";

/// How often the stash is swept.
///
/// Comfortably inside the hour and comfortably outside the bundle's own
/// ten-minute window (`BUNDLE_TTL_MINUTES`), so an abandoned keystore is gone
/// within twenty-five minutes of being prepared rather than within a day. The
/// sweep is one read over a partition this feature alone writes to, and that
/// partition holds at most a handful of rows, so running it four times an hour
/// costs nothing worth configuring. Not configurable for the same reason the
/// TTL is not: neither is something an operator has an opinion about.
pub const SWEEP_INTERVAL: TimeDelta = TimeDelta::minutes(15);

/// The message this job runs on. The window is a constant and the clock is read
/// at run time, so there is nothing to carry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudTakSweepTask {}

/// Removes prepared CloudTAK keystores whose window has closed.
pub struct CloudTakSweepJob;

register_job!(CloudTakSweepJob);

impl Job for CloudTakSweepJob {
    type JobType = CloudTakSweepTask;

    fn partition() -> &'static str {
        CLOUDTAK_SWEEP_PARTITION
    }

    fn propagate_parent() -> bool {
        false
    }

    /// Arms the schedule to run at once.
    ///
    /// Immediately rather than an interval from now, unlike the content sweep:
    /// a server that was down over the moment a bundle expired has been holding
    /// that key ever since, and the sweep is a single read of a partition with
    /// a handful of rows in it — there is nothing here that a restart is a bad
    /// moment for.
    async fn setup(&self, services: impl Services + Send + Sync + 'static) -> Result<(), Error> {
        Self::dispatch(
            CloudTakSweepTask {},
            Some(CLOUDTAK_SWEEP_PARTITION.into()),
            &services,
        )
        .await
    }

    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        _job: &Self::JobType,
    ) -> Result<(), Error> {
        let services = ctx.services();

        // Re-armed before the work, so a sweep that fails is one missed sweep
        // rather than the end of the schedule. Nothing is served from a bundle
        // a missed sweep left behind — the window is still enforced on read —
        // so a failure costs fifteen minutes of storage and no more.
        Self::dispatch_delayed(
            CloudTakSweepTask {},
            Some(CLOUDTAK_SWEEP_PARTITION.into()),
            SWEEP_INTERVAL,
            &services,
        )
        .await?;

        let removed = cloudtak::sweep(services.db(), Utc::now()).await?;

        // Silent when there is nothing to remove, which is the ordinary case on
        // every installation that is not onboarding CloudTAK this quarter-hour.
        if removed > 0 {
            debug!(
                removed,
                "Removed CloudTAK hand-over bundles that nobody collected."
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::jobs::{JobRegistration, JobRunnable};
    use crate::services::AppContext;

    /// Prepares a bundle the way a hand-over does, and reports its identifier.
    ///
    /// Through `stash` rather than through the endpoint because the only thing
    /// the endpoint adds here is the RSA-2048 key it generates for the client,
    /// which is very nearly the whole cost of running it.
    async fn stash(context: &AppContext, certificate: i64) -> String {
        let username = Username::parse("cloudtak").unwrap();

        cloudtak::stash(
            context,
            &username,
            CertificateId::new(certificate),
            b"a keystore waiting to be collected".to_vec(),
        )
        .await
        .expect("a bundle waiting from a hand-over")
        .0
    }

    /// Closes a stashed bundle's window by rewriting the stored expiry.
    ///
    /// No test waits ten minutes out: what makes a bundle stale is the
    /// timestamp on the row, so this runs in the time one write takes and
    /// depends on no clock.
    async fn backdate(context: &AppContext, id: &str) {
        let mut stored: serde_json::Value = context
            .db()
            .get(cloudtak::BUNDLE_PARTITION, id.to_string())
            .await
            .unwrap()
            .expect("the bundle is waiting");

        stored["expires_at"] = serde_json::json!(Utc::now() - TimeDelta::minutes(1));

        context
            .db()
            .set(cloudtak::BUNDLE_PARTITION, id.to_string(), stored)
            .await
            .unwrap();
    }

    /// The identifiers still waiting in the stash.
    async fn waiting(context: &AppContext) -> Vec<String> {
        let stored: Vec<(String, serde_json::Value)> =
            context.db().list(cloudtak::BUNDLE_PARTITION).await.unwrap();

        stored.into_iter().map(|(key, _)| key).collect()
    }

    #[test]
    fn the_partition_is_the_one_the_registry_dispatches_on() {
        assert_eq!(
            <CloudTakSweepJob as Job>::partition(),
            CLOUDTAK_SWEEP_PARTITION,
        );

        // Registration happens at link time, so this is what proves the job
        // actually runs rather than merely compiling.
        let registered = inventory::iter::<JobRegistration>
            .into_iter()
            .any(|registration| registration.handler().partition() == CLOUDTAK_SWEEP_PARTITION);

        assert!(registered, "the sweep should be in the job registry");
    }

    #[tokio::test]
    async fn setting_up_arms_the_schedule_to_run_at_once() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        Job::setup(&CloudTakSweepJob, context.clone())
            .await
            .unwrap();

        let due = context
            .queue()
            .try_dequeue_any(TimeDelta::seconds(30))
            .await
            .unwrap()
            .expect("the sweep should be due immediately");

        assert_eq!(due.partition, CLOUDTAK_SWEEP_PARTITION);
    }

    #[tokio::test]
    async fn a_run_removes_an_expired_bundle_leaves_a_live_one_and_re_arms() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        // Both are stashed before either is backdated: `stash` sweeps on its
        // way in, so closing the first one's window earlier would have let the
        // second stash remove it and left this test proving nothing.
        let stale = stash(&context, 1).await;
        let live = stash(&context, 2).await;
        backdate(&context, &stale).await;

        JobRunnable::handle(
            &CloudTakSweepJob,
            JobContext::new(context.clone(), Utc::now(), None, None),
            &serde_json::json!({}),
        )
        .await
        .unwrap();

        assert_eq!(
            waiting(&context).await,
            vec![live],
            "only the bundle still inside its window should have stayed",
        );

        let armed = context
            .queue()
            .peek::<_, CloudTakSweepTask>(CLOUDTAK_SWEEP_PARTITION, 10)
            .await
            .unwrap();

        assert_eq!(armed.len(), 1, "the run should have re-armed itself");
        assert!(armed[0].hidden_until > Utc::now() + TimeDelta::minutes(14));
    }

    #[tokio::test]
    async fn a_stash_with_nothing_expired_in_it_is_left_alone() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let live = stash(&context, 3).await;

        Job::handle(
            &CloudTakSweepJob,
            JobContext::new(context.clone(), Utc::now(), None, None),
            &CloudTakSweepTask {},
        )
        .await
        .unwrap();

        assert_eq!(waiting(&context).await, vec![live]);
    }
}
