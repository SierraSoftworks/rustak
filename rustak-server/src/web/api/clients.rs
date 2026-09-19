//! `/api/v1/clients`: what is connected to the CoT stream, and what used to be.
//!
//! Administrative throughout. A connection list names every device on the
//! installation, where each one is connecting from and which channels it holds;
//! that is an operator's view rather than a participant's, and the
//! participant's view already exists as `/Marti/api/contacts/all`, which is
//! narrowed to what the caller may reach.
//!
//! # A connection is not a device
//!
//! [`ConnectedClient`] exists for as long as a socket does and its callsign,
//! team and platform come out of the situational awareness the client sends —
//! not out of `devices`. So disconnecting one is not revoking anything: the
//! device may reconnect with the same certificate a moment later, which is the
//! point. Taking access away is `POST /api/v1/certificates/{id}/revoke`, and it
//! closes the connection as a consequence rather than as the request.
//!
//! # Incognito is set here and toggled there
//!
//! `POST /Marti/api/subscriptions/incognito/{uid}` toggles, because the client
//! asking is the one that knows what it is now. An operator's page does not, so
//! this endpoint takes the state it wants. The flag belongs to the connection
//! rather than to the device row, which is why there is nothing to set when
//! nothing is connected under that uid.

use std::collections::{HashMap, HashSet};

use actix_web::{HttpResponse, web};
use chrono::{Duration, Utc};
use rustak_api::{
    AuditCategory, AuditOutcome, ClientHistoryEntry, ConnectedClient, IncognitoRequest,
    StreamStatus,
};

use crate::cot_store::latest::{self, LatestRow};
use crate::db::AuditEntry;
use crate::db::repos::Page;
use crate::identity::devices;
use crate::prelude::*;
use crate::stream::{ClientEndpoint, LiveState};

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;
use super::subject::failed;

/// The protocol a mutually authenticated CoT stream reports as.
const PROTOCOL: &str = "tls";

/// How far back the history looks when the caller does not say.
const DEFAULT_SECAGO: i64 = 86_400;

/// How many devices one history request may describe.
const MAX_HISTORY: u32 = 500;

/// Registers the client routes, the literal `history` ahead of the `{uid}`
/// that would otherwise swallow it.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/clients/history", web::get().to(history))
        .route("/clients/status", web::get().to(status))
        .route("/clients", web::get().to(list))
        .route("/clients/{uid}", web::delete().to(disconnect))
        .route("/clients/{uid}/incognito", web::post().to(incognito));
}

/// How far back a history listing looks.
#[derive(Debug, Default, Deserialize)]
pub struct HistoryQuery {
    /// Seconds, TAK's own spelling. Absent means a day.
    #[serde(default)]
    pub secago: Option<i64>,

    #[serde(default)]
    pub limit: Option<u32>,
}

/// `GET /api/v1/clients`.
///
/// An installation with no stream listener answers an empty list rather than a
/// `503`: nothing is connected, which is the true answer and the one a page
/// can render.
///
/// # Errors
///
/// A `500` when the channel index cannot be read.
pub async fn list(context: web::Data<AppContext>, _: Administrative) -> ApiResult {
    if !context.has_live() {
        return Ok(json_ok(&Vec::<ConnectedClient>::new()));
    }

    let live = context.live().map_err(|err| failed(&context, &err))?;
    let index = context
        .db()
        .groups()
        .index()
        .await
        .map_err(|err| failed(&context, &err))?;

    let listed: Vec<ConnectedClient> = live
        .snapshot()
        .iter()
        .map(|peer| describe(peer, &live, &index))
        .collect();

    Ok(json_ok(&listed))
}

/// `GET /api/v1/clients/status` — what the listener itself is doing.
///
/// [`list`] answers `[]` both for an installation whose listener is running
/// with nobody on it and for one that has no listener at all, and a page cannot
/// tell an operator which without asking this. Administrative, like the rest of
/// the surface: how many devices are on an installation is not something the
/// public health check hands out.
///
/// # Errors
///
/// A `500` when the registry cannot be read, which it cannot be here.
pub async fn status(context: web::Data<AppContext>, _: Administrative) -> ApiResult {
    let enabled = context.config().stream.tls.enabled;

    // One read of the slot rather than two. This used to ask `has_live` for
    // the connection count and again for `bound`, so a listener that published
    // between the two was reported as bound with nobody on it — which is the
    // one answer this endpoint exists to make unambiguous.
    let live = match context.has_live() {
        true => Some(context.live().map_err(|err| failed(&context, &err))?),
        false => None,
    };

    let connections = live.as_ref().map_or(0, |live| live.connected());

    Ok(json_ok(&StreamStatus {
        enabled,
        bound: live.is_some(),
        // When the listener bound, not when the process started: the two are
        // the same on an ordinary start and are not on an installation whose
        // listener came up late.
        bound_at: live.as_ref().map(|live| live.bound_at()),
        connections: u32::try_from(connections).unwrap_or(u32::MAX),
    }))
}

