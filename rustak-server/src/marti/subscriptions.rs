//! `/Marti/api/subscription{,s}/*` — the diagnostic view of what is connected.
//!
//! TAK Server's admin UI reads this; neither ATAK nor CloudTAK's core flows do,
//! and node-tak defines the type without calling it. It is here because the two
//! endpoints that *are* reached — `POST …/incognito/{uid}` and
//! `DELETE …/delete/{uid}` — have nowhere else to live, and because an admin UI
//! that cannot list subscriptions cannot offer either of them.
//!
//! # Every key is present, null when unknown
//!
//! The model has thirty-odd fields, most of them device telemetry rustak has no
//! source for — heap sizes, battery temperature, frame rate. They are serialised
//! as `null` rather than omitted, because a client reading `row.battery` wants a
//! value it can test and `undefined` is the one answer that throws.
//!
//! # `incognito` is a live property, not a stored one
//!
//! It belongs to the connection rather than to the device row: a client asks to
//! disappear for as long as it is connected, and reconnecting starts it visible
//! again unless its device row says otherwise. So the toggle is a `404` when
//! nothing by that uid is connected — there is nothing to hide.

use actix_web::web;
use chrono::Utc;
use rustak_api::Direction;

use crate::prelude::*;
use crate::stream::{ClientEndpoint, LeaveReason};

use super::channels::{self, GroupJson};
use super::error::{MartiError, MartiResult};
use super::extract::CiQuery;
use super::principal::MartiPrincipal;
use super::{kind, response};

/// What TAK Server calls the handler behind a streaming subscription.
const HANDLER_TYPE: &str = "NioNettyHandler";

/// The protocol a mutually authenticated CoT stream reports as.
const PROTOCOL: &str = "tls";

/// One live subscription, in TAK Server's diagnostic shape.
///
/// Field names are upstream's, including the ones that read oddly
/// (`lastReportMilliseconds` is an instant, not a duration).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubscriptionInfoJson {
    dn: Option<String>,
    callsign: String,
    client_uid: String,
    last_report_milliseconds: i64,
    last_report_diff_milliseconds: i64,
    tak_client: Option<String>,
    tak_version: Option<String>,
    username: String,
    groups: Vec<GroupJson>,
    role: String,
    ip_address: String,
    port: u16,
    pending_writes: Option<u64>,
    team: String,
    protocol: &'static str,
    xpath: Option<String>,
    subscription_uid: String,
    num_processed: Option<u64>,
    app_framerate: Option<f64>,
    battery: Option<f64>,
    battery_status: Option<String>,
    battery_temp: Option<f64>,
    device_data_rx: Option<u64>,
    device_data_tx: Option<u64>,
    heap_current_size: Option<u64>,
    heap_free_size: Option<u64>,
    heap_max_size: Option<u64>,
    /// Upstream capitalises the acronym here and nowhere else in the model, so
    /// the rename is written out rather than left to `rename_all`.
    #[serde(rename = "deviceIPAddress")]
    device_ip_address: Option<String>,
    storage_available: Option<u64>,
    storage_total: Option<u64>,
    incognito: bool,
    handler_type: &'static str,
    metrics: Option<serde_json::Value>,
}

/// `GET /Marti/api/subscriptions/all?sortBy&direction&page&limit`.
///
/// The paging and sorting parameters are accepted and ignored: a subscription
/// list is bounded by the connection limit, and answering a page of it would be
/// a second contract for no benefit.
///
/// # Errors
///
/// [`MartiError::Unauthorized`] for an anonymous caller, and
/// [`MartiError::Internal`] when a read fails.
pub async fn all(who: MartiPrincipal, context: web::Data<AppContext>, _: CiQuery) -> MartiResult {
    Ok(response::ok(
        kind::SUBSCRIPTION_INFO,
        rows(&who, &context).await?,
    ))
}

/// `GET /Marti/api/subscription/{uid}` — one of them, by client uid.
///
/// # Errors
///
/// As [`all`]; a uid nothing is connected under answers `404` with the envelope
/// and no `data`, which is what the plural endpoint's client expects to see.
pub async fn one(
    who: MartiPrincipal,
    context: web::Data<AppContext>,
    uid: web::Path<String>,
) -> MartiResult {
    let found = rows(&who, &context)
        .await?
        .into_iter()
        .find(|row| row.client_uid == *uid);

    match found {
        Some(row) => Ok(response::ok(kind::SUBSCRIPTION_INFO, row)),
        None => Ok(response::status(
            actix_web::http::StatusCode::NOT_FOUND,
            kind::SUBSCRIPTION_INFO,
            None::<SubscriptionInfoJson>,
        )),
    }
}

