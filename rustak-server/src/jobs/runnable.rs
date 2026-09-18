//! Type-erasing a [`Job`] so the host can dispatch to it, and the registry it
//! is found through.
//!
//! Lifted from automate. [`Job`] is generic over its payload, which is what
//! makes a handler pleasant to write and impossible to put in a map; this is
//! the object-safe view of one, deserialising the raw JSON payload before
//! delegating. Every [`Job`] gets the blanket implementation, so writing a job
//! means implementing [`Job`] and calling [`register_job!`](crate::register_job).

use chrono::TimeDelta;
use rustak_core::prelude::*;

use super::{Job, JobContext};
use crate::services::AppContext;

/// A [`Job`] the host can hold without knowing its payload type.
///
/// The concrete services are [`AppContext`] rather than a type parameter,
/// because a trait object cannot be generic and the registry has to hold every
/// job in one map. Handlers themselves stay generic over
/// [`Services`](crate::services::Services), so they can still be unit-tested
/// against something else.
#[async_trait::async_trait]
pub trait JobRunnable: Send + Sync {
    /// The queue partition this job consumes, which is also its routing key.
    fn partition(&self) -> &'static str;

    /// Whether the enqueuing trace is this job's parent, or merely linked.
    fn propagate_parent(&self) -> bool;

    /// How long a dequeued message stays reserved while this job runs.
    fn timeout(&self) -> TimeDelta;

    /// Runs the underlying [`Job::setup`].
    ///
    /// # Errors
    ///
    /// Whatever the job's own wiring reports.
    async fn setup(&self, services: AppContext) -> Result<(), Error>;

    /// Deserialises the payload and runs the underlying [`Job::handle`].
    ///
    /// # Errors
    ///
    /// A [`Kind::User`](human_errors::Kind::User) error when the payload is not
    /// what the registered handler expects — which means a message outlived a
    /// change to its own payload type — and otherwise whatever the job reports.
    async fn handle(
        &self,
        ctx: JobContext<AppContext>,
        payload: &serde_json::Value,
    ) -> Result<(), Error>;
}

#[async_trait::async_trait]
impl<J> JobRunnable for J
where
    J: Job + Send + Sync + 'static,
{
    fn partition(&self) -> &'static str {
        <J as Job>::partition()
    }

    fn propagate_parent(&self) -> bool {
        <J as Job>::propagate_parent()
    }

    fn timeout(&self) -> TimeDelta {
        Job::timeout(self)
    }

    async fn setup(&self, services: AppContext) -> Result<(), Error> {
        Job::setup(self, services).await
    }

    async fn handle(
        &self,
        ctx: JobContext<AppContext>,
        payload: &serde_json::Value,
    ) -> Result<(), Error> {
        let job = <J::JobType as Deserialize>::deserialize(payload).wrap_user_err(
            "A queued job could not be read by the handler registered for it.",
            &[
                "This usually means a message outlived a change to the job it belongs to.",
                "Please report this issue to the development team via GitHub.",
            ],
        )?;

        Job::handle(self, ctx, &job).await
    }
}

/// One job's entry in the registry, collected at link time by [`inventory`].
pub struct JobRegistration(&'static dyn JobRunnable);

impl JobRegistration {
    /// Records a job. Called by [`register_job!`](crate::register_job), not
    /// directly.
    pub const fn new<T: JobRunnable>(job: &'static T) -> Self {
        Self(job)
    }

    /// The registered handler.
    pub fn handler(&self) -> &'static dyn JobRunnable {
        self.0
    }
}

inventory::collect!(JobRegistration);

/// Registers a [`Job`] so the host picks it up: `register_job!(AuditPruneJob);`.
///
/// The argument is a value of the job's unit struct. Registration happens at
/// link time, so a job is registered by existing in the binary — there is no
/// list to keep in step, and no way for a job to be written and then silently
/// never run.
#[macro_export]
macro_rules! register_job {
    ($job:expr) => {
        inventory::submit! { $crate::jobs::JobRegistration::new(&$job) }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{prelude::*, services::AppContext};

    #[derive(Serialize, Deserialize)]
    struct Payload {
        key: String,
        value: String,
    }

    struct ErasedJob;

    impl Job for ErasedJob {
        type JobType = Payload;

        fn partition() -> &'static str {
            "test/erased"
        }

        fn timeout(&self) -> TimeDelta {
            TimeDelta::seconds(17)
        }

        fn propagate_parent() -> bool {
            false
        }

        async fn setup(
            &self,
            services: impl Services + Send + Sync + 'static,
        ) -> Result<(), Error> {
            services
                .kv()
                .set("test/erased", "setup", "done".to_string())
                .await
        }

        async fn handle(
            &self,
            ctx: JobContext<impl Services + Send + Sync + 'static>,
            job: &Self::JobType,
        ) -> Result<(), Error> {
            ctx.services()
                .kv()
                .set("test/erased", job.key.clone(), job.value.clone())
                .await
        }
    }

    #[tokio::test]
    async fn the_erased_view_deserialises_the_payload_and_dispatches() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        JobRunnable::handle(
            &ErasedJob,
            JobContext::new(context.clone(), chrono::Utc::now(), None, None),
            &serde_json::json!({ "key": "k1", "value": "v1" }),
        )
        .await
        .unwrap();

        let stored: Option<String> = context.kv().get("test/erased", "k1").await.unwrap();
        assert_eq!(stored.as_deref(), Some("v1"));
    }

    #[tokio::test]
    async fn a_payload_the_handler_cannot_read_is_a_user_error_naming_the_problem() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        let refused = JobRunnable::handle(
            &ErasedJob,
            JobContext::new(context, chrono::Utc::now(), None, None),
            &serde_json::json!({ "unexpected": true }),
        )
        .await
        .unwrap_err();

        assert!(refused.is(human_errors::Kind::User));
        assert!(refused.to_string().contains("could not be read"));
    }

    #[tokio::test]
    async fn the_erased_view_runs_the_job_s_own_setup() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        JobRunnable::setup(&ErasedJob, context.clone())
            .await
            .unwrap();

        let stored: Option<String> = context.kv().get("test/erased", "setup").await.unwrap();
        assert_eq!(stored.as_deref(), Some("done"));
    }

    #[test]
    fn the_erased_view_reports_what_the_job_declared() {
        assert_eq!(JobRunnable::partition(&ErasedJob), "test/erased");
        assert_eq!(JobRunnable::timeout(&ErasedJob), TimeDelta::seconds(17));
        assert!(!JobRunnable::propagate_parent(&ErasedJob));
    }
}