/// `DELETE /api/v1/clients/{uid}` — close every connection claiming that uid.
///
/// # Errors
///
/// A `404` when nothing is connected under that uid, and a `503` when this
/// installation has no stream listener.
pub async fn disconnect(
    context: web::Data<AppContext>,
    uid: web::Path<String>,
    caller: Administrative,
) -> ApiResult {
    let live = live(&context)?;
    let handles = live.hub().handles_for_uid(&uid);

    if handles.is_empty() {
        return Err(missing());
    }

    for handle in &handles {
        handle.close();
    }

    info!(
        uid = %uid,
        connections = handles.len(),
        "Closed a client's connections at an administrator's request."
    );

    record(
        &context,
        "client.disconnected",
        &caller,
        &uid,
        serde_json::json!({ "connections": handles.len() }),
    )
    .await;

    Ok(HttpResponse::NoContent().finish())
}

/// `POST /api/v1/clients/{uid}/incognito` — set, rather than toggle.
///
/// # Errors
///
/// A `404` when nothing is connected under that uid, and a `503` when this
/// installation has no stream listener.
pub async fn incognito(
    context: web::Data<AppContext>,
    uid: web::Path<String>,
    body: web::Json<IncognitoRequest>,
    caller: Administrative,
) -> ApiResult {
    let live = live(&context)?;
    let hub = live.hub();
    let handles = hub.handles_for_uid(&uid);

    if handles.is_empty() {
        return Err(missing());
    }

    let on = body.on;
    for handle in &handles {
        hub.set_incognito(handle.id(), on);
    }

    record(
        &context,
        "client.incognito",
        &caller,
        &uid,
        serde_json::json!({ "on": on, "connections": handles.len() }),
    )
    .await;

    // The updated connection rather than the request that changed it, so the
    // page that drew the switch has the row it is now looking at instead of
    // having to re-read the whole list to find out.
    let index = context
        .db()
        .groups()
        .index()
        .await
        .map_err(|err| failed(&context, &err))?;
    let updated = live
        .snapshot()
        .iter()
        .find(|peer| peer.uid == *uid)
        .map(|peer| describe(peer, &live, &index))
        .ok_or_else(missing)?;

    Ok(json_ok(&updated))
}

/// `GET /api/v1/clients/history?secago&limit`.
///
/// Every device seen inside the window, connected or not, described with
/// whatever its last stored message said about it.
///
/// # Errors
///
/// A `500` when a read fails.
pub async fn history(
    context: web::Data<AppContext>,
    request: web::Query<HistoryQuery>,
    _: Administrative,
) -> ApiResult {
    let since = Utc::now() - Duration::seconds(request.secago.unwrap_or(DEFAULT_SECAGO).max(0));
    let limit = request.limit.unwrap_or(MAX_HISTORY).clamp(1, MAX_HISTORY);

    let rows = devices::list(context.db(), Page::first(limit))
        .await
        .map_err(|err| failed(&context, &err))?;
    let owners = devices::usernames(context.db())
        .await
        .map_err(|err| failed(&context, &err))?;

    // One read for every message in the window rather than one per device: an
    // installation's device list is hundreds of rows and its stored messages
    // are one per uid.
    let stored: HashMap<String, LatestRow> =
        latest::latest_events(context.db(), since, &[], MAX_HISTORY)
            .await
            .map_err(|err| failed(&context, &err))?
            .into_iter()
            .map(|row| (row.uid.clone(), row))
            .collect();

    let connected: HashSet<String> = match context.has_live() {
        true => context
            .live()
            .map_err(|err| failed(&context, &err))?
            .snapshot()
            .into_iter()
            .map(|peer| peer.uid)
            .collect(),
        false => HashSet::new(),
    };

    let listed: Vec<ClientHistoryEntry> = rows
        .iter()
        .filter(|row| row.last_seen_at >= since)
        .filter_map(|row| {
            let username = owners.get(&row.user_id)?;
            let uid = row.uid.as_str().to_string();
            let group = stored.get(&uid).and_then(team_and_role);

            Some(ClientHistoryEntry {
                callsign: row.callsign.clone(),
                username: username.to_string(),
                team: group.as_ref().map(|(team, _)| team.clone()),
                role: group.as_ref().map(|(_, role)| role.clone()),
                takv: platform(row),
                first_seen_at: row.first_seen_at,
                last_seen_at: row.last_seen_at,
                last_ip: row.last_ip,
                connected: connected.contains(&uid),
                client_uid: uid,
            })
        })
        .collect();

    Ok(json_ok(&listed))
}

