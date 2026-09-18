//! The single consumer that runs every registered job.
//!
//! Lifted from automate's `JobHost`, with two changes. The tenant loop is gone,
//! because a rustak installation is one installation. And the dequeue is
//! shutdown-aware: automate's host sits inside a blocking `dequeue_any` that
//! cannot be cancelled and is torn down by the process exiting, whereas rustak
//! stops cleanly — the database has a write-ahead log to fold back in, and a
//! host that could not be stopped would be a host holding the connection that
//! has to do it. [`Queue::try_dequeue_any`] polls instead, and every wait in
//! this file is a `select!` against the shutdown token.

use std::collections::HashMap;

use chrono::{TimeDelta, Utc};
use rustak_core::prelude::*;
use tokio::task::JoinSet;

use super::{JobContext, JobRegistration, JobRunnable};
use crate::{
    db::{Queue, QueueMessage, queue::POLL_INTERVAL},
    services::{AppContext, Services},
};

/// How long the host waits after a queue error before trying again.
///
/// The error is almost always the database being unavailable, so retrying at
/// the poll interval would fill the log with the same line a second at a time.
const ERROR_BACKOFF: std::time::Duration = std::time::Duration::from_secs(5);

/// The longest a failing message is held back before it is retried.
const MAX_BACKOFF: TimeDelta = TimeDelta::minutes(15);

/// The reservation taken when no job declares a timeout at all.
const FALLBACK_RESERVATION: TimeDelta = TimeDelta::minutes(5);

/// Runs queued work until the shutdown token is cancelled.
pub struct JobHost;

impl JobHost {
    /// Consumes the queue until `context`'s [`Shutdown`] is cancelled.
    ///
    /// Returns `Ok(())` on a clean shutdown. In-flight jobs are aborted rather
    /// than waited for: their messages were never completed, so they are
    /// retried by whichever process comes up next, and a shutdown that waits on
    /// a five-minute job is a shutdown an operator kills.
    ///
    /// Every job's [`setup`](super::Job::setup) runs before the first poll and
    /// is *not* interruptible, so a job's wiring belongs in the queue rather
    /// than in a network call: a slow setup is time a shutdown has to wait for.
    ///
    /// # Errors
    ///
    /// A [`Kind::User`](human_errors::Kind::User) error when two jobs claim the
    /// same partition, and whatever a job's [`setup`](super::Job::setup)
    /// reports. Neither can be recovered from by retrying, and both mean some
    /// housekeeping would silently never run.
    #[instrument("job.host.run", skip_all, fields(otel.kind = ?OpenTelemetrySpanKind::Consumer), err(Display))]
    pub async fn run(context: AppContext) -> Result<(), Error> {
        let registry = Self::registry()?;
        let shutdown = context.shutdown().clone();

        info!(
            jobs = registry.len(),
            "The job host has started with {} registered handler(s).",
            registry.len()
        );

        for handler in registry.values() {
            handler.setup(context.clone()).await?;
        }

        // Reserve for at least as long as the slowest job may take, so a message
        // is never offered to somebody else while it is still being worked on.
        // `process` narrows this to the handler's own timeout once the message
        // is in hand and the handler is known.
        let reserve_for = registry
            .values()
            .map(|handler| handler.timeout())
            .max()
            .unwrap_or(FALLBACK_RESERVATION);

        let root_span = Span::current();
        let queue = context.queue();

        // Spawned jobs are tracked rather than detached so that their lifetimes
        // are bounded by the host's. Dropping the set releases the `AppContext`
        // clones they hold — and with them the `Arc<Session>` clones — which is
        // what lets `main` reclaim the session and flush telemetry on the way
        // out.
        let mut tasks = JoinSet::new();

        while !shutdown.is_cancelled() {
            // Reap finished jobs so the set does not grow without bound.
            while tasks.try_join_next().is_some() {}

            let dequeued = tokio::select! {
                // Cancellation wins a tie: when both are ready we are stopping.
                // Losing the poll this way can abandon a message the reservation
                // transaction had already committed, which then waits out its
                // window before anybody sees it again — a few minutes' delay on
                // one message, once, against a shutdown that cannot be held up
                // by a database busy timeout.
                biased;
                () = shutdown.cancelled() => break,
                dequeued = queue.try_dequeue_any(reserve_for) => dequeued,
            };

            match dequeued {
                Ok(Some(item)) => {
                    let Some(&handler) = registry.get(item.partition.as_str()) else {
                        Self::drop_unhandled(&context, item).await;
                        continue;
                    };

                    tasks.spawn(Self::process(
                        handler,
                        item,
                        context.clone(),
                        root_span.clone(),
                    ));
                }
                Ok(None) => Self::wait(&context, POLL_INTERVAL).await,
                Err(err) => {
                    error!(error = %err, "Could not read the job queue: {err}");
                    context.session().record_human_error(&err);
                    Self::wait(&context, ERROR_BACKOFF).await;
                }
            }
        }

        info!("The job host is stopping; in-flight jobs will be retried.");
        tasks.shutdown().await;

        Ok(())
    }

