//! A typed client for the Marti API: missions, files, channels and contacts.
//!
//! The Marti API is TAK's, not ours, and it shows: every response is wrapped in
//! an envelope whose `type` is a Java class name, dates come in four formats,
//! and a handful of endpoints answer a bare array instead. A plugin should not
//! have to know any of that, so this module is where it stops.
//!
//! ```no_run
//! # async fn example(marti: &rustak_client::marti::MartiClient) -> Result<(), human_errors::Error> {
//! let missions = marti.missions().list(None).await?;
//!
//! for mission in &missions {
//!     println!("{} ({} items)", mission.name, mission.uids.len());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # What is typed, and what is not
//!
//! The fields a plugin acts on are typed; the ones TAK itself treats as opaque
//! — `externalData`, `feeds`, `mapLayers`, a mission layer's contents — stay
//! [`serde_json::Value`]. Nothing here uses `deny_unknown_fields`: the wire
//! shape is somebody else's and grows, and a client that refused a field it had
//! not heard of would break on a server upgrade it did not need to care about.
//!
//! # The envelope
//!
//! [`Envelope`] is the deserialising half of what `rustak-server`'s
//! `marti::response::ApiResponse` writes. `data` is [`Option`] because its
//! absence is meaningful: `GET {mission}/role` omits it for a caller who holds
//! no role, and the acknowledgement responses omit it always.

pub mod client;
pub mod contacts;
pub mod files;
pub mod groups;
pub mod mission_model;
pub mod missions;

use rustak_core::prelude::*;

pub use client::MartiClient;
pub use contacts::{ClientEndpoint, Contact, Contacts};
pub use files::{Files, Resource, SearchQuery, Uploaded};
pub use groups::{Channel, Groups};
pub use mission_model::{
    Mission, MissionChange, MissionCreate, MissionItem, MissionLog, MissionSubscription,
};
pub use missions::Missions;

/// The header a mission token is presented in.
///
/// Not `Authorization`: that one carries the caller's own identity, and TAK
/// reads a mission token from it only when nothing else did.
pub const MISSION_AUTHORIZATION: &str = "MissionAuthorization";

/// The header that asks for the current shape of a mission response.
pub const API_VERSION: &str = "API_VERSION";

/// The API version this client speaks.
///
/// `3` because a caller with no read role is then answered with a stripped `200`
/// rather than a `403`, which is the difference between "this mission is not for
/// you" and "something went wrong".
pub const API_VERSION_VALUE: &str = "3";

/// What every enveloped Marti response looks like.
///
/// The bound is written out because `#[serde(default)]` on `data` would
/// otherwise make the derive ask for `T: Default`, which a payload type has no
/// reason to be.
#[derive(Debug, Clone, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct Envelope<T> {
    /// `"3"` for everything a plugin calls. A string on the wire, not a number.
    #[serde(default)]
    pub version: String,

    /// The Java class name of what `data` holds, e.g. `Mission`.
    #[serde(rename = "type", default)]
    pub kind: String,

    /// The payload, absent for the acknowledgement responses.
    #[serde(default)]
    pub data: Option<T>,

    /// Anything the server wanted to say alongside it. Never set today.
    #[serde(default)]
    pub messages: Option<Vec<String>>,

    /// Which server answered.
    #[serde(rename = "nodeId", default)]
    pub node_id: String,
}

/// The body a Marti endpoint answers a failure with.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MartiErrorBody {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub message: String,
}

/// Turns a Marti refusal into an error an operator can act on.
///
/// `status` and the message are TAK's own words, which are usually the most
/// specific thing anybody has; the advice is ours.
pub(crate) fn refused(status: reqwest::StatusCode, body: &str, what: &str) -> Error {
    let detail = serde_json::from_str::<MartiErrorBody>(body)
        .ok()
        .filter(|parsed| !parsed.message.trim().is_empty())
        .map(|parsed| format!("{} ({})", parsed.message.trim(), parsed.status))
        .unwrap_or_else(|| first_line(body).unwrap_or_else(|| status.to_string()));

    let advice: &[&str] = match status.as_u16() {
        401 | 403 => &[
            "Check the client certificate under [service] — the Marti API authenticates with it.",
            "A service also has to be a member of the channel it is reading from.",
        ],
        404 => &["Check the name or uid in the request; the server does not hold it."],
        _ => &["The message above is the server's own."],
    };

    human_errors::user(
        format!(
            "Could not {what}: {detail}{}",
            crate::http::full_stop(&detail)
        ),
        advice,
    )
}

/// The first non-blank line of a body, for a server that answered text.
fn first_line(body: &str) -> Option<String> {
    let line = body.lines().map(str::trim).find(|line| !line.is_empty())?;

    Some(line.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_envelope_deserialises_with_or_without_its_payload() {
        let with: Envelope<Vec<String>> = serde_json::from_str(
            r#"{"version":"3","type":"Mission","data":["a"],"nodeId":"rustak-1"}"#,
        )
        .unwrap();
        let without: Envelope<Vec<String>> =
            serde_json::from_str(r#"{"version":"3","type":"Mission","nodeId":"rustak-1"}"#)
                .unwrap();

        assert_eq!(with.data.as_deref(), Some(["a".to_string()].as_slice()));
        assert_eq!(with.kind, "Mission");
        assert!(
            without.data.is_none(),
            "an absent payload is meaningful, not an empty one",
        );
    }

    #[test]
    fn a_refusal_carries_the_servers_own_words() {
        let err = refused(
            reqwest::StatusCode::NOT_FOUND,
            r#"{"status":"NOT_FOUND","code":1,"message":"Not Found: Mission OPS"}"#,
            "read the mission 'OPS'",
        );

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(
            err.description().contains("Not Found: Mission OPS"),
            "{err}"
        );
    }

    #[test]
    fn a_refusal_that_is_not_json_still_says_something() {
        let err = refused(
            reqwest::StatusCode::UNAUTHORIZED,
            "\n\nUnauthorized\n",
            "list missions",
        );

        assert!(err.description().contains("Unauthorized"), "{err}");
    }
}
