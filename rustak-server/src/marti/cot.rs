//! `/Marti/api/cot/**` — reading back what this server relayed.
//!
//! Five routes, all answering XML off [`cot_store`](crate::cot_store), and all
//! `ROLE_ANONYMOUS` upstream (research `06` §11). They matter for two different
//! reasons:
//!
//! * **CloudTAK** reads `/cot/xml/{uid}` and `/cot/xml/{uid}/all` through
//!   node-tak's `query.single()` / `query.history()` and parses the result with
//!   node-cot (research `03` §3.14). Without them a Data Sync's markers have no
//!   history to draw.
//! * **ATAK** is handed `{public_url}/Marti/api/cot/xml/{uid}` by the oversize
//!   substitution in [`stream::writer`](crate::stream::writer): a protobuf
//!   message over 64 KiB is replaced with a `b-f-t-r` pointing here, and a `404`
//!   there is a user-visible failed transfer.
//!
//! # A single `<event>` or an `<events>` wrapper
//!
//! `/cot/xml/{uid}` answers **one** `<event>` document; every other route
//! answers `XML_HEADER + <events>…</events>`. The wrapper holds *elements*, so
//! each stored row has its declaration stripped on the way in — see
//! [`event_element`].
//!
//! # `404` with an empty body
//!
//! Upstream answers a bare `404` with nothing in it when a query matched
//! nothing, on all but `/cot` and `/cot/matchUid`. That is reproduced here
//! rather than the Marti JSON refusal shape, because the clients above treat a
//! body they cannot parse as a transport failure. The one exception is the
//! mission-scoped `{n}/cot`, which answers an empty `<events></events>`
//! (`compat/missions.md` §14) — a different endpoint with a different contract.
//!
//! # Who may see what
//!
//! Every row carries the sender's channel bit vector as it stood when the
//! message was relayed ([`LatestRow::visible_to`]), so a read answers exactly
//! what the caller would have received at the time. Memberships since changed
//! do not retroactively reveal or hide a message. An administrator sees
//! everything; an **anonymous** caller sees nothing, which is stricter than
//! upstream's `ROLE_ANONYMOUS` and is the only sensible reading of "filtered by
//! the caller's `OUT` channels" for a caller who has none.

use actix_web::http::header::CONTENT_TYPE;
use actix_web::{HttpResponse, web};
use chrono::{DateTime, Utc};
use rustak_cot::Event;

use crate::cot_store::latest::LatestRow;
use crate::cot_store::{latest, query};
use crate::missions::changes::{EVENTS_PROLOGUE, event_element};
use crate::prelude::*;

use super::error::{MartiError, MartiResult};
use super::principal::MartiPrincipal;
use super::time::{MAX_WINDOW_HOURS, TimeWindow};
use super::{CiQuery, response};

/// How many rows a `/cot/sa` or `/cot` answer may carry.
///
/// `query::MAX_PAGE` is the store's own ceiling; naming it here says that this
/// endpoint deliberately asks for one page rather than paging, because neither
/// upstream nor any client offers a cursor for it.
const MAX_ROWS: u32 = query::MAX_PAGE;

/// The most history entries `/cot/xml/{uid}/all` may carry.
const MAX_HISTORY: usize = 500;

/// Registers the CoT query surface.
///
/// `/cot/sa` and `/cot/matchUid` before `/cot`, and `/cot/xml/{uid}/all` before
/// `/cot/xml/{uid}`, because actix matches in registration order and a uid may
/// contain anything.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/cot/xml/{uid}/all", web::get().to(history))
        .route("/cot/xml/{uid}", web::get().to(single))
        .route("/cot/sa", web::get().to(situational_awareness))
        .route("/cot/matchUid", web::get().to(match_uid))
        .route("/cot", web::get().to(by_uids))
        .route("/cot", web::post().to(by_uids));
}

/// `GET /Marti/api/cot/xml/{uid}` — the last thing one uid said.
///
/// A single `<event>` with the XML declaration and **no** `<events>` wrapper,
/// `<marti>` stripped. This is the URL the oversize substitution points a
/// client at, so its shape is the shape ATAK expects to re-read a message it
/// could not be sent.
///
/// # Errors
///
/// [`MartiError::NotFound`] with an empty body when nothing is held for that
/// uid **or** when the caller was not entitled to receive it — the same answer,
/// because telling them apart is an oracle over everything this server relayed.
#[instrument("marti.cot.single", skip_all)]
pub async fn single(
    who: MartiPrincipal,
    uid: web::Path<String>,
    context: web::Data<AppContext>,
) -> MartiResult {
    let Some(row) = visible_row(&context, &who, &uid).await? else {
        return Ok(empty_not_found());
    };

    Ok(response::xml(crate::missions::changes::without_marti(
        &row.xml,
    )))
}

