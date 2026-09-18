//! `/api/v1/cot`: what this server has relayed, as an operator browses it.
//!
//! Reading is not administrative. The stream is the product, and somebody
//! diagnosing why a marker is not on their map needs to see what the server
//! holds for that uid — but only the messages they were entitled to receive in
//! the first place, which is what [`LatestRow::visible_to`] decides from the
//! sender's channels at send time. An administrator sees everything.
//!
//! Deleting **is** administrative and takes both stores with it: the latest row
//! a map draws from and every history segment the uid wrote. There is no undo,
//! which is why it is not offered to the person whose device sent the message.
//!
//! # Why a filtered page may be short
//!
//! The type and callsign predicates are decided in SQL, so a page is a page.
//! The channel rule cannot be: a sender's channels are a bit vector on the row
//! and SQLite has nothing to test it with. So a caller who is not an
//! administrator gets a page that has been narrowed after the fact, exactly as
//! `files::search` does for exactly the same reason — twenty asked for, eleven
//! answered, and the next page starts where this one did.

use actix_web::{HttpResponse, web};
use chrono::{DateTime, Duration, Utc};
use rustak_api::{AuditCategory, AuditOutcome, CotDetail, CotSummary};
use rustak_cot::Event;

use crate::cot_store::latest::LatestRow;
use crate::cot_store::{latest, query};
use crate::db::AuditEntry;
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::{Administrative, Authenticated};
use super::subject::failed;

/// How many messages one page carries when the caller does not say.
const PAGE_SIZE: u32 = 100;

/// How far back a history request looks when it names no window.
const DEFAULT_SECAGO: i64 = 3600;

/// The most history entries one request may carry.
const MAX_HISTORY: usize = 500;

/// Registers the CoT browser's routes.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/cot", web::get().to(list))
        .route("/cot/{uid}", web::get().to(get))
        .route("/cot/{uid}", web::delete().to(remove))
        .route("/cot/{uid}/history", web::get().to(history));
}

/// What a listing may be narrowed by.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    /// A CoT type prefix: `a-` for atoms, `b-t-f` for chat.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,

    /// Part of a callsign, matched without regard to case.
    #[serde(default)]
    pub callsign: Option<String>,

    /// Only the messages whose sender was publishing into this channel.
    #[serde(default)]
    pub group: Option<GroupName>,

    #[serde(default)]
    pub page: Option<u32>,

    #[serde(default)]
    pub limit: Option<u32>,
}

/// The window a history request asks for.
#[derive(Debug, Default, Deserialize)]
pub struct HistoryQuery {
    /// How many seconds back to look, which is TAK's own spelling.
    #[serde(default)]
    pub secago: Option<i64>,

    #[serde(default)]
    pub start: Option<DateTime<Utc>>,

    #[serde(default)]
    pub end: Option<DateTime<Utc>>,

    #[serde(default)]
    pub limit: Option<usize>,
}

impl HistoryQuery {
    /// The window, with `start`/`end` beating `secago` where both are given.
    ///
    /// # Errors
    ///
    /// A `400` when the window runs backwards, which is almost always a client
    /// that has swapped the two parameters.
    fn window(&self) -> Result<(DateTime<Utc>, DateTime<Utc>), ApiError> {
        let now = Utc::now();
        let end = self.end.unwrap_or(now);
        let start = match (self.start, self.secago) {
            (Some(start), _) => start,
            (None, Some(secago)) => end - Duration::seconds(secago.max(0)),
            (None, None) => end - Duration::seconds(DEFAULT_SECAGO),
        };

        if start > end {
            return Err(ApiError::bad_request("That window ends before it starts."));
        }

        Ok((start, end))
    }
}

