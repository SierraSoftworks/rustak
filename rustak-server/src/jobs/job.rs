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

    /// Arms a recurring job's one scheduled message, leaving a run that is
    /// already armed where it is.
    ///
    /// This is what a recurring job calls from [`setup`](Job::setup), and the
    /// reason it is not a plain [`dispatch_delayed`](Job::dispatch_delayed):
    /// enqueueing under a key that is already queued *reschedules* that message,
    /// so arming a full interval out at every start-up means a server restarted
    /// more often than the interval never runs the job at all. The message is
    /// kept under the partition's own name, which is the key a recurring job's
    /// [`handle`](Job::handle) re-arms itself under.
    ///
    /// - Nothing armed: the first run is `first_delay` from now, or `interval`
    ///   where that is sooner. Pass `interval` for a job with nothing to do
    ///   sooner than that.
    /// - Armed and due within `interval`: left alone, overdue included. A
    ///   restart costs the schedule nothing.
    /// - Reserved, however far out: left alone. It is a run that was under way
    ///   when the last process stopped, or one backing off after a failure, and
    ///   arming over it would clear the reservation and the attempts that pace
    ///   its retries.
    /// - Armed further out than `interval`: the interval was shortened since,
    ///   and whoever shortened it is waiting, so it is armed afresh.
    ///
    /// The scheduled message is looked up by its key, so nothing queued beside
    /// it can hide it, and its payload is read as untyped JSON, so that a
    /// message left by a release whose payload had another shape cannot fail
    /// start-up.
    ///
    /// # Errors
    ///
    /// As [`Queue::peek_key`] and [`Queue::enqueue`].
    fn arm_recurring(
        job: Self::JobType,
        interval: TimeDelta,
        first_delay: TimeDelta,
        services: &(impl Services + Sync),
    ) -> impl std::future::Future<Output = Result<(), Error>> + Send {
        async move {
            let scheduled = services
                .queue()
                .peek_key::<_, serde_json::Value>(Self::partition(), Self::partition())
                .await?;

            let latest = Utc::now() + interval;
            let armed = scheduled.is_some_and(|message| {
                message.reserved_by.is_some() || message.hidden_until <= latest
            });

            if armed {
                return Ok(());
            }

            Self::dispatch_delayed(
                job,
                Some(Self::partition().into()),
                first_delay.min(interval),
                services,
            )
            .await
        }
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

    /// When the scheduled message on the test partition becomes due.
    async fn scheduled_for(context: &AppContext) -> DateTime<Utc> {
        let queued = context
            .queue()
            .peek::<_, Payload>("test/dispatch", 10)
            .await
            .unwrap();

        let scheduled: Vec<_> = queued
            .iter()
            .filter(|message| message.idempotency_key.as_deref() == Some("test/dispatch"))
            .collect();

        assert_eq!(scheduled.len(), 1, "there is only ever one scheduled run");
        scheduled[0].hidden_until
    }

    fn scheduled() -> Payload {
        Payload {
            value: "scheduled".to_string(),
        }
    }

    #[tokio::test]
    async fn a_schedule_with_nothing_armed_starts_after_the_first_delay() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        Dispatchable::arm_recurring(
            scheduled(),
            TimeDelta::hours(1),
            TimeDelta::minutes(5),
            &context,
        )
        .await
        .unwrap();

        let due = scheduled_for(&context).await;
        assert!(due > Utc::now(), "not at the instant of start-up");
        assert!(due <= Utc::now() + TimeDelta::minutes(5));
    }

    #[tokio::test]
    async fn the_first_delay_is_never_longer_than_the_interval() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        Dispatchable::arm_recurring(
            scheduled(),
            TimeDelta::minutes(1),
            TimeDelta::minutes(5),
            &context,
        )
        .await
        .unwrap();

        assert!(scheduled_for(&context).await <= Utc::now() + TimeDelta::minutes(1));
    }

    #[tokio::test]
    async fn arming_again_does_not_postpone_a_run_that_is_already_armed() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        Dispatchable::dispatch_delayed(
            scheduled(),
            Some("test/dispatch".into()),
            TimeDelta::minutes(40),
            &context,
        )
        .await
        .unwrap();
        let before = scheduled_for(&context).await;

        Dispatchable::arm_recurring(
            scheduled(),
            TimeDelta::hours(1),
            TimeDelta::hours(1),
            &context,
        )
        .await
        .unwrap();

        assert_eq!(scheduled_for(&context).await, before);
    }

    #[tokio::test]
    async fn arming_again_does_not_postpone_a_run_that_is_overdue() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        Dispatchable::dispatch_delayed(
            scheduled(),
            Some("test/dispatch".into()),
            TimeDelta::minutes(-5),
            &context,
        )
        .await
        .unwrap();
        let before = scheduled_for(&context).await;

        Dispatchable::arm_recurring(
            scheduled(),
            TimeDelta::hours(1),
            TimeDelta::hours(1),
            &context,
        )
        .await
        .unwrap();

        assert_eq!(scheduled_for(&context).await, before);
    }

    #[tokio::test]
    async fn a_run_that_is_reserved_is_left_to_whoever_holds_it() {
        // The TLS look is reserved for a minute and recurs every thirty
        // seconds, so a reservation can outlast the interval without the
        // interval having been shortened.
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        Dispatchable::dispatch(scheduled(), Some("test/dispatch".into()), &context)
            .await
            .unwrap();
        let held = context
            .queue()
            .dequeue::<_, Payload>("test/dispatch", TimeDelta::hours(2))
            .await
            .unwrap();

        Dispatchable::arm_recurring(
            scheduled(),
            TimeDelta::hours(1),
            TimeDelta::hours(1),
            &context,
        )
        .await
        .unwrap();

        let queued = context
            .queue()
            .peek_key::<_, Payload>("test/dispatch", "test/dispatch")
            .await
            .unwrap()
            .expect("the reserved run should still be queued");
        assert_eq!(queued.reserved_by, Some(held.reservation_id));
        assert_eq!(queued.attempts, held.attempts);
    }

    #[tokio::test]
    async fn a_shortened_interval_pulls_in_a_run_armed_under_the_old_one() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        Dispatchable::dispatch_delayed(
            scheduled(),
            Some("test/dispatch".into()),
            TimeDelta::hours(6),
            &context,
        )
        .await
        .unwrap();

        Dispatchable::arm_recurring(
            scheduled(),
            TimeDelta::hours(1),
            TimeDelta::hours(1),
            &context,
        )
        .await
        .unwrap();

        assert!(scheduled_for(&context).await <= Utc::now() + TimeDelta::hours(1));
    }

    #[tokio::test]
    async fn a_message_queued_beside_the_schedule_is_not_mistaken_for_it() {
        // The TLS reload somebody asked for shares its partition with the
        // schedule, under a key of its own.
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        Dispatchable::dispatch(
            Payload {
                value: "by hand".to_string(),
            },
            Some("test/dispatch/forced".into()),
            &context,
        )
        .await
        .unwrap();

        Dispatchable::arm_recurring(
            scheduled(),
            TimeDelta::hours(1),
            TimeDelta::hours(1),
            &context,
        )
        .await
        .unwrap();

        assert!(scheduled_for(&context).await > Utc::now());
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
