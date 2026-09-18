//! The latest situational-awareness message this server holds for each uid,
//! and what came before it.
//!
//! # Reading is narrowed, not refused
//!
//! Somebody diagnosing why a marker is not on their map needs to see what the
//! server holds for that uid, so a listing is filtered to what the caller was
//! entitled to *receive* — the sender's channels at the time the message was
//! sent — rather than being administrative. Deleting is administrative: it
//! drops the latest row and every history segment, and there is no undo.
//!
//! # A uid that is not yours is a `404`
//!
//! Not a `403`, for the same reason packages answer the same way: telling a
//! caller that a uid exists but is out of their channels is an oracle over
//! every device on the installation.

use rustak_api::{CotDetail, CotSummary, GroupName};

use crate::api::{ApiError, delete_empty, get_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// What a listing is narrowed by.
#[derive(Clone, Default, PartialEq)]
pub struct CotFilter {
    /// A CoT type prefix: `a-` for atoms, `b-t-f` for chat.
    pub kind: String,

    /// Part of a callsign, matched without regard to case.
    pub callsign: String,

    /// Only the messages whose sender was publishing into this channel.
    pub group: Option<GroupName>,
}

/// The latest message per uid, newest relayed first.
pub async fn list(filter: &CotFilter) -> Result<Vec<CotSummary>, ApiError> {
    demo!(Ok(fixtures::cot(filter)));

    let mut query = Vec::new();
    if !filter.kind.trim().is_empty() {
        query.push(format!("type={}", urlencode(filter.kind.trim())));
    }
    if !filter.callsign.trim().is_empty() {
        query.push(format!("callsign={}", urlencode(filter.callsign.trim())));
    }
    if let Some(group) = &filter.group {
        query.push(format!("group={}", urlencode(group.as_str())));
    }

    match query.is_empty() {
        true => get_json("/cot").await,
        false => get_json(&format!("/cot?{}", query.join("&"))).await,
    }
}

/// One uid's latest message, with the XML its recipients were sent.
pub async fn get(uid: &str) -> Result<CotDetail, ApiError> {
    demo!(fixtures::cot_detail(uid));

    get_json(&format!("/cot/{}", urlencode(uid))).await
}

/// What that uid sent over the last `secago` seconds, newest first.
pub async fn history(uid: &str, secago: i64) -> Result<Vec<CotSummary>, ApiError> {
    demo!(Ok(fixtures::cot_history(uid, secago)));

    get_json(&format!("/cot/{}/history?secago={secago}", urlencode(uid))).await
}

/// Forgets a uid: the latest row and every history segment.
pub async fn forget(uid: &str) -> Result<(), ApiError> {
    demo!(fixtures::forget_cot(uid));

    delete_empty(&format!("/cot/{}", urlencode(uid))).await
}
