//! Watching the certificate files `[web.public.tls] mode = "files"` reads.
//!
//! Every `[web.public.tls] reload_interval` — thirty seconds by default —
//! this stats the pair and, when either file has moved, re-reads it, proves
//! that the key belongs to the leaf, and installs it into the listener's
//! resolver. No restart, and no connection dropped: see
//! [`pki::tls::files`](crate::pki::tls::files) for why it polls rather than
//! watching, and for what happens to a pair that does not go together.
//!
//! # Failure is not the job's failure
//!
//! A pair that cannot be read is recorded on the listener, reported by
//! `GET /api/v1/settings/tls` and logged once — once, because the fingerprint
//! that failed is remembered and the same bytes are not read again. The run
//! then returns `Ok`: handing it to the job host's back-off would stop the
//! *schedule*, and the schedule is what notices the operator fixing it.

use chrono::TimeDelta;

use crate::pki::tls::files::{self, Outcome};
use crate::prelude::*;
use crate::register_job;

/// The queue partition this job owns.
pub const TLS_FILES_PARTITION: &str = "housekeeping/tls-files";

/// The idempotency key a reload somebody asked for is queued under.
///
/// Distinct from the schedule's, so that `POST /api/v1/settings/tls/renew` is
/// not swallowed by the pending scheduled message — and shared between
/// clicks, so that leaning on the button queues one reload.
pub const TLS_FILES_RELOAD_KEY: &str = "housekeeping/tls-files/forced";

/// The message this job runs on.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TlsFilesTask {
    /// Whether this run is an extra one beside the schedule, and so must not
    /// re-arm it.
    #[serde(default)]
    pub forced: bool,
}

/// Re-reads `[web.public.tls] cert_file` and `key_file` when they change.
pub struct TlsFilesJob;

register_job!(TlsFilesJob);

impl Job for TlsFilesJob {
    type JobType = TlsFilesTask;

    fn partition() -> &'static str {
        TLS_FILES_PARTITION
    }

    fn propagate_parent() -> bool {
        false
    }

    /// Two `stat` calls and, at most, two small files: a run that takes longer
    /// than this is a filesystem that has stopped answering.
    fn timeout(&self) -> TimeDelta {
        TimeDelta::minutes(1)
    }

    /// Arms the schedule, the first look one interval out, leaving a look that
    /// is already armed alone.
    ///
    /// Not immediate: [`web::tls`](crate::web::tls) has just read the pair, or
    /// just said that it could not. Not a plain enqueue either, which would push
    /// an armed look a whole interval out at every restart; see
    /// [`Job::arm_recurring`].
    async fn setup(&self, services: impl Services + Send + Sync + 'static) -> Result<(), Error> {
        let Some(interval) = services.config().web.public.tls.reload_every() else {
            debug!(
                "The public listener does not read its certificate from disk, or the check is \
                 switched off, so nothing is watching for one."
            );

            return Ok(());
        };

        Self::arm_recurring(TlsFilesTask::default(), interval, interval, &services).await
    }

    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), Error> {
        let services = ctx.services();
        let config = services.config();

        // Re-armed before the work, so a look that fails is one missed look
        // rather than the end of the schedule. A forced run does not re-arm:
        // it is an extra message beside the schedule, not the schedule itself.
        if !job.forced
            && let Some(interval) = config.web.public.tls.reload_every()
        {
            Self::dispatch_delayed(
                TlsFilesTask::default(),
                Some(TLS_FILES_PARTITION.into()),
                interval,
                &services,
            )
            .await?;
        }

        let Some(watched) = files::for_config(&config) else {
            return Ok(());
        };

        // On the blocking pool: this reads files an operator's agent may be
        // writing over NFS, and a stall there must not hold a worker thread.
        let outcome = tokio::task::spawn_blocking(move || watched.reload())
            .await
            .or_system_err(&[
                "This is unexpected; please report it with the surrounding log entries.",
            ])?;

        report(&outcome);

        Ok(())
    }
}

