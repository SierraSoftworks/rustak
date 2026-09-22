//! `PUT` and `DELETE /api/v1/map/features/{uid}`: a marker placed, moved,
//! renamed or removed from the admin console.
//!
//! Neither writes to the store directly. A marker is a CoT event, and the one
//! thing that makes a CoT event real on this server is being relayed: tagged
//! with this server's flow tag, recorded in `cot_latest` and the history
//! segments, fanned out to every device that may see it, and announced to
//! every open map. So a `PUT` becomes an event and goes through
//! [`Router::publish`], and what the map draws afterwards is what the router
//! recorded — the same bytes a device would have been sent.
//!
//! A `DELETE` is the same again with the message every TAK client understands
//! as "forget this": a `t-x-d-d` naming the uid in its `<link>`. The row it
//! leaves behind in `cot_latest` is taken out too, because a snapshot read a
//! minute later must not resurrect what a device has just been told to drop.
//!
//! # Who may publish
//!
//! Anybody signed in, into the channels they hold in the `IN` direction —
//! exactly what their own device could publish. A channel they do not hold
//! is refused as the stream refuses it; naming none is a broadcast to
//! whoever they reach.

use actix_web::{HttpResponse, web};
use chrono::{Duration, Utc};
use rustak_api::PublishFeature;
use rustak_cot::detail::marti::{Dest, marti_element};
use rustak_cot::detail::{Contact, Element, Remarks};
use rustak_cot::event::Point;
use rustak_cot::types::cot_type;
use rustak_cot::{CotTime, Event};

use crate::cot_store::query;
use crate::prelude::*;
use crate::stream::Disposition;
use crate::stream::dest::DropReason;

use super::feature;
use crate::web::api::cot::readable_row;
use crate::web::api::error::{ApiError, ApiResult, json_ok};
use crate::web::api::extract::Authenticated;
use crate::web::api::subject::failed;

/// How long a marker stays current when the page does not say.
const DEFAULT_LIFETIME: Duration = Duration::hours(24);

/// `PUT /api/v1/map/features/{uid}` — publishes a marker, replacing whatever
/// that uid said before. Answers the feature as the map now draws it.
///
/// # Errors
///
/// A `400` for a uid or type that is not one, or a type a map does not draw;
/// a `403` for a channel the caller may not publish into; a `404` for a
/// channel that does not exist; a `503` when there is no stream to publish
/// through.
pub async fn put(
    context: web::Data<AppContext>,
    uid: web::Path<String>,
    request: web::Json<PublishFeature>,
    caller: Authenticated,
) -> ApiResult {
    let uid = uid.into_inner();
    let draft = request.into_inner();

    if uid.trim().is_empty() || uid.len() > 256 {
        return Err(ApiError::bad_request("That is not a usable uid."));
    }
    if !well_formed_type(&draft.kind) || !feature::drawable(&draft.kind) {
        return Err(ApiError::bad_request(
            "That is not a CoT type a map draws: it should look like a-u-G or b-m-p-s-m.",
        ));
    }

    let event = event_from(&uid, &draft);
    let relayed = relay(&context, &caller, event.clone()).await?;

    let groups = draft.groups.clone();
    let published = feature::from_event(relayed.event(), Utc::now(), groups);

    Ok(json_ok(&published))
}

/// `DELETE /api/v1/map/features/{uid}` — tells every device and every map to
/// forget a marker, and forgets it here.
///
/// # Errors
///
/// A `404` when nothing is held for that uid or the caller may not see what
/// is; a `503` when there is no stream to publish through.
pub async fn remove(
    context: web::Data<AppContext>,
    uid: web::Path<String>,
    caller: Authenticated,
) -> ApiResult {
    let uid = uid.into_inner();
    let row = readable_row(&context, &uid, &caller).await?;

    let event = Event::builder(cot_type::DISCONNECT, format!("{uid}-delete"))
        .how("h-g-i-g-o")
        .stale_after(std::time::Duration::from_secs(60))
        .push(
            Element::new("link")
                .attr("uid", uid.as_str())
                .attr("relation", "p-p")
                .attr("type", row.kind.as_str()),
        )
        .push(Element::new("__forcedelete"))
        .build();

    relay(&context, &caller, event).await?;

    query::forget(context.db(), &context.config().streams_dir(), &uid)
        .await
        .map_err(|err| failed(&context, &err))?;

    Ok(HttpResponse::NoContent().finish())
}

/// Sends `event` through the stream as the caller, and answers it as relayed.
async fn relay(
    context: &web::Data<AppContext>,
    caller: &Authenticated,
    event: Event,
) -> Result<std::sync::Arc<rustak_cot::codec::EncodedEvent>, ApiError> {
    let Ok(live) = context.live() else {
        return Err(ApiError::new(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "The CoT stream is not running on this server, so nothing can be published.",
        ));
    };

    // The relayed copy is what the tap announces, so it is read back from
    // there rather than reconstructed: the flow tag and the times are the
    // router's.
    let uid = event.uid.clone();
    let mut seen = live.router().tap().subscribe();
    let disposition = live
        .router()
        .publish(std::sync::Arc::new(caller.principal.clone()), event)
        .await;

    match disposition {
        Disposition::Dropped(DropReason::GroupNotMember(group)) => {
            return Err(ApiError::new(
                actix_web::http::StatusCode::FORBIDDEN,
                format!("You cannot publish into the {group} channel."),
            ));
        }
        Disposition::Dropped(DropReason::NoSuchGroup(group)) => {
            return Err(ApiError::not_found(format!(
                "There is no channel called {group} on this server."
            )));
        }
        // Reaching nobody is not a failure: it was recorded and every open
        // map was told.
        Disposition::Dropped(_) | Disposition::Relayed { .. } | Disposition::Control(_) => {}
    }

    // Whatever else was relayed in between is somebody else's.
    while let Ok(relayed) = seen.try_recv() {
        if relayed.encoded.event().uid == uid {
            return Ok(std::sync::Arc::clone(&relayed.encoded));
        }
    }

    Err(ApiError::new(
        actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
        "The marker was not relayed.",
    ))
}

