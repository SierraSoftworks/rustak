//! The SQLite implementation of [`Queue`].
//!
//! Reservation is a single `BEGIN IMMEDIATE` transaction that reads the next due
//! row and hides it in the same breath, so two consumers cannot be handed the
//! same message. Everything else follows from that: a consumer that dies simply
//! stops extending its reservation and the message becomes due again.

use std::{borrow::Cow, collections::HashMap};

use rusqlite::OptionalExtension as _;
use rustak_core::prelude::*;

use super::{
    Database,
    queue::{POLL_INTERVAL, PeekedMessage, Queue, QueueMessage, QueuedMessage},
    row::{Timestamp, to_json, ts},
};

/// The columns a reservation reads, in the order the row mapper expects.
const RESERVED_COLUMNS: &str =
    "partition, key, payload, scheduled_at, traceparent, tracestate, idempotency_key, attempts";

#[async_trait::async_trait]
impl Queue for Database {
    #[instrument("db.queue.enqueue", skip_all, fields(otel.kind = ?OpenTelemetrySpanKind::Producer, job.kind = std::any::type_name::<T>()), err(Display))]
    async fn enqueue<P: Into<Cow<'static, str>> + Send, T: Serialize + Send + 'static>(
        &self,
        partition: P,
        job: T,
        idempotency_key: Option<Cow<'static, str>>,
        delay: Option<chrono::Duration>,
    ) -> Result<QueuedMessage, Error> {
        let mut trace_headers = HashMap::new();
        get_text_map_propagator(|propagator| {
            propagator.inject_context(&Span::current().context(), &mut trace_headers);
        });

        let partition = partition.into().into_owned();
        // The row key is the caller's idempotency key when they gave one and a
        // uuid otherwise; `idempotency_key` records which, so a handler can tell
        // an identity it was given from one we invented.
        let key = idempotency_key
            .as_deref()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let idempotency_key = idempotency_key.map(|key| key.into_owned());

        let now = chrono::Utc::now();
        let hidden_until = Timestamp::from(delay.map_or(now, |delay| now + delay));
        let (bound_partition, bound_key) = (partition.clone(), key.clone());

        let created = self
            .write(move |tx| {
                // Asked before the upsert rather than inferred from `changes()`,
                // which reports one row for an insert and for an update alike.
                let existed: bool = tx.query_one(
                    "SELECT EXISTS (SELECT 1 FROM queues WHERE partition = ?1 AND key = ?2)",
                    (&bound_partition, &bound_key),
                    |row| row.get::<_, i64>(0),
                )? == 1;

                tx.execute(
                    "INSERT INTO queues \
                       (partition, key, payload, scheduled_at, hidden_until, \
                        traceparent, tracestate, idempotency_key, attempts) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0) \
                     ON CONFLICT (partition, key) DO UPDATE SET \
                       payload = excluded.payload, scheduled_at = excluded.scheduled_at, \
                       hidden_until = excluded.hidden_until, reserved_by = NULL, \
                       traceparent = excluded.traceparent, tracestate = excluded.tracestate, \
                       idempotency_key = excluded.idempotency_key, attempts = 0",
                    rusqlite::params![
                        bound_partition,
                        bound_key,
                        to_json(&job)?,
                        Timestamp::from(now),
                        hidden_until,
                        trace_headers.get("traceparent"),
                        trace_headers.get("tracestate"),
                        idempotency_key,
                    ],
                )?;

                Ok(!existed)
            })
            .await?;

        Ok(QueuedMessage {
            partition,
            key,
            created,
        })
    }