/// One live connection as the API describes it.
///
/// The channel names come from the connection's principal rather than from the
/// account's stored memberships, because a device's *selection* narrows what it
/// actually holds and a page showing memberships would say a client can see a
/// channel it has switched off.
fn describe(peer: &ClientEndpoint, live: &LiveState, index: &GroupIndex) -> ConnectedClient {
    let groups = live
        .hub()
        .handles_for_uid(&peer.uid)
        .first()
        .and_then(|handle| live.hub().principal(handle.id()))
        .map(|principal| {
            (
                principal.groups.names(index, Direction::In),
                principal.groups.names(index, Direction::Out),
            )
        });
    let (in_groups, out_groups) = groups.unwrap_or_else(|| (Vec::new(), peer.groups.clone()));

    ConnectedClient {
        client_uid: peer.uid.clone(),
        callsign: peer.callsign.clone(),
        username: peer.username.clone(),
        team: peer.team.clone(),
        role: peer.role.clone(),
        takv: peer.takv.clone(),
        protocol: PROTOCOL.to_string(),
        ip: peer.peer.ip(),
        port: peer.peer.port(),
        connected_at: peer.connected_at,
        last_event_at: peer.last_status,
        incognito: peer.incognito,
        in_groups,
        out_groups,
    }
}

/// The team and role a stored message reported, when it reported either.
fn team_and_role(row: &LatestRow) -> Option<(String, String)> {
    let event = rustak_cot::xml::parse_str(&row.xml).ok()?;
    let group = event.group()?;

    Some((group.name, group.role))
}

/// `platform version`, from whichever halves the device told us.
fn platform(row: &crate::db::repos::DeviceRow) -> Option<String> {
    match (&row.platform, &row.version) {
        (Some(platform), Some(version)) => Some(format!("{platform} {version}")),
        (Some(platform), None) => Some(platform.clone()),
        (None, Some(version)) => Some(version.clone()),
        (None, None) => None,
    }
}

/// The live registry, or the `503` an installation with no listener answers.
fn live(context: &web::Data<AppContext>) -> Result<std::sync::Arc<LiveState>, ApiError> {
    context.live().map_err(|err| {
        context.session().record_human_error(&err);
        ApiError::new(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "This server is not running a CoT stream listener.",
        )
    })
}

/// What a uid nothing is connected under is answered with.
fn missing() -> ApiError {
    ApiError::not_found("Nothing is connected under that uid.")
}

/// Writes what was done to a client and who did it.
async fn record(
    context: &AppContext,
    action: &'static str,
    caller: &Administrative,
    uid: &str,
    detail: serde_json::Value,
) {
    let entry = AuditEntry::new(AuditCategory::Administration, action, AuditOutcome::Success)
        .subject(uid)
        .actor(&caller.user.username)
        .detail(detail);

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a client change in the audit log.");
        context.session().record_human_error(&err);
    }
}

#[cfg(test)]
mod tests {
    use crate::db::repos::DeviceRow;

    use super::*;

    fn row() -> LatestRow {
        LatestRow {
            uid: "ANDROID-1".to_string(),
            kind: "a-f-G-U-C".to_string(),
            callsign: Some("ALPHA".to_string()),
            user_id: None,
            device_id: None,
            group_bits: GroupSet::new().to_bytes(),
            time: Utc::now(),
            stale: Utc::now(),
            xml: "<event version=\"2.0\" uid=\"ANDROID-1\" type=\"a-f-G-U-C\" \
                  time=\"2026-09-18T12:00:00.000Z\" start=\"2026-09-18T12:00:00.000Z\" \
                  stale=\"2026-09-18T12:02:00.000Z\" how=\"m-g\">\
                  <point lat=\"51.5\" lon=\"-0.12\" hae=\"0\" ce=\"0\" le=\"0\"/>\
                  <detail><__group name=\"Cyan\" role=\"Team Member\"/></detail></event>"
                .to_string(),
            received_at: Utc::now(),
        }
    }

