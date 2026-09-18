//! `/Marti/api/groups/*` and `/Marti/api/users/*` — the channels surface.
//!
//! The endpoints ATAK's Channels UI and CloudTAK's channel filter are both
//! built on. The shapes live in [`channels`]; this file is the
//! routes, the query parameters and the one deliberately lenient body parser.
//!
//! # `created` is a date going out and a number coming in
//!
//! `GET …/all` renders `created` as `yyyy-MM-dd` and `PUT …/active` receives the
//! same field as epoch milliseconds, because that is what ATAK's own writer
//! emits. This is not a documentation error and it is not worth normalising:
//! the field is ignored on the way in (nothing about a channel's creation date
//! is the client's to change), so the asymmetry costs nothing beyond being
//! written down. `compat/groups.md` §"Gotchas" is the source.
//!
//! # An element we cannot read is dropped, not refused
//!
//! ATAK sends back the whole list it was given, including channels that have
//! been deleted since. Refusing the request over one of them would break every
//! client still holding a stale cache, so an entry naming a channel that is not
//! here — or one with no usable `name`/`direction` — is skipped and the rest is
//! applied (`compat/groups.md` §2).
//!
//! # Who is told, and what they do about it
//!
//! A change makes every one of the account's live stream connections
//! re-authenticate against the new selection, and sends `t-x-g-c` to its other
//! devices. Design 04 D9 departs from TAK Server here: TAK sends nothing at all
//! when `clientUid` is absent, and CloudTAK never sends one — so a channel
//! toggled from a browser would never reach the phone that is looking at the
//! map. Both clients answer the notice by discarding this server's map items and
//! re-fetching `…/all?sendLatestSA=true`, which is why the replay this file
//! serves has to be computed after the re-authentication rather than before it.

use actix_web::web;
use rustak_api::{ActiveGroup, Direction, GroupName};

use crate::identity::members;
use crate::prelude::*;

use super::channels::{self, GroupJson};
use super::error::{MartiError, MartiResult};
use super::extract::CiQuery;
use super::principal::MartiPrincipal;
use super::{kind, response, time};

/// What TAK Server reports for a connection it did not federate in.
const CONNECTION_TYPE: &str = "CORE";

/// One account, as `/Marti/api/users/all` describes it.
///
/// The shape is unverified beyond the fully-qualified class name the envelope
/// carries — neither ATAK nor CloudTAK reads this endpoint — so the fields are
/// the four TAK Server's model has and nothing has been invented beside them.
#[derive(Debug, Clone, Serialize)]
struct UserJson {
    id: String,
    name: String,
    #[serde(rename = "connectionType")]
    connection_type: &'static str,
    created: String,
}

/// `GET /Marti/api/groups/all?useCache&sendLatestSA`.
///
/// `useCache` is accepted and ignored (design 04 D8): rustak always answers with
/// both directions and the caller's own active flags, which is a superset of
/// what TAK Server returns for either value, and both clients call with `true`.
///
/// # Errors
///
/// [`MartiError::Unauthorized`] for an anonymous caller and
/// [`MartiError::Internal`] when a read fails.
pub async fn all(
    who: MartiPrincipal,
    context: web::Data<AppContext>,
    query: CiQuery,
) -> MartiResult {
    let resolved = who.require()?;
    let groups = channels::visible(&context, resolved).await?;

    if query.flag("sendLatestSA").get() {
        resend(&context, &resolved.user.username);
    }

    Ok(response::ok(kind::GROUP, groups))
}

/// `PUT /Marti/api/groups/active?clientUid=` — the caller's new selection.
///
/// The body is a bare JSON array, never an envelope, and the response is an
/// empty `200` whatever was applied. A selection leaving the caller in no
/// channels at all is accepted: TAK Server's refusal is behind a setting that
/// defaults off, and a client that has switched everything off has said so.
///
/// # Errors
///
/// [`MartiError::Unauthorized`] for an anonymous caller,
/// [`MartiError::InvalidRequest`] for a body that is not a JSON array, and
/// [`MartiError::Internal`] when a write fails.
pub async fn set_active(
    who: MartiPrincipal,
    context: web::Data<AppContext>,
    query: CiQuery,
    body: web::Bytes,
) -> MartiResult {
    // Before the body is looked at: an anonymous caller has nothing to apply,
    // and parsing an unauthenticated body is work somebody else asked for.
    who.require()?;

    let states = states(&body)?;

    apply(&who, &context, &query, &states).await
}

