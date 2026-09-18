//! What a background job is, and what it is given when it runs.
//!
//! Lifted from automate's `job.rs`, with the tenant removed: a rustak
//! installation is one installation, so a job's context is the whole server's
//! [`Services`] rather than one account's slice of them.

use std::borrow::Cow;

use chrono::{DateTime, TimeDelta, Utc};
use rustak_core::prelude::*;

use crate::{db::Queue, services::Services};

/// How long a job may run before its message becomes visible again.
///
/// Generous, because the penalty for being wrong in this direction is a job
/// that is retried while the first attempt is still running. A job that knows
/// better overrides [`Job::timeout`].
pub const DEFAULT_JOB_TIMEOUT: TimeDelta = TimeDelta::minutes(5);

/// What a running job knows about the message that started it.
///
/// Beyond the [`Services`], the useful part is
/// [`scheduled_at`](JobContext::scheduled_at): for a job enqueued by a webhook
/// that is when the request arrived, which is the point a time-sensitive
/// signature should be checked against, so that a retry ten minutes later still
/// validates.
pub struct JobContext<S>
where
    S: Services + Send + Sync + 'static,
{
    services: S,
    scheduled_at: DateTime<Utc>,
    attempts: u32,
    key: Option<String>,
    traceparent: Option<String>,
    tracestate: Option<String>,
}

impl<S> JobContext<S>
where
    S: Services + Send + Sync + 'static,
{
    /// Builds a context for a message. The job host does this; a test doing it
    /// directly is running a handler without a queue behind it.
    pub fn new(
        services: S,
        scheduled_at: DateTime<Utc>,
        traceparent: Option<String>,
        tracestate: Option<String>,
    ) -> Self {
        Self {
            services,
            scheduled_at,
            attempts: 1,
            key: None,
            traceparent,
            tracestate,
        }
    }

    /// Attaches the idempotency key the message was enqueued with.
    ///
    /// Only the job host sets this, and only when the *caller* chose the key: a
    /// generated uuid identifies the row but says nothing about what the message
    /// is, and a recurring job that re-enqueues itself under one would lose the
    /// identity it was given.
    #[must_use]
    pub fn with_key(mut self, key: Option<String>) -> Self {
        self.key = key;
        self
    }

    /// Records which attempt this is, counting from one.
    #[must_use]
    pub fn with_attempts(mut self, attempts: u32) -> Self {
        self.attempts = attempts;
        self
    }

    /// The services this job runs against.
    pub fn services(&self) -> &S {
        &self.services
    }

    /// Consumes the context, handing back the services, for a handler that
    /// needs to pass owned services to a helper.
    pub fn into_services(self) -> S {
        self.services
    }

    /// The idempotency key this message was enqueued under, where the caller
    /// chose one.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// How many times this message has been handed to a handler, including now.
    ///
    /// A job that gives up rather than retrying for ever reads this.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// When the message was enqueued — for a webhook, when the request arrived.
    pub fn scheduled_at(&self) -> DateTime<Utc> {
        self.scheduled_at
    }

    /// The W3C `traceparent` the message carried, if any.
    pub fn traceparent(&self) -> Option<&str> {
        self.traceparent.as_deref()
    }

    /// The W3C `tracestate` the message carried, if any.
    pub fn tracestate(&self) -> Option<&str> {
        self.tracestate.as_deref()
    }
}

/// Work that runs off the queue rather than inside a request.
///
/// A job owns a queue partition, and the partition is the routing key: the job
/// host looks a message's partition up in the registry built from
/// [`register_job!`](crate::register_job) and hands it to the matching handler.
/// Implementations are unit structs, because the handler is shared by every
/// message on the partition and holds no per-message state.
pub trait Job {
    /// The payload this job's messages carry.
    type JobType: Serialize + DeserializeOwned + Send + 'static;

    /// The queue partition this job owns. Must be unique across the binary;
    /// the job host refuses to start if two jobs claim one.
    fn partition() -> &'static str;

    /// Queues a message for this job.
    ///
    /// # Errors
    ///
    /// As [`Queue::enqueue`].
    fn dispatch(
        job: Self::JobType,
        idempotency_key: Option<Cow<'static, str>>,
        services: &impl Services,
    ) -> impl std::future::Future<Output = Result<(), Error>> + Send {
        Self::dispatch_in(job, idempotency_key, None, services)
    }

    /// Queues a message for this job, to become due after `delay`.
    ///
    /// # Errors
    ///
    /// As [`Queue::enqueue`].
    fn dispatch_delayed(
        job: Self::JobType,
        idempotency_key: Option<Cow<'static, str>>,
        delay: TimeDelta,
        services: &impl Services,
    ) -> impl std::future::Future<Output = Result<(), Error>> + Send {
        Self::dispatch_in(job, idempotency_key, Some(delay), services)
    }