    /// The handlers, by partition, refusing a partition claimed twice.
    fn registry() -> Result<HashMap<&'static str, &'static dyn JobRunnable>, Error> {
        let mut registry: HashMap<&'static str, &'static dyn JobRunnable> = HashMap::new();

        for registration in inventory::iter::<JobRegistration> {
            let handler = registration.handler();
            let partition = handler.partition();

            if registry.insert(partition, handler).is_some() {
                return Err(human_errors::user(
                    format!(
                        "Two job handlers are registered for the queue partition '{partition}'."
                    ),
                    &[
                        "Each job must own a partition of its own.",
                        "Please report this issue to the development team via GitHub.",
                    ],
                ));
            }
        }

        Ok(registry)
    }

    /// Sleeps, unless we are asked to stop first.
    async fn wait(context: &AppContext, how_long: std::time::Duration) {
        tokio::select! {
            () = context.shutdown().cancelled() => {}
            () = tokio::time::sleep(how_long) => {}
        }
    }

    /// Removes a message nothing is registered to run.
    ///
    /// Dropped rather than left in place: no handler is going to appear, and a
    /// message that cannot be run is a message that would be re-reserved for
    /// ever, hiding the ones that can.
    async fn drop_unhandled(context: &AppContext, item: QueueMessage<serde_json::Value>) {
        warn!(
            job.name = %item.partition,
            "No job handler is registered for the partition '{}', so the message was dropped.",
            item.partition,
        );

        let partition = item.partition.clone();
        if let Err(err) = context.queue().complete(partition, item).await {
            error!(error = %err, "Could not drop an unhandled job message: {err}");
            context.session().record_human_error(&err);
        }
    }

    /// Runs one message through its handler.
    async fn process(
        handler: &'static dyn JobRunnable,
        item: QueueMessage<serde_json::Value>,
        context: AppContext,
        root_span: Span,
    ) {
        let queue = context.queue();
        let name = handler.partition();
        let delay = Utc::now() - item.scheduled_at;

        // Narrow the generous dequeue reservation to this job's own timeout. The
        // reservation window doubles as the retry backoff, so a job that fails —
        // or a process that dies holding it — keeps the message hidden for this
        // long rather than immediately spinning on it.
        Self::hold(&context, &item, handler.timeout()).await;

        let span = info_span!(
            parent: None,
            "job.run",
            job.name = name,
            job.delay = delay.num_milliseconds(),
            job.attempts = item.attempts,
            otel.kind = ?OpenTelemetrySpanKind::Consumer,
        );
        span.follows_from(&root_span);
        Self::adopt_trace(&span, &item, handler.propagate_parent());

        let ctx = JobContext::new(
            context.clone(),
            item.scheduled_at,
            item.traceparent.clone(),
            item.tracestate.clone(),
        )
        .with_key(item.idempotency_key.clone())
        .with_attempts(item.attempts);

        match handler
            .handle(ctx, &item.payload)
            .instrument(span.clone())
            .await
        {
            Ok(()) => {
                debug!(job.name = name, "The job '{name}' finished.");

                let partition = item.partition.clone();
                if let Err(err) = queue.complete(partition, item).await {
                    error!(error = %err, "Could not mark the job '{name}' as finished: {err}");
                    context.session().record_human_error(&err);
                }
            }
            Err(err) => {
                if err.is(human_errors::Kind::System) {
                    context.session().record_human_error(&err);
                }

                // Recorded against the job's own span rather than the host's, so
                // that it is exported with the trace of the run that failed.
                record_failure(&span, &err);

                let backoff = Self::backoff(item.attempts, handler.timeout());
                Self::hold(&context, &item, backoff).await;

                error!(
                    error = %err,
                    job.name = name,
                    job.attempts = item.attempts,
                    "The job '{name}' failed and will be retried in {backoff}: {err}",
                );
            }
        }
    }