/// `GET /api/v1/cot?type&callsign&group&page&limit`.
///
/// # Errors
///
/// A `404` when a channel is named that this installation does not have, and a
/// `500` when a read fails.
pub async fn list(
    context: web::Data<AppContext>,
    request: web::Query<ListQuery>,
    caller: Authenticated,
) -> ApiResult {
    let limit = request.limit.unwrap_or(PAGE_SIZE).clamp(1, query::MAX_PAGE);
    let rows = query::latest(
        context.db(),
        query::LatestQuery {
            kind: request.kind.clone(),
            callsign: request.callsign.clone(),
            limit,
            offset: request.page.unwrap_or(0).saturating_mul(limit),
        },
    )
    .await
    .map_err(|err| failed(&context, &err))?;

    let index = index(&context).await?;
    let bitpos = match &request.group {
        Some(name) => Some(index.bitpos(name).ok_or_else(|| {
            ApiError::not_found("There is no channel with that name on this server.")
        })?),
        None => None,
    };

    let listed: Vec<CotSummary> = rows
        .into_iter()
        .filter(|row| readable(row, &caller))
        .filter(|row| bitpos.is_none_or(|bitpos| publishes_into(row, bitpos)))
        .map(|row| summarise(&row, &index))
        .collect();

    Ok(json_ok(&listed))
}

/// `GET /api/v1/cot/{uid}` — the latest message, with its XML.
///
/// # Errors
///
/// A `404` when nothing is held for that uid **or** when the caller may not
/// see what is, and a `500` when a read fails.
pub async fn get(
    context: web::Data<AppContext>,
    uid: web::Path<String>,
    caller: Authenticated,
) -> ApiResult {
    let row = readable_row(&context, &uid, &caller).await?;
    let index = index(&context).await?;

    Ok(json_ok(&CotDetail {
        summary: summarise(&row, &index),
        xml: row.xml.clone(),
    }))
}

/// `GET /api/v1/cot/{uid}/history?secago&start&end&limit` — newest first.
///
/// # Errors
///
/// A `400` for a window that ends before it starts, a `404` as [`get`], and a
/// `500` when the index or a segment cannot be read.
pub async fn history(
    context: web::Data<AppContext>,
    uid: web::Path<String>,
    request: web::Query<HistoryQuery>,
    caller: Authenticated,
) -> ApiResult {
    // The latest row is the gate: it is the only record of who the sender was
    // publishing to, and a uid with no row is one this caller has no claim on.
    let row = readable_row(&context, &uid, &caller).await?;
    let (start, end) = request.window()?;
    let limit = request.limit.unwrap_or(MAX_HISTORY).clamp(1, MAX_HISTORY);

    let events = query::history(
        context.db(),
        &context.config().streams_dir(),
        &uid,
        start,
        end,
        limit,
    )
    .await
    .map_err(|err| failed(&context, &err))?;

    let index = index(&context).await?;
    let groups = names(&row, &index);
    let listed: Vec<CotSummary> = events
        .iter()
        .map(|event| from_event(event, groups.clone()))
        .collect();

    Ok(json_ok(&listed))
}

/// `DELETE /api/v1/cot/{uid}` — forget the latest row and every segment.
///
/// # Errors
///
/// A `404` when nothing is held for that uid, and a `500` when a write fails.
pub async fn remove(
    context: web::Data<AppContext>,
    uid: web::Path<String>,
    caller: Administrative,
) -> ApiResult {
    let forgotten = query::forget(context.db(), &context.config().streams_dir(), &uid)
        .await
        .map_err(|err| failed(&context, &err))?;

    if forgotten == query::Forgotten::default() {
        return Err(ApiError::not_found("Nothing is stored under that uid."));
    }

    let entry = AuditEntry::new(
        AuditCategory::Administration,
        "cot.deleted",
        AuditOutcome::Success,
    )
    .subject(&*uid)
    .actor(&caller.user.username)
    .detail(serde_json::json!({ "segments": forgotten.segments }));

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a CoT deletion in the audit log.");
        context.session().record_human_error(&err);
    }

    Ok(HttpResponse::NoContent().finish())
}