/// A CoT event for a marker, as a TAK client would write one it placed.
fn event_from(uid: &str, draft: &PublishFeature) -> Event {
    let now = CotTime::now();
    let stale = draft
        .stale
        .map(CotTime::from_datetime)
        .unwrap_or_else(|| CotTime::from_datetime(Utc::now() + DEFAULT_LIFETIME));

    let mut builder = Event::builder(draft.kind.trim(), uid)
        .how(draft.how.as_deref().unwrap_or("h-g-i-g-o"))
        .time(now)
        .start(now)
        .stale(stale)
        .point_full(Point {
            lat: draft.point.lat,
            lon: draft.point.lon,
            hae: draft.point.hae.unwrap_or(Point::UNKNOWN_HAE),
            ce: draft.point.ce.unwrap_or(Point::UNKNOWN_CE),
            le: draft.point.le.unwrap_or(Point::UNKNOWN_LE),
        })
        .typed(&Contact::new(draft.callsign.trim()))
        // What ATAK writes on a marker it means to keep across a restart.
        .push(Element::new("archive"));

    if let Some(remarks) = draft
        .remarks
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        builder = builder.typed(&Remarks {
            text: remarks.to_owned(),
            ..Remarks::default()
        });
    }
    if let Some(sidc) = draft.sidc.as_deref().and_then(rustak_api::map::sidc) {
        builder = builder.push(Element::new("__milicon").attr("id", sidc));
    }
    if !draft.groups.is_empty() {
        let dests: Vec<Dest> = draft.groups.iter().map(Dest::group).collect();
        builder = builder.push(marti_element(&dests));
    }

    builder.build()
}

/// Whether text is shaped like a CoT type: dash-separated alphanumeric
/// segments, the first one letter.
fn well_formed_type(kind: &str) -> bool {
    let kind = kind.trim();
    let mut segments = kind.split('-');

    matches!(segments.next(), Some(first) if first.len() == 1 && first.chars().all(|c| c.is_ascii_lowercase()))
        && segments.all(|segment| {
            !segment.is_empty() && segment.chars().all(|c| c.is_ascii_alphanumeric())
        })
        && kind.len() <= 64
}

#[cfg(test)]
mod tests {
    use rustak_api::MapPoint;
    use rustak_cot::detail::Track;

    use super::*;

    fn draft() -> PublishFeature {
        PublishFeature {
            kind: "a-u-G".to_string(),
            callsign: " MARKER 1 ".to_string(),
            point: MapPoint {
                lat: 51.5,
                lon: -0.12,
                hae: Some(30.0),
                ce: None,
                le: None,
            },
            how: None,
            remarks: Some("  Two vehicles.  ".to_string()),
            sidc: Some("10031000001211000000".to_string()),
            stale: None,
            groups: vec!["Blue".to_string()],
        }
    }

    #[test]
    fn a_draft_becomes_the_event_a_client_would_have_placed() {
        let event = event_from("MARKER-1", &draft());
        let drawn = feature::from_event(&event, Utc::now(), Vec::new());

        assert_eq!(event.how.as_deref(), Some("h-g-i-g-o"));
        assert_eq!(drawn.callsign.as_deref(), Some("MARKER 1"));
        assert_eq!(drawn.remarks.as_deref(), Some("Two vehicles."));
        assert_eq!(drawn.sidc.as_deref(), Some("10031000001211000000"));
        assert_eq!(drawn.point.hae, Some(30.0));
        assert_eq!(drawn.point.ce, None, "an error nobody gave is unknown");
        assert!(event.detail.find("archive").is_some());
        assert!(event.detail.get::<Track>().is_none());
        assert!(
            event.detail.find("marti").is_some(),
            "the channels are the message's destinations"
        );
        assert!(
            drawn.stale > Utc::now() + Duration::hours(23),
            "a marker lasts a day unless told otherwise"
        );
    }

    #[test]
    fn what_a_draft_leaves_out_is_left_out_of_the_event() {
        let event = event_from(
            "MARKER-2",
            &PublishFeature {
                remarks: Some("   ".to_string()),
                sidc: Some("not a code".to_string()),
                groups: Vec::new(),
                ..draft()
            },
        );

        assert!(event.detail.find("remarks").is_none());
        assert!(event.detail.find("__milicon").is_none());
        assert!(event.detail.find("marti").is_none());
    }

    #[test]
    fn only_something_shaped_like_a_type_is_one() {
        for (kind, ok) in [
            ("a-u-G", true),
            ("b-m-p-s-m", true),
            ("a-f-G-E-V-A-T-H", true),
            ("", false),
            ("a--G", false),
            ("aa-u-G", false),
            ("a-u-G;drop", false),
            ("A-u-G", false),
        ] {
            assert_eq!(well_formed_type(kind), ok, "{kind:?}");
        }
    }
}