/// `GET /Marti/api/cot/xml/{uid}/all?secago|start|end` — that uid's history.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for a window that will not parse, and
/// [`MartiError::NotFound`] with an empty body when the window holds nothing.
#[instrument("marti.cot.history", skip_all)]
pub async fn history(
    who: MartiPrincipal,
    uid: web::Path<String>,
    query_string: CiQuery,
    context: web::Data<AppContext>,
) -> MartiResult {
    // The latest row is the gate: it is the only record of who the sender was
    // publishing to, and a uid with no row is one this caller has no claim on.
    if visible_row(&context, &who, &uid).await?.is_none() {
        return Ok(empty_not_found());
    }

    let window = TimeWindow::parse(
        query_string.parsed::<i64>("secago")?,
        query_string.get("start"),
        query_string.get("end"),
    )?;

    let events = query::history(
        context.db(),
        &context.config().streams_dir(),
        &uid,
        window.start,
        window.end,
        MAX_HISTORY,
    )
    .await?;

    if events.is_empty() {
        return Ok(empty_not_found());
    }

    Ok(response::xml(wrap(events.iter().map(rendered))))
}

/// `GET|POST /Marti/api/cot` — the latest event of each of several uids.
///
/// The body is a JSON array of uids on **both** verbs; upstream really does
/// read a body on the `GET`, and node-tak sends one.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for a body that is not a non-empty array of
/// strings, which is upstream's `400`.
#[instrument("marti.cot.by_uids", skip_all)]
pub async fn by_uids(
    who: MartiPrincipal,
    body: web::Bytes,
    context: web::Data<AppContext>,
) -> MartiResult {
    let uids: Vec<String> = serde_json::from_slice(&body)
        .map_err(|_| MartiError::InvalidRequest("a JSON array of uids is required".to_string()))?;

    if uids.is_empty() {
        return Err(MartiError::InvalidRequest(
            "a JSON array of uids is required".to_string(),
        ));
    }

    let mut rendered = Vec::new();

    for uid in uids.iter().take(MAX_ROWS as usize) {
        if let Some(row) = latest::latest_event(context.db(), uid).await?
            && visible(&row, &who)
        {
            rendered.push(event_element(&row.xml));
        }
    }

    // Never a `404`: upstream answers the wrapper whatever the set matched, and
    // only a missing or empty request body is an error here.
    Ok(response::xml(wrap(rendered.into_iter())))
}

/// `GET /Marti/api/cot/sa?start&end&left&bottom&right&top` — a window of SA.
///
/// `start` and `end` are required, the four bounds are all-or-nothing, and the
/// window may not exceed [`MAX_WINDOW_HOURS`] — all three of which upstream
/// answers `400` for rather than silently narrowing.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for a missing bound, a partial box, a window
/// that runs backwards or one longer than a day; [`MartiError::NotFound`] with
/// an empty body when it holds nothing.
#[instrument("marti.cot.sa", skip_all)]
pub async fn situational_awareness(
    who: MartiPrincipal,
    query_string: CiQuery,
    context: web::Data<AppContext>,
) -> MartiResult {
    let (start, end) = required_window(&query_string)?;
    let bounds = bbox(&query_string)?;

    let rows = query::latest(
        context.db(),
        query::LatestQuery {
            since: Some(start),
            until: Some(end),
            limit: MAX_ROWS,
            ..query::LatestQuery::default()
        },
    )
    .await?;

    let rendered: Vec<String> = rows
        .iter()
        .filter(|row| visible(row, &who))
        .filter(|row| bounds.is_none_or(|bounds| inside(row, bounds)))
        .map(|row| event_element(&row.xml))
        .collect();

    if rendered.is_empty() {
        return Ok(empty_not_found());
    }

    Ok(response::xml(wrap(rendered.into_iter())))
}

/// `GET /Marti/api/cot/matchUid?search=` — a bare JSON array of uids.
///
/// No envelope and always `200`, even when nothing matched: upstream returns a
/// `ResponseEntity<List<String>>` directly.
///
/// # Errors
///
/// A system error if the read fails.
#[instrument("marti.cot.match_uid", skip_all)]
pub async fn match_uid(
    who: MartiPrincipal,
    query_string: CiQuery,
    context: web::Data<AppContext>,
) -> MartiResult {
    let needle = query_string.get("search").unwrap_or(" ").trim().to_string();

    let rows = query::latest(
        context.db(),
        query::LatestQuery {
            limit: MAX_ROWS,
            ..query::LatestQuery::default()
        },
    )
    .await?;

    let matched: Vec<String> = rows
        .iter()
        .filter(|row| visible(row, &who))
        .filter(|row| needle.is_empty() || row.uid.contains(&needle))
        .map(|row| row.uid.clone())
        .collect();

    Ok(response::bare_json(&matched))
}

/// The stored row for a uid this caller was entitled to receive.
async fn visible_row(
    context: &AppContext,
    who: &MartiPrincipal,
    uid: &str,
) -> Result<Option<LatestRow>, MartiError> {
    Ok(latest::latest_event(context.db(), uid)
        .await?
        .filter(|row| visible(row, who)))
}

/// Upstream's `404` on these routes: the status and **nothing else**.
///
/// Not the Marti JSON refusal shape. node-cot parses the body of a CoT query as
/// XML and ATAK's `TakHttpClient` treats an unexpected body as a transport
/// failure, so a JSON explanation here is worse than silence. Returned as an
/// `Ok` rather than raised as a [`MartiError`] precisely because the error type
/// owes every other route a body.
fn empty_not_found() -> HttpResponse {
    HttpResponse::NotFound()
        .insert_header((CONTENT_TYPE, response::XML))
        .finish()
}