/// The latest row for a uid the caller may see, or a `404`.
///
/// A message that is here but not theirs answers the same `404` as one that is
/// not here: telling a caller that a uid exists but is out of their channels is
/// an oracle over everything this server has relayed.
async fn readable_row(
    context: &web::Data<AppContext>,
    uid: &str,
    caller: &Authenticated,
) -> Result<LatestRow, ApiError> {
    latest::latest_event(context.db(), uid)
        .await
        .map_err(|err| failed(context, &err))?
        .filter(|row| readable(row, caller))
        .ok_or_else(|| ApiError::not_found("Nothing is stored under that uid."))
}

/// Whether this caller was entitled to receive the message.
fn readable(row: &LatestRow, caller: &Authenticated) -> bool {
    caller.principal.is_admin || row.visible_to(&caller.principal.groups)
}

/// Whether the sender was publishing into one particular channel.
fn publishes_into(row: &LatestRow, bitpos: u32) -> bool {
    GroupSet::from_bytes(&row.group_bits).is_ok_and(|sender| sender.contains(bitpos, Direction::In))
}

/// The channel names a stored message's sender was publishing into.
///
/// A bit with no name is a channel deleted since the message was sent; it is
/// left out rather than rendered as a number nobody can act on.
fn names(row: &LatestRow, index: &GroupIndex) -> Vec<String> {
    GroupSet::from_bytes(&row.group_bits)
        .map(|sender| {
            sender
                .names(index, Direction::In)
                .into_iter()
                .map(|name| name.as_str().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One stored row as the browser reads it.
///
/// The team and the role are parsed out of the stored XML rather than stored
/// beside it: `cot_latest` keeps the bytes the recipients were sent, and the
/// `<__group>` inside them is the same answer without a column that could
/// disagree with the message it describes.
fn summarise(row: &LatestRow, index: &GroupIndex) -> CotSummary {
    let parsed = rustak_cot::xml::parse_str(&row.xml).ok();
    let group = parsed.as_ref().and_then(Event::group);
    let point = parsed.as_ref().map(|event| event.point);

    CotSummary {
        uid: row.uid.clone(),
        kind: row.kind.clone(),
        callsign: row.callsign.clone(),
        team: group.as_ref().map(|group| group.name.clone()),
        role: group.as_ref().map(|group| group.role.clone()),
        time: row.time,
        stale: row.stale,
        received_at: row.received_at,
        lat: point.map_or(0.0, |point| point.lat),
        lon: point.map_or(0.0, |point| point.lon),
        groups: names(row, index),
    }
}

/// One history entry as the browser reads it.
///
/// The channels are the ones the *latest* row records, because a segment holds
/// only the payload — so they describe the sender rather than the individual
/// message, and a message sent before a membership changed may be listed under
/// a channel it did not reach.
fn from_event(event: &Event, groups: Vec<String>) -> CotSummary {
    let group = event.group();
    let now = Utc::now();

    CotSummary {
        uid: event.uid.clone(),
        kind: event.r#type.clone(),
        callsign: event.callsign().map(str::to_owned),
        team: group.as_ref().map(|group| group.name.clone()),
        role: group.as_ref().map(|group| group.role.clone()),
        time: event.time.to_datetime().unwrap_or(now),
        stale: event.stale.to_datetime().unwrap_or(now),
        received_at: event.time.to_datetime().unwrap_or(now),
        lat: event.point.lat,
        lon: event.point.lon,
        groups,
    }
}

/// The channel index, which every renderer here needs and none should read
/// once per row.
async fn index(context: &web::Data<AppContext>) -> Result<GroupIndex, ApiError> {
    context
        .db()
        .groups()
        .index()
        .await
        .map_err(|err| failed(context, &err))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(bits: &[(u32, Direction)]) -> LatestRow {
        let mut groups = GroupSet::new();
        for (bitpos, direction) in bits {
            groups.set(*bitpos, *direction);
        }

        LatestRow {
            uid: "UID-A".to_string(),
            kind: "a-f-G-U-C".to_string(),
            callsign: Some("ALPHA".to_string()),
            user_id: None,
            device_id: None,
            group_bits: groups.to_bytes(),
            time: Utc::now(),
            stale: Utc::now(),
            xml: "<event version=\"2.0\" uid=\"UID-A\" type=\"a-f-G-U-C\" \
                  time=\"2026-09-18T12:00:00.000Z\" start=\"2026-09-18T12:00:00.000Z\" \
                  stale=\"2026-09-18T12:02:00.000Z\" how=\"m-g\">\
                  <point lat=\"51.5\" lon=\"-0.12\" hae=\"9999999.0\" ce=\"9999999.0\" \
                  le=\"9999999.0\"/><detail><__group name=\"Cyan\" role=\"Team Member\"/>\
                  </detail></event>"
                .to_string(),
            received_at: Utc::now(),
        }
    }

    fn index_with(entries: &[(u32, &str)]) -> GroupIndex {
        let mut index = GroupIndex::new();
        for (bitpos, name) in entries {
            index.insert(*bitpos, GroupName::parse(name).unwrap());
        }

        index
    }

    #[test]
    fn the_team_and_role_come_out_of_the_bytes_the_recipients_were_sent() {
        let summary = summarise(&row(&[(7, Direction::In)]), &index_with(&[(7, "Blue")]));

        assert_eq!(summary.team.as_deref(), Some("Cyan"));
        assert_eq!(summary.role.as_deref(), Some("Team Member"));
        assert_eq!(summary.groups, vec!["Blue".to_string()]);
        assert!((summary.lat - 51.5).abs() < f64::EPSILON);
    }

    #[test]
    fn a_channel_deleted_since_the_message_was_sent_is_left_out() {
        // The bit is still on the row; nothing can name it any more, and a bare
        // number in a channel column is worse than a shorter list.
        let summary = summarise(
            &row(&[(7, Direction::In), (9, Direction::In)]),
            &index_with(&[(7, "Blue")]),
        );

        assert_eq!(summary.groups, vec!["Blue".to_string()]);
    }

    #[test]
    fn a_message_that_cannot_be_parsed_still_lists() {
        // A row written by an older schema, or truncated: the uid, the type and
        // the times are columns, so the row is still worth showing.
        let mut broken = row(&[]);
        broken.xml = "not xml".to_string();

        let summary = summarise(&broken, &index_with(&[]));

        assert_eq!(summary.uid, "UID-A");
        assert!(summary.team.is_none());
        assert_eq!(summary.lat, 0.0);
    }

    #[test]
    fn the_group_filter_asks_which_channel_the_sender_published_into() {
        let published = row(&[(7, Direction::In)]);

        assert!(publishes_into(&published, 7));
        assert!(
            !publishes_into(&published, 9),
            "a channel the sender only receives from is not where this went",
        );
    }

    #[test]
    fn a_window_that_ends_before_it_starts_is_refused() {
        let now = Utc::now();
        let backwards = HistoryQuery {
            start: Some(now),
            end: Some(now - Duration::hours(1)),
            ..HistoryQuery::default()
        };

        assert!(backwards.window().is_err());
    }

    #[test]
    fn secago_is_relative_to_the_end_of_the_window_and_start_beats_it() {
        let end = Utc::now();
        let (start, resolved) = HistoryQuery {
            secago: Some(600),
            end: Some(end),
            ..HistoryQuery::default()
        }
        .window()
        .unwrap();

        assert_eq!(resolved, end);
        assert_eq!((end - start).num_seconds(), 600);

        let explicit = end - Duration::days(1);
        let (start, _) = HistoryQuery {
            secago: Some(600),
            start: Some(explicit),
            end: Some(end),
            ..HistoryQuery::default()
        }
        .window()
        .unwrap();

        assert_eq!(start, explicit, "an explicit start is not a suggestion");
    }

    #[test]
    fn a_negative_secago_is_now_rather_than_the_future() {
        let end = Utc::now();
        let (start, _) = HistoryQuery {
            secago: Some(-600),
            end: Some(end),
            ..HistoryQuery::default()
        }
        .window()
        .unwrap();

        assert_eq!(start, end);
    }
}