/// `PUT /Marti/api/groups/activebits?clientUid=`.
///
/// A body of bit positions rather than channels: everything listed is on and
/// everything the caller holds and did not list is off. Neither ATAK nor
/// CloudTAK sends this; it is here because it is the one shorthand TAK Server
/// offers and a third-party client may have been written against it.
///
/// # Errors
///
/// As [`set_active`].
pub async fn set_active_bits(
    who: MartiPrincipal,
    context: web::Data<AppContext>,
    query: CiQuery,
    body: web::Bytes,
) -> MartiResult {
    let resolved = who.require()?;
    let wanted: Vec<u32> = serde_json::from_slice(&body)?;
    let states: Vec<ActiveGroup> = channels::visible(&context, resolved)
        .await?
        .iter()
        .filter_map(|view| {
            Some(ActiveGroup {
                group: GroupName::parse(&view.name).ok()?,
                direction: Direction::parse(view.direction)?,
                active: wanted.contains(&view.bitpos),
            })
        })
        .collect();

    apply(&who, &context, &query, &states).await
}

/// `GET /Marti/api/groups/groupCacheEnabled`.
///
/// Always `true`: rustak keeps the selection server-side, which is what the
/// question asks. CloudTAK never calls it; ATAK does, before deciding whether
/// to send its cached list back.
///
/// # Errors
///
/// Never.
pub async fn cache_enabled() -> MartiResult {
    Ok(response::ok(kind::BOOLEAN, true))
}

/// `GET /Marti/api/groups/{name}/{direction}`.
///
/// # Errors
///
/// [`MartiError::Unauthorized`] for an anonymous caller,
/// [`MartiError::InvalidRequest`] for a direction that is not `IN` or `OUT`,
/// [`MartiError::NotFound`] when the caller does not hold it, and
/// [`MartiError::Internal`] when a read fails.
pub async fn one(
    who: MartiPrincipal,
    context: web::Data<AppContext>,
    path: web::Path<(String, String)>,
) -> MartiResult {
    let resolved = who.require()?;
    let (name, direction) = path.into_inner();

    let direction = match Direction::parse(&direction) {
        Some(single @ (Direction::In | Direction::Out)) => single,
        _ => {
            return Err(MartiError::InvalidRequest(format!("direction={direction}")));
        }
    };

    let found = channels::visible(&context, resolved)
        .await?
        .into_iter()
        .find(|view| view.name == name && view.direction == direction.as_str());

    match found {
        Some(view) => Ok(response::ok(kind::GROUP, view)),
        // The envelope with no `data`, rather than the error shape: this is the
        // one groups route that answers a miss with a payload-shaped body, and
        // ATAK reads `data` being absent as "you do not hold it".
        None => Ok(response::status(
            actix_web::http::StatusCode::NOT_FOUND,
            kind::GROUP,
            None::<GroupJson>,
        )),
    }
}

/// `GET /Marti/api/groups/user?username=` — somebody else's channels.
///
/// # Errors
///
/// [`MartiError::Unauthorized`]/[`MartiError::Forbidden`] for a caller who does
/// not administer this installation, [`MartiError::InvalidRequest`] when
/// `username` is missing or unusable, [`MartiError::NotFound`] when there is no
/// such account, and [`MartiError::Internal`] when a read fails.
pub async fn for_user(
    who: MartiPrincipal,
    context: web::Data<AppContext>,
    query: CiQuery,
) -> MartiResult {
    who.require_admin()?;

    let username = query
        .get("username")
        .and_then(|value| Username::parse(value).ok())
        .ok_or_else(|| MartiError::InvalidRequest("username is required".to_string()))?;

    let user = context
        .db()
        .users()
        .get_by_username(&username)
        .await?
        .ok_or_else(|| MartiError::NotFound(username.to_string()))?;

    Ok(response::ok(
        kind::GROUP,
        channels::visible_to(&context, user.id).await?,
    ))
}

/// `GET /Marti/api/users/all` — every account, newest first.
///
/// # Errors
///
/// [`MartiError::Unauthorized`]/[`MartiError::Forbidden`] for a caller who does
/// not administer this installation, and [`MartiError::Internal`] when the read
/// fails.
pub async fn users_all(who: MartiPrincipal, context: web::Data<AppContext>) -> MartiResult {
    who.require_admin()?;

    let mut rows = context
        .db()
        .users()
        .list(crate::db::repos::Page::first(1000))
        .await?;

    rows.sort_by_key(|row| std::cmp::Reverse(row.created_at));

    let listed: Vec<UserJson> = rows
        .iter()
        .map(|row| UserJson {
            id: row.username.to_string(),
            name: row.username.to_string(),
            connection_type: CONNECTION_TYPE,
            created: time::cot_date(row.created_at),
        })
        .collect();

    Ok(response::ok(kind::USER, listed))
}