    /// Moves a held message's visibility timeout to `for_how_long` from now.
    async fn hold(
        context: &AppContext,
        item: &QueueMessage<serde_json::Value>,
        how_long: TimeDelta,
    ) {
        let held = context
            .queue()
            .reserve(
                item.partition.clone(),
                item.key.clone(),
                item.reservation_id.clone(),
                how_long,
            )
            .await;

        if let Err(err) = held {
            warn!(
                error = %err,
                job.name = %item.partition,
                "Could not set the reservation window for the job '{}': {err}",
                item.partition,
            );
            context.session().record_human_error(&err);
        }
    }

    /// Links this run to the trace that enqueued it, as the job asked.
    fn adopt_trace(span: &Span, item: &QueueMessage<serde_json::Value>, propagate_parent: bool) {
        if item.traceparent.is_none() {
            return;
        }

        let enqueued_in = get_text_map_propagator(|propagator| propagator.extract(item));

        if propagate_parent {
            if let Err(err) = span.set_parent(enqueued_in) {
                warn!(error = %err, "Could not adopt the trace a job was enqueued in: {err}");
            }
        } else {
            // A recurring job re-arms itself, so carrying the parent through
            // would make every run for the lifetime of the installation one
            // unbounded trace. A link says the same thing without that.
            span.add_link(enqueued_in.span().span_context().clone());
        }
    }

    /// How long a message that just failed is held back before it is retried.
    ///
    /// Doubles per attempt from the job's own timeout up to
    /// [`MAX_BACKOFF`]: a job failing because a remote service is down should
    /// not keep asking it every few seconds, and a job failing because of a bug
    /// should not be the reason the log is unreadable. A job that wants to be
    /// retried at once says so with a timeout in the past, which stays in the
    /// past however many times it is doubled.
    fn backoff(attempts: u32, timeout: TimeDelta) -> TimeDelta {
        // Capped before the shift so it cannot overflow the exponent; `1 << 16`
        // is already far beyond `MAX_BACKOFF` for any sane timeout.
        let doublings = attempts.saturating_sub(1).min(16);

        timeout
            .checked_mul(1_i32 << doublings)
            .map_or(MAX_BACKOFF, |backoff| backoff.min(MAX_BACKOFF))
    }
}