    /// The enqueue both dispatch forms share.
    ///
    /// Written as a function returning a future rather than as an `async fn`
    /// because an `async fn` in a trait promises nothing about `Send`, and a
    /// recurring job re-arming itself from inside [`setup`](Job::setup) — whose
    /// future *is* required to be `Send` — could then not call it.
    ///
    /// # Errors
    ///
    /// As [`Queue::enqueue`].
    fn dispatch_in(
        job: Self::JobType,
        idempotency_key: Option<Cow<'static, str>>,
        delay: Option<TimeDelta>,
        services: &impl Services,
    ) -> impl std::future::Future<Output = Result<(), Error>> + Send {
        let span = info_span!(
            "job.dispatch",
            otel.kind = ?OpenTelemetrySpanKind::Producer,
            job.name = Self::partition(),
            job.delay = delay.unwrap_or_default().num_milliseconds(),
        );

        // The queue is taken here and owned by the block, rather than reached
        // through a `Partition` handle, because a `Partition` carries a
        // `PhantomData<T>` and so would demand `Sync` of every job payload for
        // the sake of a name this function already has in hand.
        let queue = services.queue();

        async move {
            queue
                .enqueue(Self::partition(), job, idempotency_key, delay)
                .await?;

            Ok(())
        }
        .instrument(span)
    }

    /// Whether the enqueuing trace is this job's parent span, or merely linked
    /// to it.
    ///
    /// A job dispatched by a request is that request's continuation, so the
    /// default is to carry the parent through. A recurring job that re-arms
    /// itself should say `false`, or every run for the lifetime of the
    /// installation becomes one unbounded trace.
    fn propagate_parent() -> bool {
        true
    }

    /// How long this job may run before its message is offered to somebody
    /// else. Doubles as the delay before a failed message is retried.
    fn timeout(&self) -> TimeDelta {
        DEFAULT_JOB_TIMEOUT
    }

    /// One-time wiring, run for every registered job as the host starts.
    ///
    /// This is where a recurring job arms itself. Most jobs do nothing here and
    /// take the default.
    ///
    /// # Errors
    ///
    /// Whatever the wiring reports. The job host treats a failure as fatal:
    /// a server whose housekeeping never armed is one whose write-ahead log
    /// grows without bound.
    fn setup(
        &self,
        services: impl Services + Send + Sync + 'static,
    ) -> impl std::future::Future<Output = Result<(), Error>> + Send {
        async move {
            let _ = services;
            Ok(())
        }
    }

    /// Does the work.
    ///
    /// # Errors
    ///
    /// Whatever the work reports. A failure leaves the message on the queue to
    /// be retried once the reservation lapses; see
    /// [`JobHost`](super::JobHost) for the backoff that applies.
    fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> impl std::future::Future<Output = Result<(), Error>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::services::AppContext;

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct Payload {
        value: String,
    }

    struct Dispatchable;

    impl Job for Dispatchable {
        type JobType = Payload;

        fn partition() -> &'static str {
            "test/dispatch"
        }

        async fn handle(
            &self,
            _ctx: JobContext<impl Services + Send + Sync + 'static>,
            _job: &Self::JobType,
        ) -> Result<(), Error> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn dispatching_puts_a_message_on_the_job_s_own_partition() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        Dispatchable::dispatch(
            Payload {
                value: "now".to_string(),
            },
            Some("chosen".into()),
            &context,
        )
        .await
        .unwrap();

        let queued = context
            .queue()
            .peek::<_, Payload>("test/dispatch", 10)
            .await
            .unwrap();

        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].payload.value, "now");
        assert_eq!(queued[0].idempotency_key.as_deref(), Some("chosen"));
    }

    #[tokio::test]
    async fn a_delayed_dispatch_is_not_due_yet() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        Dispatchable::dispatch_delayed(
            Payload {
                value: "later".to_string(),
            },
            None,
            TimeDelta::hours(1),
            &context,
        )
        .await
        .unwrap();

        assert!(
            context
                .queue()
                .try_dequeue_any(TimeDelta::seconds(30))
                .await
                .unwrap()
                .is_none(),
            "a message delayed by an hour should not be handed out now",
        );
    }

    #[tokio::test]
    async fn a_context_carries_what_the_message_was_enqueued_with() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let scheduled_at = Utc::now();

        let ctx = JobContext::new(
            context,
            scheduled_at,
            Some("00-trace-span-01".to_string()),
            None,
        )
        .with_key(Some("chosen".to_string()))
        .with_attempts(3);

        assert_eq!(ctx.scheduled_at(), scheduled_at);
        assert_eq!(ctx.key(), Some("chosen"));
        assert_eq!(ctx.attempts(), 3);
        assert_eq!(ctx.traceparent(), Some("00-trace-span-01"));
        assert_eq!(ctx.tracestate(), None);
    }

    #[test]
    fn the_default_timeout_is_the_documented_one() {
        assert_eq!(Dispatchable.timeout(), DEFAULT_JOB_TIMEOUT);
        assert!(Dispatchable::propagate_parent());
    }
}
