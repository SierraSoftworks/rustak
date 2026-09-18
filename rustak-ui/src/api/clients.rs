//! The devices connected to the stream listener right now.
//!
//! Administrative throughout: this list names every device on the
//! installation and the address each is connecting from. The participant's
//! view of the same question already exists as `/Marti/api/contacts/all`,
//! narrowed to what the caller can reach.
//!
//! # Disconnecting is not revoking
//!
//! `DELETE /clients/{uid}` closes the sockets; the device may reconnect with
//! the same certificate a moment later. Taking access away is
//! `POST /certificates/{id}/revoke`, which closes the connection as a
//! *consequence*. Two verbs, two outcomes, and the page says which is which.
//!
//! # An installation with no listener answers `[]`
//!
//! Nothing is connected, which is the true answer and one a page can render —
//! so [`list`] needs no special case. The two endpoints that *act* on a
//! connection answer `503` instead, because "there is no registry" and "that
//! uid is not connected" are different things to be told after clicking
//! Disconnect.
//!
//! [`status`] is how the page tells the empty list apart from the switched-off
//! one, which `[]` on its own cannot.

use rustak_api::{ClientHistoryEntry, ConnectedClient, IncognitoRequest, StreamStatus};

use crate::api::{ApiError, delete_empty, get_json, post_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// Everything connected, or an empty list on an installation with no stream
/// listener.
pub async fn list() -> Result<Vec<ConnectedClient>, ApiError> {
    demo!(Ok(fixtures::clients()));

    get_json("/clients").await
}

/// Whether there is a listener at all, and how many are on it.
///
/// `[]` from [`list`] means both "nobody is connected" and "there is no
/// listener", and those are a quiet exercise and a configuration problem.
pub async fn status() -> Result<StreamStatus, ApiError> {
    demo!(Ok(fixtures::stream_status()));

    get_json("/clients/status").await
}

/// Every device seen in the last `secago` seconds, connected or not.
pub async fn history(secago: i64) -> Result<Vec<ClientHistoryEntry>, ApiError> {
    demo!(Ok(fixtures::client_history(secago)));

    get_json(&format!("/clients/history?secago={secago}")).await
}

/// Closes every connection claiming this uid.
pub async fn disconnect(client_uid: &str) -> Result<(), ApiError> {
    demo!(fixtures::disconnect_client(client_uid));

    delete_empty(&format!("/clients/{}", urlencode(client_uid))).await
}

/// Hides a device from other clients' contact lists, or stops hiding it.
///
/// Set rather than toggled: `/Marti/api/subscriptions/incognito/{uid}` toggles
/// because the client asking is the one that knows what it is now. An
/// operator's page does not, so this says which way it should end up.
///
/// Answers the connection as it now is, so a caller that wants the resulting
/// row has it without re-reading the whole list.
pub async fn set_incognito(client_uid: &str, on: bool) -> Result<ConnectedClient, ApiError> {
    demo!(fixtures::set_incognito(client_uid, on));

    post_json(
        &format!("/clients/{}/incognito", urlencode(client_uid)),
        &IncognitoRequest { on },
    )
    .await
}