    fn device() -> DeviceRow {
        DeviceRow {
            id: DeviceId::from(1),
            uid: DeviceUid::parse("ANDROID-1").unwrap(),
            user_id: UserId::from(1),
            callsign: Some("ALPHA".to_string()),
            platform: Some("ATAK-CIV".to_string()),
            version: Some("5.6.0".to_string()),
            device_model: None,
            os: None,
            incognito: false,
            last_certificate_id: None,
            first_seen_at: Utc::now(),
            last_seen_at: Utc::now(),
            last_ip: None,
        }
    }

    #[test]
    fn the_team_and_role_come_out_of_the_last_stored_message() {
        assert_eq!(
            team_and_role(&row()),
            Some(("Cyan".to_string(), "Team Member".to_string())),
        );

        let mut unparseable = row();
        unparseable.xml = "not xml".to_string();
        assert_eq!(team_and_role(&unparseable), None);
    }

    #[test]
    fn a_platform_is_whichever_halves_the_device_told_us() {
        assert_eq!(platform(&device()).as_deref(), Some("ATAK-CIV 5.6.0"));

        let mut half = device();
        half.version = None;
        assert_eq!(platform(&half).as_deref(), Some("ATAK-CIV"));

        let mut silent = device();
        silent.platform = None;
        silent.version = None;
        assert_eq!(platform(&silent), None);
    }

    /// A registry with one connection that has identified itself.
    ///
    /// Built from the pieces the listener builds, so that `describe` is asked
    /// the same question here that it is asked in production — a hand-made
    /// `ClientEndpoint` would not exercise the principal lookup, which is where
    /// the two channel directions come from.
    async fn live_with_alpha(bits: &[(u32, Direction)]) -> LiveState {
        use std::sync::Arc;

        use crate::cot_store::CotStoreHandle;
        use crate::stream::{ConnHandle, Hub, Router, StreamMetrics, Subscription};

        let hub = Arc::new(Hub::new());
        let metrics = Arc::new(StreamMetrics::default());
        let store = CotStoreHandle::disabled();
        let router = Arc::new(Router::new(
            Arc::clone(&hub),
            crate::db::Database::open_in_memory().await.unwrap(),
            store.clone(),
            crate::stream::mission_hook::no_missions(),
            Arc::clone(&metrics),
            "rustak-test",
        ));

        let mut groups = GroupSet::new();
        for (bitpos, direction) in bits {
            groups.set(*bitpos, *direction);
        }

        let id = hub.next_id();
        let (tx, _rx) = tokio::sync::mpsc::channel(8);

        hub.register(Subscription::new(
            id,
            Arc::new(
                Principal::new(
                    UserId::from(1),
                    Username::parse("grace").unwrap(),
                    PrincipalKind::Person,
                    AuthMethod::SetupToken,
                )
                .with_groups(Arc::new(groups)),
            ),
            Vec::new(),
            format!("{:f>64}", "grace"),
            "198.51.100.7:41234".parse().unwrap(),
            ConnHandle::new(
                id,
                tx,
                Arc::new(crate::stream::subscription::ConnStats::default()),
                512,
                Shutdown::new(),
            ),
        ));

        hub.apply_event(
            id,
            &rustak_cot::Event::builder("a-f-G-U-C", "ANDROID-1")
                .point(51.5, -0.12)
                .typed(
                    &rustak_cot::detail::Contact::new("ALPHA")
                        .with_endpoint(rustak_cot::detail::contact::STREAMING_ENDPOINT),
                )
                .typed(&rustak_cot::detail::Group::new("Cyan", "Team Member"))
                .build(),
            None,
        );

        LiveState::new(hub, router, store, metrics)
    }

