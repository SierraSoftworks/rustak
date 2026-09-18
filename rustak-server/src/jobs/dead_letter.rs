//! Where a message that has run out of attempts goes.
//!
//! A payload that no longer deserialises, or a handler with a deterministic
//! bug, does not become correct on the thousandth attempt. Before
//! `[jobs] max_attempts` existed it was retried every fifteen minutes for the
//! life of the installation, one error line at a time, burying the failures
//! that could still be fixed. Past the ceiling the message is kept here with
//! the error that ended it, and taken off the queue.

use chrono::Utc;
use rustak_core::prelude::*;

use crate::{
    db::{KeyValueStore, Queue, QueueMessage},
    services::{AppContext, Services},
};

/// The key-value partition a message that has run out of attempts is kept in.
///
/// A table of its own would be tidier, but the point of a dead letter is that
/// somebody can find it and read the error that put it there, and this is
/// already the durable, inspectable place for exactly that.
pub const DEAD_LETTERS: &str = "jobs/dead-letters";

/// Sets a message aside once it has run out of attempts.
///
/// A payload that no longer deserialises, or a handler with a deterministic
/// bug, does not become correct on the thousandth attempt: it just costs an
/// error line every backoff for the life of the installation and buries the
/// failures that can still be fixed. The message is kept with the error
/// that ended it, and removed from the queue.
pub async fn set_aside(
    context: &AppContext,
    item: QueueMessage<serde_json::Value>,
    name: &str,
    err: &Error,
) {
    let attempts = item.attempts;
    let letter = serde_json::json!({
        "partition": item.partition,
        "key": item.key,
        "payload": item.payload,
        "attempts": attempts,
        "scheduled_at": item.scheduled_at,
        "failed_at": Utc::now(),
        "error": err.to_string(),
    });
    let letter_key = format!("{}/{}", item.partition, item.key);

    error!(
        error = %err,
        job.name = name,
        job.attempts = attempts,
        "The job '{name}' has failed {attempts} times and will not be retried; \
         it is in the '{DEAD_LETTERS}' dead letters: {err}",
    );

    if let Err(err) = context.kv().set(DEAD_LETTERS, letter_key, letter).await {
        error!(error = %err, "Could not record a dead-lettered job: {err}");
        context.session().record_human_error(&err);
    }

    let partition = item.partition.clone();
    if let Err(err) = context.queue().complete(partition, item).await {
        error!(error = %err, "Could not remove a dead-lettered job from the queue: {err}");
        context.session().record_human_error(&err);
    }
}

/// Whatever a panicking handler carried, as text.
pub fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&'static str>() {
        return (*message).to_string();
    }

    if let Some(message) = panic.downcast_ref::<String>() {
        return message.clone();
    }

    "no message".to_string()
}
