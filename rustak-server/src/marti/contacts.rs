//! `/Marti/api/contacts/all` and `/Marti/api/clientEndPoints` — who else is here.
//!
//! Two read-only listings of the same population in two deliberately different
//! shapes, and the difference is load-bearing:
//!
//! * **`/contacts/all` is a bare JSON array.** Every other list endpoint in this
//!   family carries the `{version, type, data}` envelope; this one does not, and
//!   node-tak calls `.map` on the result with no envelope check. Wrapping it is
//!   a `TypeError` inside CloudTAK (`compat/contacts.md` §1).
//! * **`/clientEndPoints` is enveloped, and never cached.** It is the one
//!   endpoint TAK Server marks explicitly non-cacheable, because a stale contact
//!   list is indistinguishable from a contact who has left.
//!
//! # `lastStatus` has exactly two spellings
//!
//! ATAK runs `enum.valueOf` on it and discards the **whole response** — not the
//! one element — if any row says anything but `Connected` or `Disconnected`.
//! That is why the field is a `&'static str` from a two-armed match rather than
//! anything a caller could influence.
//!
//! # Where the rows come from
//!
//! Connected clients come from the stream registry, which is the only place
//! that knows a callsign: a certificate says which *account* a connection
//! belongs to, and everything a contact list shows comes out of the situational
//! -awareness messages the client sends after it connects. Disconnected ones
//! come from the `devices` table and its `last_seen_at`, which is what makes
//! `secAgo` mean anything at all on an installation whose fleet is asleep.
//!
//! An installation with the stream listener switched off has no registry, and
//! both endpoints then answer an empty list rather than a `500` — nobody can be
//! connected, so an empty answer is the true one.

use std::collections::HashMap;

use actix_web::HttpResponse;
use actix_web::http::header::CONTENT_TYPE;
use actix_web::web;
use chrono::{DateTime, Duration, Utc};
use rustak_api::{Direction, GroupName};
use rustak_core::identity::can_reach;

use crate::db::repos::Page;
use crate::identity::devices;
use crate::prelude::*;
use crate::stream::ClientEndpoint;

use super::error::{MartiError, MartiResult};
use super::extract::CiQuery;
use super::principal::MartiPrincipal;
use super::{ApiResponse, channels, kind, response, time};

/// What a row reports for a field the client never sent.
const UNKNOWN: &str = "unknown";

/// `lastStatus` for a client the listener is holding a socket to.
const CONNECTED: &str = "Connected";

/// `lastStatus` for a device that has enrolled and is not here now.
const DISCONNECTED: &str = "Disconnected";

/// How many devices one listing reads.
const DEVICE_PAGE: u32 = 500;

/// One contact, in the shape node-tak's `Contact` types against.
///
/// Every key is always present — `null` is never emitted — because node-tak's
/// own formatter calls `contact.notes.trim()` unguarded, so an absent field is
/// a crash rather than a blank.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ContactJson {
    /// rustak has no per-contact geospatial filter, so this is always empty.
    #[serde(rename = "filterGroups")]
    filter_groups: Vec<String>,
    notes: String,
    callsign: String,
    team: String,
    role: String,
    takv: String,
    uid: String,
}

/// One row of `/Marti/api/clientEndPoints`.
///
/// `groups` is deliberately absent: TAK Server computes channel visibility to
/// decide which rows to emit and does not serialise it, and adding it would put
/// one account's channel membership in another's contact list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ClientEndpointJson {
    callsign: String,
    uid: String,
    username: String,
    team: String,
    role: String,
    #[serde(rename = "lastStatus")]
    last_status: &'static str,
    #[serde(rename = "lastEventTime")]
    last_event_time: String,
}

/// `GET /Marti/api/contacts/all` — a **bare array**, never an envelope.
///
/// `sortBy` and `direction` are accepted and ignored: TAK Server declares them
/// and never applies them, and a `400` for a value a real server accepts would
/// break a client for being no worse than upstream.
///
/// # Errors
///
/// [`MartiError::Unauthorized`] for an anonymous caller.
pub async fn all(who: MartiPrincipal, context: web::Data<AppContext>) -> MartiResult {
    let resolved = who.require()?;

    let contacts: Vec<ContactJson> = reachable(&context, &resolved.principal, who.is_admin())
        .iter()
        .map(|peer| ContactJson {
            filter_groups: Vec::new(),
            // Free text on a real TAK Server; the account behind the device is
            // the one useful thing rustak has to put there, and it is never
            // null because node-tak trims it unguarded.
            notes: peer.username.clone(),
            callsign: peer.callsign.clone(),
            team: peer.team.clone(),
            role: peer.role.clone(),
            takv: peer.takv.clone(),
            uid: peer.uid.clone(),
        })
        .collect();

    Ok(response::bare_json(&contacts))
}