    #[tokio::test]
    async fn a_connection_is_described_with_both_of_its_channel_directions() {
        // An operator looking at a client that cannot see anybody needs to know
        // which of the two lists is empty, which the single list the TAK wire
        // format carries cannot say.
        let live = live_with_alpha(&[(7, Direction::In), (9, Direction::Out)]).await;
        let mut index = GroupIndex::new();
        index.insert(7, GroupName::parse("Blue").unwrap());
        index.insert(9, GroupName::parse("Red").unwrap());

        let peer = live.snapshot().pop().expect("one connection");
        let described = describe(&peer, &live, &index);

        assert_eq!(described.client_uid, "ANDROID-1");
        assert_eq!(described.callsign, "ALPHA");
        assert_eq!(described.username, "grace");
        assert_eq!(described.team, "Cyan");
        assert_eq!(described.role, "Team Member");
        assert_eq!(described.protocol, PROTOCOL);
        assert_eq!(described.ip.to_string(), "198.51.100.7");
        assert_eq!(described.port, 41234);
        assert!(!described.incognito);
        assert_eq!(
            described.in_groups,
            vec![GroupName::parse("Blue").unwrap()],
            "what it may publish into",
        );
        assert_eq!(
            described.out_groups,
            vec![GroupName::parse("Red").unwrap()],
            "what reaches it",
        );
    }

    #[tokio::test]
    async fn hiding_a_connection_answers_the_connection_rather_than_the_request() {
        // The page that drew the switch wants the row it is now looking at.
        // Answering with the body it sent made the toggle two requests.
        let live = live_with_alpha(&[(7, Direction::In)]).await;
        let mut index = GroupIndex::new();
        index.insert(7, GroupName::parse("Blue").unwrap());

        let hub = live.hub();
        for handle in hub.handles_for_uid("ANDROID-1") {
            hub.set_incognito(handle.id(), true);
        }

        let peer = live
            .snapshot()
            .into_iter()
            .find(|peer| peer.uid == "ANDROID-1")
            .expect("the connection that was just changed");
        let described = describe(&peer, &live, &index);

        assert!(described.incognito, "the answer carries the new state");
        assert_eq!(described.client_uid, "ANDROID-1");
        assert_eq!(
            described.callsign, "ALPHA",
            "and everything else the row needs, so nothing has to be re-read",
        );
        assert_eq!(described.in_groups, vec![GroupName::parse("Blue").unwrap()]);
    }

    #[tokio::test]
    async fn hiding_and_closing_a_connection_reach_it_by_the_uid_it_claims() {
        let live = live_with_alpha(&[]).await;
        let hub = live.hub();

        let handle = hub
            .handles_for_uid("ANDROID-1")
            .into_iter()
            .next()
            .expect("the connection the uid names");

        assert!(!hub.is_incognito(handle.id()));
        hub.set_incognito(handle.id(), true);
        assert!(hub.is_incognito(handle.id()));

        assert!(
            live.snapshot_for(&Principal::new(
                UserId::from(2),
                Username::parse("ada").unwrap(),
                PrincipalKind::Person,
                AuthMethod::SetupToken,
            ))
            .is_empty(),
            "an incognito client is out of everybody else's contact list",
        );

        handle.close();
        assert!(handle.is_closing());

        assert!(
            hub.handles_for_uid("ANDROID-2").is_empty(),
            "a uid nothing claims resolves to nothing, which is the 404",
        );
    }

    #[actix_web::test]
    async fn an_installation_with_no_listener_has_nothing_to_date() {
        use actix_web::{App, test};

        use crate::testing::TestServer;
        use crate::testing::context::bearer;

        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let status: StreamStatus = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/clients/status")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert!(!status.bound);
        assert!(
            status.bound_at.is_none(),
            "a listener that never bound has no moment to report",
        );
        assert_eq!(status.connections, 0);
    }

    #[actix_web::test]
    async fn a_bound_listener_says_when_it_bound() {
        use actix_web::{App, test};

        use crate::testing::TestServer;
        use crate::testing::context::bearer;

        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let before = Utc::now();
        let live = live_with_alpha(&[]).await;
        server
            .install_live(std::sync::Arc::new(live))
            .expect("the registry is installed once");
        let after = Utc::now();

        let app = test::init_service(App::new().configure(server.app())).await;

        let status: StreamStatus = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/clients/status")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert!(status.bound);
        assert_eq!(status.connections, 1);

        let bound_at = status.bound_at.expect("a bound listener reports when");
        assert!(
            bound_at >= before && bound_at <= after,
            "{bound_at} is not when the registry was built",
        );
    }
}