/// `POST /Marti/api/subscriptions/incognito/{uid}` — toggle, not set.
///
/// TAK Server has no "set to this value" spelling and neither do we: the client
/// asking is the one that knows what it is now.
///
/// # Errors
///
/// [`MartiError::Unauthorized`] for an anonymous caller,
/// [`MartiError::Forbidden`] when the subscription is somebody else's, and
/// [`MartiError::NotFound`] when nothing is connected under that uid.
pub async fn incognito(
    who: MartiPrincipal,
    context: web::Data<AppContext>,
    uid: web::Path<String>,
) -> MartiResult {
    let resolved = who.require()?;
    let live = context.live()?;
    let hub = live.hub();

    let Some(handle) = hub.handles_for_uid(&uid).into_iter().next() else {
        return Err(MartiError::NotFound(uid.to_string()));
    };

    // A subscription somebody else owns is not this caller's to hide, however
    // visible it happens to be to them.
    let owned = hub
        .principal(handle.id())
        .is_some_and(|principal| principal.username == resolved.user.username);

    if !owned && !who.is_admin() {
        return Err(MartiError::Forbidden(uid.to_string()));
    }

    let now = hub.is_incognito(handle.id());
    hub.set_incognito(handle.id(), !now);

    Ok(response::text(actix_web::http::StatusCode::OK, ""))
}

/// `DELETE /Marti/api/subscriptions/delete/{uid}` — close the connection.
///
/// Administrative. It does not revoke anything: the device may reconnect with
/// the same certificate a moment later, which is the point — this is how an
/// operator clears a client that is misbehaving, not how one is taken off the
/// installation.
///
/// # Errors
///
/// [`MartiError::Unauthorized`]/[`MartiError::Forbidden`] for a caller who does
/// not administer this installation, and [`MartiError::NotFound`] when nothing
/// is connected under that uid.
pub async fn delete(
    who: MartiPrincipal,
    context: web::Data<AppContext>,
    uid: web::Path<String>,
) -> MartiResult {
    who.require_admin()?;

    let live = context.live()?;
    let handles = live.hub().handles_for_uid(&uid);

    if handles.is_empty() {
        return Err(MartiError::NotFound(uid.to_string()));
    }

    for handle in &handles {
        handle.close(LeaveReason::Administrator);
    }

    info!(uid = %uid, connections = handles.len(), "Closed a subscription an administrator asked us to.");

    Ok(response::ok(
        kind::STRING,
        format!("Closed {} connection(s) for {uid}", handles.len()),
    ))
}

/// `PUT`/`DELETE /Marti/api/subscriptions/{clientUid}/filter` — accepted, no-op.
///
/// The geospatial filters these set are deferred; answering `200` rather than
/// `501` is deliberate, because a client that sets one and is refused stops
/// rather than carrying on unfiltered, and unfiltered is what it would have got
/// from a server that had never heard of the feature.
///
/// # Errors
///
/// [`MartiError::Unauthorized`] for an anonymous caller.
pub async fn filter(who: MartiPrincipal) -> MartiResult {
    who.require()?;

    Ok(response::text(actix_web::http::StatusCode::OK, ""))
}

/// Every subscription the caller may see.
async fn rows(
    who: &MartiPrincipal,
    context: &AppContext,
) -> Result<Vec<SubscriptionInfoJson>, MartiError> {
    let resolved = who.require()?;

    if !context.has_live() {
        return Ok(Vec::new());
    }

    let live = context.live()?;
    let peers = match who.is_admin() {
        true => live.snapshot(),
        false => live.snapshot_for(&resolved.principal),
    };

    if peers.is_empty() {
        return Ok(Vec::new());
    }

    let groups = channels::rows(context).await?;

    Ok(peers.iter().map(|peer| row(peer, &groups)).collect())
}