/// `GET /Marti/api/clientEndPoints?secAgo&showCurrentlyConnectedClients&showMostRecentOnly&group`.
///
/// `showMostRecentOnly` is accepted and ignored: there is one row per client uid
/// either way, because a device that reconnects is the same device.
///
/// # Errors
///
/// [`MartiError::Unauthorized`] for an anonymous caller,
/// [`MartiError::InvalidRequest`] for a negative `secAgo`,
/// [`MartiError::Forbidden`] when `group` names a channel the caller cannot
/// read, and [`MartiError::Internal`] when a read fails.
pub async fn client_endpoints(
    who: MartiPrincipal,
    context: web::Data<AppContext>,
    query: CiQuery,
) -> MartiResult {
    let resolved = who.require()?;
    let sec_ago: i64 = query.parsed("secAgo")?.unwrap_or(0);

    if sec_ago < 0 {
        return Err(MartiError::InvalidRequest(format!("secAgo={sec_ago}")));
    }

    let wanted = filter(&context, &who, &query).await?;
    let since = (sec_ago > 0).then(|| Utc::now() - Duration::seconds(sec_ago));

    let mut rows: Vec<ClientEndpointJson> =
        reachable(&context, &resolved.principal, who.is_admin())
            .iter()
            .filter(|peer| matches(&peer.groups, wanted.as_deref()))
            .filter(|peer| since.is_none_or(|since| peer.last_status >= since))
            .map(connected)
            .collect();

    if !query.flag("showCurrentlyConnectedClients").get() {
        let live: Vec<String> = rows.iter().map(|row| row.uid.clone()).collect();

        rows.extend(disconnected(&context, &who, wanted.as_deref(), since, &live).await?);
    }

    rows.sort_by(|left, right| left.callsign.cmp(&right.callsign));

    render(&rows)
}

/// The rows the stream registry holds, narrowed to what the caller may see.
///
/// Empty on an installation with no stream listener: nobody can be connected,
/// so nobody is.
fn reachable(context: &AppContext, viewer: &Principal, is_admin: bool) -> Vec<ClientEndpoint> {
    if !context.has_live() {
        return Vec::new();
    }

    match context.live() {
        Ok(live) if is_admin => live.snapshot(),
        Ok(live) => live.snapshot_for(viewer),
        Err(err) => {
            warn!(error = %err, "Could not read the live connections for a contact listing.");

            Vec::new()
        }
    }
}

/// The channel names a `group=` filter named, once the caller is allowed them.
///
/// [`None`] means no filter was sent, which is not the same as a filter that
/// matched nothing.
async fn filter(
    context: &AppContext,
    who: &MartiPrincipal,
    query: &CiQuery,
) -> Result<Option<Vec<String>>, MartiError> {
    let named = query.strings("group");

    if named.is_empty() {
        return Ok(None);
    }

    let rows = channels::rows(context).await?;
    let Some(resolved) = who.identity.as_ref() else {
        return Err(MartiError::Unauthorized("a credential is required".into()));
    };

    for name in &named {
        // A channel the caller cannot read is a hard refusal of the whole
        // request rather than a silent drop of that one name, per
        // `compat/contacts.md` §2: a filtered listing that quietly ignored half
        // the filter would look like an empty network.
        let readable = who.is_admin()
            || rows
                .iter()
                .find(|row| row.name.as_str() == name)
                .is_some_and(|row| resolved.principal.has_group(row.bitpos, Direction::Out));

        if !readable {
            return Err(MartiError::Forbidden(format!("group={name}")));
        }
    }

    Ok(Some(named))
}

/// Whether a client's channels satisfy the filter that was asked for.
fn matches(groups: &[GroupName], wanted: Option<&[String]>) -> bool {
    let Some(wanted) = wanted else {
        return true;
    };

    groups
        .iter()
        .any(|held| wanted.iter().any(|name| held.as_str() == name))
}

/// One live connection, as a row.
fn connected(peer: &ClientEndpoint) -> ClientEndpointJson {
    ClientEndpointJson {
        callsign: peer.callsign.clone(),
        uid: peer.uid.clone(),
        username: peer.username.clone(),
        team: peer.team.clone(),
        role: peer.role.clone(),
        last_status: CONNECTED,
        last_event_time: time::cot_date_unpadded(peer.last_status),
    }
}