/// Whether this caller was entitled to receive the message.
///
/// An anonymous caller holds no channels at all, so this is `false` for every
/// row — which is what stops the public listener handing the whole relay
/// history to anybody who asks.
fn visible(row: &LatestRow, who: &MartiPrincipal) -> bool {
    if who.is_admin() {
        return true;
    }

    who.principal()
        .is_some_and(|principal| row.visible_to(&principal.groups))
}

/// The `<events>` document a list of rendered elements makes.
fn wrap(elements: impl Iterator<Item = String>) -> String {
    let mut document = String::from(EVENTS_PROLOGUE);

    for element in elements {
        document.push_str(&element);
        document.push('\n');
    }

    document.push_str("</events>");

    document
}

/// One decoded history event as an element of an `<events>` document.
fn rendered(event: &Event) -> String {
    event_element(&String::from_utf8_lossy(&rustak_cot::xml::write(event)))
}

/// The `start`/`end` pair `/cot/sa` requires, bounded to a day.
fn required_window(query: &CiQuery) -> Result<(DateTime<Utc>, DateTime<Utc>), MartiError> {
    let (Some(start), Some(end)) = (query.get("start"), query.get("end")) else {
        return Err(MartiError::InvalidRequest(
            "start and end are required".to_string(),
        ));
    };

    let window = TimeWindow::parse(None, Some(start), Some(end))?;

    // Upstream throws rather than narrowing, and a client that asked for a week
    // and silently got a day would draw a map it could not explain.
    if window.capped {
        return Err(MartiError::InvalidRequest(format!(
            "that window is longer than {MAX_WINDOW_HOURS} hours"
        )));
    }

    Ok((window.start, window.end))
}

/// The four bounds, which arrive together or not at all.
fn bbox(query: &CiQuery) -> Result<Option<[f64; 4]>, MartiError> {
    let corners = [
        query.parsed::<f64>("left")?,
        query.parsed::<f64>("bottom")?,
        query.parsed::<f64>("right")?,
        query.parsed::<f64>("top")?,
    ];

    if corners.iter().all(Option::is_none) {
        return Ok(None);
    }

    let mut bounds = [0.0f64; 4];

    for (slot, corner) in bounds.iter_mut().zip(corners) {
        *slot = corner.ok_or_else(|| {
            MartiError::InvalidRequest(
                "left, bottom, right and top are given together or not at all".to_string(),
            )
        })?;
    }

    Ok(Some(bounds))
}

/// Whether a stored message was reported from inside the box.
///
/// Read out of the stored XML rather than out of a column: `cot_latest` keeps
/// the bytes the recipients were sent, and the `<point>` inside them is the same
/// answer without a column that could disagree with the message it describes.
/// A row we cannot re-parse is left out — a filter that fails open is not one.
fn inside(row: &LatestRow, [left, bottom, right, top]: [f64; 4]) -> bool {
    rustak_cot::xml::parse_str(&row.xml).is_ok_and(|event| {
        event.point.lon >= left
            && event.point.lon <= right
            && event.point.lat >= bottom
            && event.point.lat <= top
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(pairs: &[(&str, &str)]) -> CiQuery {
        CiQuery::parse(
            &pairs
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("&"),
        )
    }

    #[test]
    fn a_bounding_box_is_all_four_corners_or_none() {
        assert_eq!(bbox(&query(&[])).unwrap(), None);
        assert_eq!(
            bbox(&query(&[
                ("left", "-1"),
                ("bottom", "50"),
                ("right", "1"),
                ("top", "52"),
            ]))
            .unwrap(),
            Some([-1.0, 50.0, 1.0, 52.0]),
        );
        assert!(
            bbox(&query(&[("left", "-1"), ("bottom", "50")])).is_err(),
            "half a box would silently answer for the whole world",
        );
    }

    #[test]
    fn sa_refuses_a_window_it_was_not_given_and_one_longer_than_a_day() {
        assert!(required_window(&query(&[])).is_err());
        assert!(required_window(&query(&[("start", "2026-01-01T00:00:00Z")])).is_err());
        assert!(
            required_window(&query(&[
                ("start", "2026-01-01T00:00:00Z"),
                ("end", "2026-01-03T00:00:00Z"),
            ]))
            .is_err(),
            "two days is longer than the cap, and upstream refuses rather than narrowing",
        );
        assert!(
            required_window(&query(&[
                ("start", "2026-01-01T00:00:00Z"),
                ("end", "2026-01-01T06:00:00Z"),
            ]))
            .is_ok()
        );
    }

    #[test]
    fn an_events_wrapper_holds_elements_rather_than_documents() {
        let document = wrap(["<event uid=\"A\"/>".to_string()].into_iter());

        assert!(document.starts_with("<?xml version='1.0'"));
        assert_eq!(
            document.matches("<?xml").count(),
            1,
            "a declaration inside the wrapper would make the document unparseable",
        );
        assert!(document.ends_with("</events>"));
    }
}