    #[instrument("db.queue.dequeue", skip_all, fields(otel.kind = ?OpenTelemetrySpanKind::Consumer, job.kind = std::any::type_name::<T>()), err(Display))]
    async fn dequeue<P: Into<Cow<'static, str>> + Send, T: DeserializeOwned + Send + 'static>(
        &self,
        partition: P,
        reserve_for: chrono::Duration,
    ) -> Result<QueueMessage<T>, Error> {
        let partition = partition.into().into_owned();

        loop {
            if let Some(message) = reserve_next(self, Some(partition.clone()), reserve_for).await? {
                return typed(message);
            }

            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    #[instrument("db.queue.dequeue_any", skip_all, fields(otel.kind = ?OpenTelemetrySpanKind::Consumer), err(Display))]
    async fn dequeue_any(
        &self,
        reserve_for: chrono::Duration,
    ) -> Result<QueueMessage<serde_json::Value>, Error> {
        loop {
            if let Some(message) = reserve_next(self, None, reserve_for).await? {
                return Ok(message);
            }

            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    #[instrument("db.queue.try_dequeue_any", skip_all, fields(otel.kind = ?OpenTelemetrySpanKind::Consumer), err(Display))]
    async fn try_dequeue_any(
        &self,
        reserve_for: chrono::Duration,
    ) -> Result<Option<QueueMessage<serde_json::Value>>, Error> {
        reserve_next(self, None, reserve_for).await
    }

    #[instrument("db.queue.complete", skip_all, fields(otel.kind = ?OpenTelemetrySpanKind::Consumer), err(Display))]
    async fn complete<P: Into<Cow<'static, str>> + Send, T: Send + 'static>(
        &self,
        partition: P,
        msg: QueueMessage<T>,
    ) -> Result<(), Error> {
        let partition = partition.into().into_owned();
        let (key, reservation) = (msg.key, msg.reservation_id);

        self.write(move |tx| {
            tx.execute(
                "DELETE FROM queues \
                 WHERE partition = ?1 AND key = ?2 AND reserved_by = ?3",
                (partition, key, reservation),
            )
        })
        .await?;

        Ok(())
    }

    #[instrument("db.queue.reserve", skip_all, fields(otel.kind = ?OpenTelemetrySpanKind::Consumer), err(Display))]
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
    ) -> Result<(), Error> {
        let partition = partition.into().into_owned();
        let key = key.into().into_owned();
        let reservation = reservation_id.into().into_owned();
        let hidden_until = Timestamp::from(chrono::Utc::now() + reserve_for);

        self.write(move |tx| {
            tx.execute(
                "UPDATE queues SET hidden_until = ?1 \
                 WHERE partition = ?2 AND key = ?3 AND reserved_by = ?4",
                rusqlite::params![hidden_until, partition, key, reservation],
            )
        })
        .await?;

        Ok(())
    }

    #[instrument("db.queue.peek", skip_all, err(Display))]
    async fn peek<P: Into<Cow<'static, str>> + Send, T: DeserializeOwned + Send + 'static>(
        &self,
        partition: P,
        max_items: usize,
    ) -> Result<Vec<PeekedMessage<T>>, Error> {
        let partition = partition.into().into_owned();

        self.read(move |c| {
            let mut statement = c.prepare(
                "SELECT key, payload, scheduled_at, hidden_until, reserved_by, \
                        traceparent, tracestate, idempotency_key, attempts \
                 FROM queues WHERE partition = ?1 ORDER BY scheduled_at ASC LIMIT ?2",
            )?;
            let rows =
                statement.query_map(rusqlite::params![partition, max_items as i64], |row| {
                    Ok(PeekedMessage {
                        key: row.get(0)?,
                        payload: super::row::json_col(row, 1)?,
                        scheduled_at: ts(row, 2)?,
                        hidden_until: ts(row, 3)?,
                        reserved_by: row.get(4)?,
                        traceparent: row.get(5)?,
                        tracestate: row.get(6)?,
                        idempotency_key: row.get(7)?,
                        attempts: row.get::<_, i64>(8)?.max(0) as u32,
                    })
                })?;

            rows.collect()
        })
        .await
    }

    #[instrument("db.queue.purge", skip_all, err(Display))]
    async fn purge<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send>(
        &self,
        partition: P,
        key: K,
    ) -> Result<(), Error> {
        let partition = partition.into().into_owned();
        let key = key.into().into_owned();

        self.write(move |tx| {
            tx.execute(
                "DELETE FROM queues WHERE partition = ?1 AND key = ?2",
                (partition, key),
            )
        })
        .await?;

        Ok(())
    }

    #[instrument("db.queue.partitions", skip_all, err(Display))]
    async fn partitions(&self) -> Result<Vec<String>, Error> {
        self.read(|c| {
            let mut statement =
                c.prepare("SELECT DISTINCT partition FROM queues ORDER BY partition ASC")?;

            statement.query_map([], |row| row.get(0))?.collect()
        })
        .await
    }
}

/// Reads and hides the next due message in one transaction.
///
/// `partition` of `None` takes the oldest due message from anywhere, which is
/// first-come-first-served across the installation.
async fn reserve_next(
    db: &Database,
    partition: Option<String>,
    reserve_for: chrono::Duration,
) -> Result<Option<QueueMessage<serde_json::Value>>, Error> {
    let reservation_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now();
    let hidden_until = Timestamp::from(now + reserve_for);
    let now = Timestamp::from(now);
    let held = reservation_id.clone();

    db.write(move |tx| {
        let (sql, params) = match &partition {
            Some(partition) => (
                format!(
                    "SELECT {RESERVED_COLUMNS} FROM queues \
                     WHERE partition = ?2 AND hidden_until <= ?1 \
                     ORDER BY scheduled_at ASC LIMIT 1"
                ),
                rusqlite::params_from_iter(vec![now.to_text(), partition.clone()]),
            ),
            None => (
                format!(
                    "SELECT {RESERVED_COLUMNS} FROM queues \
                     WHERE hidden_until <= ?1 ORDER BY scheduled_at ASC LIMIT 1"
                ),
                rusqlite::params_from_iter(vec![now.to_text()]),
            ),
        };

        let message = tx
            .query_one(&sql, params, |row| {
                Ok(QueueMessage {
                    partition: row.get(0)?,
                    key: row.get(1)?,
                    payload: super::row::json_col(row, 2)?,
                    scheduled_at: ts(row, 3)?,
                    traceparent: row.get(4)?,
                    tracestate: row.get(5)?,
                    idempotency_key: row.get(6)?,
                    // The update below is about to add one.
                    attempts: row.get::<_, i64>(7)?.max(0) as u32 + 1,
                    reservation_id: held.clone(),
                })
            })
            .optional()?;

        if let Some(message) = &message {
            tx.execute(
                "UPDATE queues SET reserved_by = ?1, hidden_until = ?2, attempts = attempts + 1 \
                 WHERE partition = ?3 AND key = ?4",
                rusqlite::params![held, hidden_until, message.partition, message.key],
            )?;
        }

        Ok(message)
    })
    .await
}

/// Re-reads a reserved message's payload as the caller's own type.
fn typed<T: DeserializeOwned>(
    message: QueueMessage<serde_json::Value>,
) -> Result<QueueMessage<T>, Error> {
    Ok(QueueMessage {
        payload: serde_json::from_value(message.payload).wrap_system_err(
            "A queued job's payload was not the shape its handler expects.",
            super::ADVICE_REPORT_DEV,
        )?,
        key: message.key,
        partition: message.partition,
        reservation_id: message.reservation_id,
        scheduled_at: message.scheduled_at,
        attempts: message.attempts,
        traceparent: message.traceparent,
        tracestate: message.tracestate,
        idempotency_key: message.idempotency_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Job {
        step: u32,
    }

    const RESERVE: chrono::TimeDelta = chrono::TimeDelta::seconds(30);

    async fn queue() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn a_message_comes_back_out_with_its_payload() {
        let db = queue().await;
        let queued = db
            .enqueue("pki", Job { step: 1 }, None, None)
            .await
            .unwrap();

        assert!(queued.created);
        assert_eq!(queued.partition, "pki");

        let message: QueueMessage<Job> = db.dequeue("pki", RESERVE).await.unwrap();
        assert_eq!(message.payload, Job { step: 1 });
        assert_eq!(message.attempts, 1);
        assert_eq!(message.idempotency_key, None);
    }

    #[tokio::test]
    async fn a_reserved_message_is_not_handed_out_twice() {
        let db = queue().await;
        db.enqueue("pki", Job { step: 1 }, None, None)
            .await
            .unwrap();

        let first = db.try_dequeue_any(RESERVE).await.unwrap();
        assert!(first.is_some());

        let second = db.try_dequeue_any(RESERVE).await.unwrap();
        assert!(second.is_none(), "the message should still be hidden");
    }

    #[tokio::test]
    async fn completing_with_a_stale_reservation_removes_nothing() {
        let db = queue().await;
        db.enqueue("pki", Job { step: 1 }, None, None)
            .await
            .unwrap();

        let mut held: QueueMessage<Job> = db.dequeue("pki", RESERVE).await.unwrap();
        held.reservation_id = "somebody-else".into();
        db.complete("pki", held).await.unwrap();

        assert_eq!(db.peek::<_, Job>("pki", 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn completing_with_the_right_reservation_removes_the_message() {
        let db = queue().await;
        db.enqueue("pki", Job { step: 1 }, None, None)
            .await
            .unwrap();

        let held: QueueMessage<Job> = db.dequeue("pki", RESERVE).await.unwrap();
        db.complete("pki", held).await.unwrap();

        assert!(db.peek::<_, Job>("pki", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_idempotency_key_reschedules_rather_than_duplicating() {
        let db = queue().await;

        let first = db
            .enqueue("pki", Job { step: 1 }, Some("renew-ca".into()), None)
            .await
            .unwrap();
        let second = db
            .enqueue("pki", Job { step: 2 }, Some("renew-ca".into()), None)
            .await
            .unwrap();

        assert!(first.created);
        assert!(!second.created, "the second enqueue updated the first row");

        let peeked = db.peek::<_, Job>("pki", 10).await.unwrap();
        assert_eq!(peeked.len(), 1);
        assert_eq!(peeked[0].payload, Job { step: 2 });
        assert_eq!(peeked[0].idempotency_key.as_deref(), Some("renew-ca"));
    }

    #[tokio::test]
    async fn a_delayed_message_is_not_due_yet() {
        let db = queue().await;
        db.enqueue(
            "pki",
            Job { step: 1 },
            None,
            Some(chrono::TimeDelta::hours(1)),
        )
        .await
        .unwrap();

        assert!(db.try_dequeue_any(RESERVE).await.unwrap().is_none());
        assert_eq!(db.peek::<_, Job>("pki", 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn shortening_a_reservation_makes_the_message_due_again() {
        let db = queue().await;
        db.enqueue("pki", Job { step: 1 }, None, None)
            .await
            .unwrap();

        let held: QueueMessage<Job> = db.dequeue("pki", RESERVE).await.unwrap();
        db.reserve(
            "pki",
            held.key.clone(),
            held.reservation_id.clone(),
            chrono::TimeDelta::seconds(-1),
        )
        .await
        .unwrap();

        let again = db.try_dequeue_any(RESERVE).await.unwrap().unwrap();
        assert_eq!(again.attempts, 2, "the retry should be counted");
    }

    #[tokio::test]
    async fn dequeue_any_takes_the_oldest_across_partitions() {
        let db = queue().await;
        db.enqueue("a", Job { step: 1 }, None, None).await.unwrap();
        db.enqueue("b", Job { step: 2 }, None, None).await.unwrap();

        let first = db.try_dequeue_any(RESERVE).await.unwrap().unwrap();
        assert_eq!(first.partition, "a");

        assert_eq!(db.partitions().await.unwrap(), vec!["a", "b"]);
    }

    #[tokio::test]
    async fn purging_removes_a_message_whether_or_not_it_is_held() {
        let db = queue().await;
        db.enqueue("pki", Job { step: 1 }, Some("k".into()), None)
            .await
            .unwrap();

        let _held: QueueMessage<Job> = db.dequeue("pki", RESERVE).await.unwrap();
        db.purge("pki", "k").await.unwrap();

        assert!(db.peek::<_, Job>("pki", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_payload_of_the_wrong_shape_is_reported_rather_than_panicking() {
        let db = queue().await;
        db.enqueue("pki", serde_json::json!({"unexpected": true}), None, None)
            .await
            .unwrap();

        let read: Result<QueueMessage<Job>, _> = db.dequeue("pki", RESERVE).await;
        assert!(read.is_err());
    }
}