/// The devices that have enrolled and are not connected now.
///
/// Visibility is judged from the *account's* channels rather than the device's
/// own switched-on set: a device that is not connected is not routing anything,
/// and whether its owner switched a channel off on it says nothing about
/// whether the caller was ever allowed to see it.
async fn disconnected(
    context: &AppContext,
    who: &MartiPrincipal,
    wanted: Option<&[String]>,
    since: Option<DateTime<Utc>>,
    live: &[String],
) -> Result<Vec<ClientEndpointJson>, MartiError> {
    let db = context.db();
    let index = db.groups().index().await?;
    let viewer = who.principal().map(|principal| &principal.groups);
    let mut owners: HashMap<UserId, (String, Vec<GroupName>)> = HashMap::new();
    let mut rows = Vec::new();

    for device in devices::list(db, Page::first(DEVICE_PAGE)).await? {
        if live.iter().any(|uid| uid == device.uid.as_str())
            || since.is_some_and(|since| device.last_seen_at < since)
        {
            continue;
        }

        let owner = match owners.get(&device.user_id) {
            Some(owner) => owner.clone(),
            None => {
                let Some(user) = db.users().get(device.user_id).await? else {
                    continue;
                };
                let groups = db.members().group_set(device.user_id).await?;
                let owner = (
                    user.username.to_string(),
                    groups.names(&index, Direction::Out),
                );

                // Read once per account rather than once per device: a fleet is
                // a handful of accounts and a great many phones.
                let visible =
                    who.is_admin() || viewer.is_some_and(|viewer| can_reach(&groups, viewer));

                owners.insert(device.user_id, owner.clone());

                if !visible {
                    continue;
                }

                owner
            }
        };

        if !matches(&owner.1, wanted) {
            continue;
        }

        rows.push(ClientEndpointJson {
            callsign: device
                .callsign
                .clone()
                .unwrap_or_else(|| device.uid.to_string()),
            uid: device.uid.to_string(),
            username: owner.0,
            team: UNKNOWN.to_string(),
            role: UNKNOWN.to_string(),
            last_status: DISCONNECTED,
            last_event_time: time::cot_date_unpadded(device.last_seen_at),
        });
    }

    Ok(rows)
}

/// The envelope, with the cache headers this one endpoint carries.
fn render(rows: &[ClientEndpointJson]) -> MartiResult {
    let body = serde_json::to_vec(&ApiResponse::new(kind::CLIENT_ENDPOINT, rows))?;
    let mut builder = HttpResponse::Ok();

    response::no_store(&mut builder).insert_header((CONTENT_TYPE, response::JSON));

    Ok(builder.body(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(uid: &str, callsign: &str) -> ClientEndpointJson {
        ClientEndpointJson {
            callsign: callsign.to_string(),
            uid: uid.to_string(),
            username: "ada".to_string(),
            team: "Cyan".to_string(),
            role: "Team Member".to_string(),
            last_status: CONNECTED,
            last_event_time: "2024-01-31T09:07:05.1Z".to_string(),
        }
    }

    #[test]
    fn a_contact_never_omits_a_key() {
        // node-tak's formatter calls `.trim()` on `notes` with no guard, so an
        // absent key is a crash inside the library rather than a blank column.
        let contact = ContactJson {
            filter_groups: Vec::new(),
            notes: String::new(),
            callsign: "ALPHA".to_string(),
            team: "Cyan".to_string(),
            role: "Team Member".to_string(),
            takv: "ATAK-v5.0".to_string(),
            uid: "UID-A".to_string(),
        };

        let json = serde_json::to_value(&contact).unwrap();

        for key in [
            "filterGroups",
            "notes",
            "callsign",
            "team",
            "role",
            "takv",
            "uid",
        ] {
            assert!(json.get(key).is_some(), "{key} is missing");
            assert!(!json[key].is_null(), "{key} is null");
        }
    }

    #[test]
    fn the_only_two_statuses_are_the_ones_ataks_enum_has() {
        // Anything else and ATAK discards the whole response, not the row.
        let json = serde_json::to_value(row("UID-A", "ALPHA")).unwrap();

        assert_eq!(json["lastStatus"], CONNECTED);
        assert!(["Connected", "Disconnected"].contains(&json["lastStatus"].as_str().unwrap()));
        assert!(
            json.get("groups").is_none(),
            "the model never serialises the channels it was filtered by",
        );
    }

    #[test]
    fn no_filter_matches_everybody_and_a_filter_matches_by_name() {
        let held = [GroupName::parse("Blue").unwrap()];

        assert!(matches(&held, None));
        assert!(matches(&held, Some(&["Blue".to_string()])));
        assert!(!matches(&held, Some(&["Red".to_string()])));
        assert!(
            !matches(&[], Some(&["Blue".to_string()])),
            "a client in no channels matches no filter",
        );
    }

    #[actix_web::test]
    async fn the_endpoint_listing_says_it_must_not_be_cached() {
        use actix_web::body::MessageBody as _;

        let response = render(&[row("UID-A", "ALPHA")]).unwrap();

        assert_eq!(
            response.headers().get("cache-control").unwrap(),
            "must-revalidate, max-age=0, no-cache, no-store",
        );
        assert_eq!(response.headers().get("expires").unwrap(), "0");
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json",
        );

        let body = response.into_body().try_into_bytes().unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(parsed["type"], kind::CLIENT_ENDPOINT);
        assert_eq!(parsed["data"][0]["uid"], "UID-A");
    }
}