/// One live connection, as the diagnostic shape.
fn row(peer: &ClientEndpoint, groups: &[crate::db::repos::GroupRow]) -> SubscriptionInfoJson {
    let last = peer.last_status.timestamp_millis();
    // `takv` is `platform:version`; TAK Server carries the two halves.
    let (client, version) = match peer.takv.split_once(':') {
        Some((client, version)) => (Some(client.to_string()), Some(version.to_string())),
        None => (Some(peer.takv.clone()), None),
    };

    SubscriptionInfoJson {
        dn: None,
        callsign: peer.callsign.clone(),
        client_uid: peer.uid.clone(),
        last_report_milliseconds: last,
        last_report_diff_milliseconds: (Utc::now() - peer.last_status).num_milliseconds(),
        tak_client: client,
        tak_version: version,
        username: peer.username.clone(),
        groups: peer
            .groups
            .iter()
            .filter_map(|name| groups.iter().find(|row| &row.name == name))
            .map(|row| GroupJson {
                name: row.name.to_string(),
                direction: Direction::Out.as_str(),
                created: super::time::group_date(row.created_at),
                kind: channels::SYSTEM,
                bitpos: row.bitpos,
                active: true,
                description: row.description.clone(),
            })
            .collect(),
        role: peer.role.clone(),
        ip_address: peer.peer.ip().to_string(),
        port: peer.peer.port(),
        pending_writes: None,
        team: peer.team.clone(),
        protocol: PROTOCOL,
        xpath: None,
        subscription_uid: peer.uid.clone(),
        num_processed: None,
        app_framerate: None,
        battery: None,
        battery_status: None,
        battery_temp: None,
        device_data_rx: None,
        device_data_tx: None,
        heap_current_size: None,
        heap_free_size: None,
        heap_max_size: None,
        device_ip_address: None,
        storage_available: None,
        storage_total: None,
        incognito: peer.incognito,
        handler_type: HANDLER_TYPE,
        metrics: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> ClientEndpoint {
        ClientEndpoint {
            uid: "UID-A".to_string(),
            callsign: "ALPHA".to_string(),
            username: "ada".to_string(),
            team: "Cyan".to_string(),
            role: "Team Member".to_string(),
            takv: "ATAK:5.0.0".to_string(),
            groups: Vec::new(),
            last_status: Utc::now(),
            connected_at: Utc::now(),
            incognito: false,
            mode: rustak_cot::codec::Mode::Xml,
            peer: "10.0.0.7:41234".parse().unwrap(),
        }
    }

    #[test]
    fn every_key_is_present_even_when_we_have_no_value_for_it() {
        // A client reading `row.battery` wants something it can test; an absent
        // key is the one answer that throws.
        let json = serde_json::to_value(row(&peer(), &[])).unwrap();
        let object = json.as_object().expect("an object");

        for key in [
            "dn",
            "callsign",
            "clientUid",
            "lastReportMilliseconds",
            "lastReportDiffMilliseconds",
            "takClient",
            "takVersion",
            "username",
            "groups",
            "role",
            "ipAddress",
            "port",
            "pendingWrites",
            "team",
            "protocol",
            "xpath",
            "subscriptionUid",
            "numProcessed",
            "appFramerate",
            "battery",
            "batteryStatus",
            "batteryTemp",
            "deviceDataRx",
            "deviceDataTx",
            "heapCurrentSize",
            "heapFreeSize",
            "heapMaxSize",
            "deviceIPAddress",
            "storageAvailable",
            "storageTotal",
            "incognito",
            "handlerType",
            "metrics",
        ] {
            assert!(object.contains_key(key), "{key} is missing");
        }
    }

    #[test]
    fn the_platform_and_the_version_are_the_two_halves_of_takv() {
        let json = serde_json::to_value(row(&peer(), &[])).unwrap();

        assert_eq!(json["takClient"], "ATAK");
        assert_eq!(json["takVersion"], "5.0.0");
        assert_eq!(json["ipAddress"], "10.0.0.7");
        assert_eq!(json["port"], 41234);
        assert_eq!(json["protocol"], PROTOCOL);
    }

    #[test]
    fn a_takv_with_no_version_is_still_a_client_name() {
        let mut peer = peer();
        peer.takv = "unknown".to_string();

        let json = serde_json::to_value(row(&peer, &[])).unwrap();

        assert_eq!(json["takClient"], "unknown");
        assert!(json["takVersion"].is_null());
    }
}