/// Says what changed, and nothing at all when nothing did.
fn report(outcome: &Outcome) {
    match outcome {
        Outcome::Swapped { not_after } => info!(
            not_after = ?not_after,
            "The public listener is now presenting the certificate on disk. Existing connections \
             keep the one they negotiated with; every new handshake gets this one."
        ),
        Outcome::Rejected(err) => warn!(
            reason = %err.description(),
            "The certificate files changed and the new pair cannot be served, so the previous one \
             is still being presented. GET /api/v1/settings/tls reports this."
        ),
        Outcome::Waiting => debug!("The public certificate files are not on disk yet."),
        Outcome::Unchanged => debug!("The public certificate files have not changed."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    use crate::config::TlsMode;
    use crate::jobs::JobRunnable;

    /// A pair on disk, as an operator's agent writes it.
    fn write_pair(directory: &Path) -> Vec<u8> {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let certificate = rcgen::CertificateParams::new(vec!["tak.example.com".to_string()])
            .unwrap()
            .self_signed(&key)
            .unwrap();

        std::fs::write(directory.join("chain.pem"), certificate.pem()).unwrap();
        std::fs::write(directory.join("key.pem"), key.serialize_pem()).unwrap();

        certificate.der().to_vec()
    }

    /// A context whose public listener reads its certificate from `directory`.
    async fn watching(directory: &Path) -> AppContext {
        let (cert_file, key_file) = (directory.join("chain.pem"), directory.join("key.pem"));

        AppContext::new_mock(move |config| {
            config.web.public.tls.mode = TlsMode::Files;
            config.web.public.tls.cert_file = Some(cert_file.clone());
            config.web.public.tls.key_file = Some(key_file.clone());
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn an_installation_that_does_not_read_its_certificate_from_disk_schedules_nothing() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        Job::setup(&TlsFilesJob, context.clone()).await.unwrap();

        assert!(
            context
                .queue()
                .peek::<_, TlsFilesTask>(TLS_FILES_PARTITION, 10)
                .await
                .unwrap()
                .is_empty(),
        );
    }

    #[tokio::test]
    async fn switching_the_check_off_schedules_nothing() {
        // `reload_interval = "0"` is how an operator whose files never change
        // says so; the renew endpoint is still there for the day they do.
        let directory = tempfile::tempdir().unwrap();
        let (cert_file, key_file) = (
            directory.path().join("chain.pem"),
            directory.path().join("key.pem"),
        );

        let context = AppContext::new_mock(move |config| {
            config.web.public.tls.mode = TlsMode::Files;
            config.web.public.tls.cert_file = Some(cert_file.clone());
            config.web.public.tls.key_file = Some(key_file.clone());
            config.web.public.tls.reload_interval = TimeDelta::zero();
        })
        .await
        .unwrap();

        Job::setup(&TlsFilesJob, context.clone()).await.unwrap();

        assert!(
            context
                .queue()
                .peek::<_, TlsFilesTask>(TLS_FILES_PARTITION, 10)
                .await
                .unwrap()
                .is_empty(),
        );
    }

    #[tokio::test]
    async fn setting_up_arms_the_schedule_one_interval_out() {
        let directory = tempfile::tempdir().unwrap();
        let context = watching(directory.path()).await;

        Job::setup(&TlsFilesJob, context.clone()).await.unwrap();

        let armed = context
            .queue()
            .peek::<_, TlsFilesTask>(TLS_FILES_PARTITION, 10)
            .await
            .unwrap();

        assert_eq!(armed.len(), 1);
        assert!(armed[0].hidden_until > chrono::Utc::now() + TimeDelta::seconds(20));
    }

    #[tokio::test]
    async fn a_restart_does_not_postpone_a_look_that_is_already_armed() {
        let directory = tempfile::tempdir().unwrap();
        let context = watching(directory.path()).await;
        TlsFilesJob::dispatch_delayed(
            TlsFilesTask::default(),
            Some(TLS_FILES_PARTITION.into()),
            TimeDelta::seconds(10),
            &context,
        )
        .await
        .unwrap();
        let armed_for = async || {
            context
                .queue()
                .peek::<_, TlsFilesTask>(TLS_FILES_PARTITION, 10)
                .await
                .unwrap()[0]
                .hidden_until
        };
        let before = armed_for().await;

        Job::setup(&TlsFilesJob, context.clone()).await.unwrap();

        assert_eq!(armed_for().await, before);
    }

    #[tokio::test]
    async fn a_run_re_arms_the_schedule_even_with_no_listener_to_swap() {
        // A job host without a listener — every test server — must still keep
        // its schedule, or one such run would end the watching for good.
        let directory = tempfile::tempdir().unwrap();
        write_pair(directory.path());
        let context = watching(directory.path()).await;

        JobRunnable::handle(
            &TlsFilesJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &serde_json::json!({}),
        )
        .await
        .unwrap();

        assert_eq!(
            context
                .queue()
                .peek::<_, TlsFilesTask>(TLS_FILES_PARTITION, 10)
                .await
                .unwrap()
                .len(),
            1,
        );
    }

    #[tokio::test]
    async fn a_run_somebody_asked_for_does_not_re_arm_the_schedule() {
        let directory = tempfile::tempdir().unwrap();
        let context = watching(directory.path()).await;

        Job::handle(
            &TlsFilesJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &TlsFilesTask { forced: true },
        )
        .await
        .unwrap();

        assert!(
            context
                .queue()
                .peek::<_, TlsFilesTask>(TLS_FILES_PARTITION, 10)
                .await
                .unwrap()
                .is_empty(),
        );
    }

    #[tokio::test]
    async fn a_renewal_written_to_disk_is_picked_up_by_one_run() {
        let directory = tempfile::tempdir().unwrap();
        let first = write_pair(directory.path());
        let context = watching(directory.path()).await;
        let config = context.config();
        let tls = &config.web.public.tls;

        let loaded = files::load(
            tls.cert_file.as_ref().unwrap(),
            tls.key_file.as_ref().unwrap(),
        )
        .unwrap();
        let resolver =
            crate::pki::HotSwapCertResolver::new(Some(std::sync::Arc::clone(&loaded.certified)));
        files::publish(files::FilesCertificate::new(
            tls.cert_file.clone().unwrap(),
            tls.key_file.clone().unwrap(),
            std::sync::Arc::clone(&resolver),
            Some(loaded),
            None,
        ));

        let second = write_pair(directory.path());
        assert_ne!(first, second);

        Job::handle(
            &TlsFilesJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &TlsFilesTask { forced: true },
        )
        .await
        .unwrap();

        assert_eq!(
            resolver
                .current()
                .unwrap()
                .end_entity_cert()
                .unwrap()
                .as_ref(),
            second,
            "a renewal on disk must reach the listener without a restart",
        );
    }
}