/// Writes a selection, re-authenticates what is connected, and notifies.
async fn apply(
    who: &MartiPrincipal,
    context: &AppContext,
    query: &CiQuery,
    states: &[ActiveGroup],
) -> MartiResult {
    let resolved = who.require()?;
    let client_uid = query.get("clientUid").map(str::to_string);

    channels::apply(context, resolved.user.id, states, client_uid.as_deref()).await?;

    members::channels_changed(
        context,
        resolved.user.id,
        &resolved.user.username,
        client_uid.as_deref(),
    )
    .await;

    // After the re-authentication above, so that what a client is replayed is
    // what its new selection can see rather than what its old one could.
    if query.flag("sendLatestSA").get() {
        resend(context, &resolved.user.username);
    }

    Ok(response::text(actix_web::http::StatusCode::OK, ""))
}

/// Pushes every reachable peer's latest position at the caller's devices.
///
/// A side effect on the stream, not on this response — and silently nothing on
/// an installation with no stream listener, where the caller has no connection
/// for it to arrive on.
fn resend(context: &AppContext, username: &Username) {
    if !context.has_live() {
        return;
    }

    match context.live() {
        Ok(live) => {
            let sent = live.resend_latest_sa(username);

            debug!(user = %username, events = sent, "Replayed the map after a channel change.");
        }
        Err(err) => warn!(error = %err, "Could not reach the live connections to replay a map."),
    }
}

/// Reads the bare array `PUT …/active` carries.
///
/// Deliberately forgiving, per design 04 §3.1: `name`, `direction` and `active`
/// are read and everything else — `created` in either spelling, `type`,
/// `bitpos`, `distinguishedName` — is ignored. An entry missing a usable name or
/// direction is dropped rather than refused.
fn states(body: &[u8]) -> Result<Vec<ActiveGroup>, MartiError> {
    let parsed: serde_json::Value = serde_json::from_slice(body)?;

    let serde_json::Value::Array(items) = parsed else {
        return Err(MartiError::InvalidRequest(
            "a JSON array of channels is required".to_string(),
        ));
    };

    Ok(items.iter().filter_map(state).collect())
}

/// One element of that array, when it is one we can read.
fn state(item: &serde_json::Value) -> Option<ActiveGroup> {
    let group = GroupName::parse(item.get("name")?.as_str()?).ok()?;
    let direction = Direction::parse(item.get("direction")?.as_str()?)?;

    Some(ActiveGroup {
        group,
        direction,
        // Absent means on: a client that sent a channel and said nothing about
        // it is listing it, not switching it off.
        active: item
            .get("active")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_ataks_writer_produced_is_read_whole() {
        // `created` arrives as epoch milliseconds here and as `yyyy-MM-dd` on
        // the way out. Both are real; neither is worth normalising.
        let body = br#"[
            {"name":"Blue","direction":"IN","created":1706691425000,
             "type":"SYSTEM","bitpos":2,"active":false},
            {"name":"Blue","direction":"OUT","created":1706691425000,
             "type":"SYSTEM","bitpos":2,"active":true}
        ]"#;

        let states = states(body).unwrap();

        assert_eq!(states.len(), 2);
        assert_eq!(states[0].group.as_str(), "Blue");
        assert_eq!(states[0].direction, Direction::In);
        assert!(!states[0].active);
        assert!(states[1].active);
    }

    #[test]
    fn an_entry_we_cannot_read_is_dropped_and_the_rest_applied() {
        // ATAK sends back the list it was given, deleted channels and all.
        let body = br#"[
            {"direction":"IN","active":true},
            {"name":"Blue","direction":"sideways","active":true},
            {"name":"Red","direction":"OUT","active":false}
        ]"#;

        let states = states(body).unwrap();

        assert_eq!(states.len(), 1);
        assert_eq!(states[0].group.as_str(), "Red");
    }

    #[test]
    fn an_element_with_no_active_flag_is_listed_rather_than_switched_off() {
        let states = states(br#"[{"name":"Blue","direction":"IN"}]"#).unwrap();

        assert!(states[0].active);
    }

    #[test]
    fn an_envelope_where_a_bare_array_belongs_is_refused() {
        // The mistake a client makes once: every other list endpoint in this
        // family is wrapped, and this one deliberately is not.
        let refused = states(br#"{"version":"3","data":[]}"#).unwrap_err();

        assert_eq!(refused.status().as_u16(), 400);
    }

    #[test]
    fn a_body_that_is_not_json_at_all_is_refused() {
        assert_eq!(states(b"not json").unwrap_err().status().as_u16(), 400);
    }

    #[test]
    fn a_direction_is_read_in_whatever_case_it_arrives() {
        let states = states(br#"[{"name":"Blue","direction":"out","active":true}]"#).unwrap();

        assert_eq!(states[0].direction, Direction::Out);
    }
}
