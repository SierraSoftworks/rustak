//! The job queue's contract: the messages and the trait.
//!
//! The SQLite implementation is in [`super::queue_sqlite`], so that this file
//! stays readable as the description of what a queue promises.
//!
//! Lifted from automate's `Queue` with its `tenant` column removed and an
//! `attempts` counter added, which the job host uses to back a failing message
//! off rather than spinning on it.

use std::borrow::Cow;

use rustak_core::prelude::*;

use super::Partition;

/// A message a consumer currently holds a reservation on.
pub struct QueueMessage<T> {
    /// The row key: the caller's idempotency key, or a generated uuid.
    pub key: String,
    /// The partition it came from, which names the handler that should run it.
    pub partition: String,
    /// Proof that this consumer holds the message; completing or extending the
    /// reservation requires it, so a message whose visibility timeout lapsed
    /// cannot be completed twice.
    pub reservation_id: String,
    pub payload: T,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
    /// How many times this message has been handed out, including this time.
    pub attempts: u32,
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
    /// The idempotency key the message was enqueued with, if the caller gave
    /// one. `None` means the key was generated, so it carries no meaning beyond
    /// identifying the row.
    pub idempotency_key: Option<String>,
}

impl<T> OpenTelemetryPropagationExtractor for QueueMessage<T> {
    fn get(&self, key: &str) -> Option<&str> {
        match key {
            "traceparent" => self.traceparent.as_deref(),
            "tracestate" => self.tracestate.as_deref(),
            _ => None,
        }
    }

    fn keys(&self) -> Vec<&str> {
        match (&self.traceparent, &self.tracestate) {
            (Some(_), Some(_)) => vec!["traceparent", "tracestate"],
            (Some(_), None) => vec!["traceparent"],
            (None, Some(_)) => vec!["tracestate"],
            (None, None) => vec![],
        }
    }
}

/// A message read without reserving it, for the admin view of a queue.
pub struct PeekedMessage<T> {
    pub key: String,
    pub payload: T,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
    pub hidden_until: chrono::DateTime<chrono::Utc>,
    pub reserved_by: Option<String>,
    pub attempts: u32,
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
    pub idempotency_key: Option<String>,
}

/// A partitioned work queue with visibility timeouts.
#[async_trait::async_trait]
pub trait Queue {
    /// Adds a message, or reschedules the one already under `idempotency_key`.
    async fn enqueue<P: Into<Cow<'static, str>> + Send, T: Serialize + Send + 'static>(
        &self,
        partition: P,
        job: T,
        idempotency_key: Option<Cow<'static, str>>,
        delay: Option<chrono::Duration>,
    ) -> Result<QueuedMessage, Error>;

    /// Reserves the next due message in one partition, waiting for one to
    /// become due.
    async fn dequeue<P: Into<Cow<'static, str>> + Send, T: DeserializeOwned + Send + 'static>(
        &self,
        partition: P,
        reserve_for: chrono::Duration,
    ) -> Result<QueueMessage<T>, Error>;

    /// Reserves the next due message from any partition, in scheduling order.
    ///
    /// This is what the single job consumer uses;
    /// [`QueueMessage::partition`] tells it which handler to run.
    async fn dequeue_any(
        &self,
        reserve_for: chrono::Duration,
    ) -> Result<QueueMessage<serde_json::Value>, Error>;

    /// Reserves the next due message from any partition, or returns `None`
    /// immediately when nothing is due.
    ///
    /// The polling form exists so a consumer can interleave the queue with a
    /// shutdown signal rather than sitting inside a sleep it cannot cancel.
    async fn try_dequeue_any(
        &self,
        reserve_for: chrono::Duration,
    ) -> Result<Option<QueueMessage<serde_json::Value>>, Error>;

    /// Deletes a message this consumer holds. A lapsed reservation deletes
    /// nothing, because somebody else may now hold the message.
    async fn complete<P: Into<Cow<'static, str>> + Send, T: Send + 'static>(
        &self,
        partition: P,
        msg: QueueMessage<T>,
    ) -> Result<(), Error>;

    /// Moves a held message's visibility timeout to `reserve_for` from now.
    ///
    /// Lets the consumer narrow the generous dequeue reservation down to each
    /// job's own timeout, which doubles as the backoff before a failed message
    /// is retried.
    async fn reserve<
        P: Into<Cow<'static, str>> + Send,
        K: Into<Cow<'static, str>> + Send,
        R: Into<Cow<'static, str>> + Send,
    >(
        &self,
        partition: P,
        key: K,
        reservation_id: R,
        reserve_for: chrono::Duration,
    ) -> Result<(), Error>;

    /// Reads messages without reserving them, oldest first.
    async fn peek<P: Into<Cow<'static, str>> + Send, T: DeserializeOwned + Send + 'static>(
        &self,
        partition: P,
        max_items: usize,
    ) -> Result<Vec<PeekedMessage<T>>, Error>;

    /// Reads the one message under `key` without reserving it, whatever else
    /// the partition holds and however much of it.
    ///
    /// `key` is the row's own: the idempotency key a caller enqueued under. This
    /// is how a recurring job finds its scheduled message, which
    /// [`peek`](Queue::peek)'s oldest-first page may not reach.
    async fn peek_key<P: Into<Cow<'static, str>> + Send, T: DeserializeOwned + Send + 'static>(
        &self,
        partition: P,
        key: &str,
    ) -> Result<Option<PeekedMessage<T>>, Error>;

    /// Removes a message by key whether or not it is reserved, for cancelling
    /// queued work by hand.
    async fn purge<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send>(
        &self,
        partition: P,
        key: K,
    ) -> Result<(), Error>;

    /// Every partition holding at least one message.
    async fn partitions(&self) -> Result<Vec<String>, Error>;

    /// A handle bound to one partition and one payload type.
    fn partition<T: Serialize + DeserializeOwned + Send + 'static>(
        &self,
        name: impl ToString,
    ) -> Partition<Self, T>
    where
        Self: Sized + Clone,
    {
        Partition::new(self.clone(), name.to_string())
    }
}

/// What [`Queue::enqueue`] wrote, so the caller can cancel or await it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedMessage {
    pub partition: String,
    pub key: String,
    /// `false` when an existing message under the same idempotency key was
    /// rescheduled rather than a new one added.
    pub created: bool,
}

/// How long a poll waits before looking at the queue again.
///
/// A second is short enough that a job scheduled now starts promptly and long
/// enough that an idle installation is not running a query a second per worker.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