/// Records a job failure on `span` following the OpenTelemetry conventions, so
/// that it shows up as an exception on the exported trace rather than only in
/// the log.
fn record_failure(span: &Span, err: &Error) {
    let exception_type = if err.is(human_errors::Kind::System) {
        "SystemFailure"
    } else {
        "UserError"
    };

    span.add_event(
        "exception",
        vec![
            opentelemetry::KeyValue::new("exception.type", exception_type),
            opentelemetry::KeyValue::new("exception.message", err.to_string()),
            opentelemetry::KeyValue::new("exception.escaped", true),
        ],
    );
    span.set_status(opentelemetry::trace::Status::error(err.to_string()));
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, Instant},
    };

    use super::*;

    use crate::{
        jobs::{AUDIT_PRUNE_PARTITION, Job, WAL_CHECKPOINT_PARTITION},
        prelude::*,
    };

    #[derive(Serialize, Deserialize)]
    struct Payload {
        id: String,
        value: String,
    }

    /// Records what it was given, so a test can assert what the host surfaced.
    struct RecordingJob;

    static RECORDING: RecordingJob = RecordingJob;

    impl Job for RecordingJob {
        type JobType = Payload;

        fn partition() -> &'static str {
            "test/recording"
        }

        async fn handle(
            &self,
            ctx: JobContext<impl Services + Send + Sync + 'static>,
            job: &Self::JobType,
        ) -> Result<(), Error> {
            let kv = ctx.services().kv();

            kv.set("test/recording", job.id.clone(), job.value.clone())
                .await?;
            kv.set(
                "test/recording",
                format!("{}/key", job.id),
                ctx.key().unwrap_or("<generated>").to_string(),
            )
            .await
        }
    }

    /// Always fails, with a timeout in the past so that a failed message becomes
    /// available again at once — which is how a test asserts that the host
    /// applied the job's own window without waiting for a real one.
    struct FailingJob;

    static FAILING: FailingJob = FailingJob;
    static FAILING_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

    impl Job for FailingJob {
        type JobType = Payload;

        fn partition() -> &'static str {
            "test/failing"
        }

        fn timeout(&self) -> TimeDelta {
            TimeDelta::seconds(-1)
        }

        async fn handle(
            &self,
            _ctx: JobContext<impl Services + Send + Sync + 'static>,
            _job: &Self::JobType,
        ) -> Result<(), Error> {
            FAILING_ATTEMPTS.fetch_add(1, Ordering::SeqCst);

            Err(human_errors::user(
                "The job failed.",
                &["This failure is expected in tests."],
            ))
        }
    }

    async fn enqueued(context: &AppContext, partition: &'static str, id: &str, key: Option<&str>) {
        context
            .queue()
            .enqueue(
                partition,
                Payload {
                    id: id.to_string(),
                    value: "value".to_string(),
                },
                key.map(|key| key.to_string().into()),
                None,
            )
            .await
            .unwrap();
    }

    #[test]
    fn every_registered_job_owns_a_partition_of_its_own() {
        let mut seen = HashSet::new();

        for registration in inventory::iter::<JobRegistration> {
            let partition = registration.handler().partition();

            assert!(
                seen.insert(partition),
                "two jobs are registered for the partition '{partition}'",
            );
        }

        // The housekeeping that ships with the server must be registered, or it
        // silently never runs.
        assert!(seen.contains(AUDIT_PRUNE_PARTITION));
        assert!(seen.contains(WAL_CHECKPOINT_PARTITION));
        assert_eq!(JobHost::registry().unwrap().len(), seen.len());
    }

    #[tokio::test]
    async fn a_processed_message_runs_its_handler_and_leaves_the_queue() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        enqueued(&context, "test/recording", "k1", None).await;

        let item = context
            .queue()
            .try_dequeue_any(TimeDelta::seconds(60))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.partition, "test/recording");

        JobHost::process(&RECORDING, item, context.clone(), Span::none()).await;

        let stored: Option<String> = context.kv().get("test/recording", "k1").await.unwrap();
        assert_eq!(stored.as_deref(), Some("value"));

        assert!(
            context
                .queue()
                .try_dequeue_any(TimeDelta::seconds(60))
                .await
                .unwrap()
                .is_none(),
            "a completed message should have been removed from the queue",
        );
    }

    /// The host surfaces the key a message was enqueued under, but only when the
    /// enqueuer chose it: a generated uuid is a misleading identity for a
    /// recurring job to re-enqueue itself under.
    #[tokio::test]
    async fn only_a_chosen_idempotency_key_is_surfaced_to_the_handler() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        enqueued(&context, "test/recording", "k2", Some("chosen")).await;
        enqueued(&context, "test/recording", "k3", None).await;

        for _ in 0..2 {
            let item = context
                .queue()
                .try_dequeue_any(TimeDelta::seconds(60))
                .await
                .unwrap()
                .unwrap();
            JobHost::process(&RECORDING, item, context.clone(), Span::none()).await;
        }

        let chosen: Option<String> = context.kv().get("test/recording", "k2/key").await.unwrap();
        assert_eq!(chosen.as_deref(), Some("chosen"));

        let generated: Option<String> = context.kv().get("test/recording", "k3/key").await.unwrap();
        assert_eq!(generated.as_deref(), Some("<generated>"));
    }

    #[tokio::test]
    async fn a_failed_message_stays_on_the_queue_under_the_job_s_own_window() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        enqueued(&context, "test/failing", "k4", None).await;

        // Dequeued with a generous reservation: if the host did not narrow it to
        // the job's own timeout, the failed message would stay hidden for a
        // minute instead of becoming retriable at once.
        let item = context
            .queue()
            .try_dequeue_any(TimeDelta::seconds(60))
            .await
            .unwrap()
            .unwrap();

        JobHost::process(&FAILING, item, context.clone(), Span::none()).await;
        assert!(FAILING_ATTEMPTS.load(Ordering::SeqCst) > 0);

        let retried = context
            .queue()
            .try_dequeue_any(TimeDelta::seconds(60))
            .await
            .unwrap()
            .expect("a failed job whose window has elapsed should be retriable at once");

        assert_eq!(retried.partition, "test/failing");
        assert_eq!(retried.attempts, 2, "the retry should be counted");
    }

    #[tokio::test]
    async fn a_message_nothing_is_registered_for_is_dropped() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        enqueued(&context, "test/unregistered", "k5", None).await;

        let item = context
            .queue()
            .try_dequeue_any(TimeDelta::seconds(60))
            .await
            .unwrap()
            .unwrap();
        JobHost::drop_unhandled(&context, item).await;

        assert!(
            context
                .queue()
                .peek::<_, Payload>("test/unregistered", 10)
                .await
                .unwrap()
                .is_empty(),
            "a message no handler can run should not be left to be re-reserved for ever",
        );
    }

    #[test]
    fn the_backoff_doubles_per_attempt_up_to_its_cap() {
        let timeout = TimeDelta::minutes(1);

        assert_eq!(JobHost::backoff(1, timeout), TimeDelta::minutes(1));
        assert_eq!(JobHost::backoff(2, timeout), TimeDelta::minutes(2));
        assert_eq!(JobHost::backoff(4, timeout), TimeDelta::minutes(8));
        assert_eq!(JobHost::backoff(10, timeout), MAX_BACKOFF);
        assert_eq!(JobHost::backoff(u32::MAX, timeout), MAX_BACKOFF);
    }

    #[test]
    fn a_job_asking_to_be_retried_at_once_still_is() {
        // A window in the past stays in the past however often it is doubled,
        // which is what keeps the failing-job test above honest.
        assert!(JobHost::backoff(5, TimeDelta::seconds(-1)) < TimeDelta::zero());
    }

    /// The property the shutdown-aware loop exists for: the host has to be back
    /// before the rest of shutdown — the final `TRUNCATE` checkpoint in
    /// particular — can run.
    #[tokio::test]
    async fn the_host_returns_within_a_second_of_cancellation() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let shutdown = context.shutdown().clone();

        let host = tokio::spawn(JobHost::run(context));

        // Long enough for the registered housekeeping to be set up and run, so
        // that cancellation arrives while the host is idling in its poll.
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!host.is_finished(), "the host should still be consuming");

        let cancelled_at = Instant::now();
        shutdown.cancel();

        tokio::time::timeout(Duration::from_secs(1), host)
            .await
            .expect("the job host did not return within a second of cancellation")
            .expect("the job host panicked")
            .expect("the job host reported an error on a clean shutdown");

        assert!(cancelled_at.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn the_host_sets_every_registered_job_up_before_it_consumes() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let shutdown = context.shutdown().clone();

        let host = tokio::spawn(JobHost::run(context.clone()));
        tokio::time::sleep(Duration::from_millis(250)).await;
        shutdown.cancel();
        host.await.unwrap().unwrap();

        // The checkpoint job arms itself an interval out, so it is still there;
        // the prune arms itself immediately, runs, and re-arms for tomorrow.
        let armed = context.queue().partitions().await.unwrap();
        assert!(armed.contains(&WAL_CHECKPOINT_PARTITION.to_string()));
        assert!(armed.contains(&AUDIT_PRUNE_PARTITION.to_string()));
    }

    #[tokio::test]
    async fn a_host_whose_context_is_already_cancelled_returns_at_once() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        context.shutdown().cancel();

        let started = Instant::now();
        JobHost::run(context).await.unwrap();

        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn the_registry_is_keyed_by_partition() {
        let registry = JobHost::registry().unwrap();
        let handler = registry
            .get(AUDIT_PRUNE_PARTITION)
            .expect("the audit prune should be registered");

        assert_eq!(handler.partition(), AUDIT_PRUNE_PARTITION);
        assert!(
            !handler.propagate_parent(),
            "a job that re-arms itself must not chain its traces",
        );
    }
}
