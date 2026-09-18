//! The secrets a client presents: enrolment tokens, client passwords and
//! service tokens.
//!
//! Self-service throughout. Asking for nobody in particular lists — and mints
//! for — the signed-in account; naming somebody is the administrative form, and
//! the server refuses it for anybody who is not one.
//!
//! # The secret exists in one response
//!
//! [`create`] is the only call that returns one. Nothing here can ask for it
//! again, because only its hash was kept, so the page that receives a
//! [`CredentialCreated`] is the last place it exists outside the client that
//! will use it.

use rustak_api::{
    CreateCredentialRequest, Credential, CredentialCreated, CredentialId, EnrollTemplate, Username,
};

use crate::api::{ApiError, delete_empty, get_json, post_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// The credentials held by `username`, or by the signed-in account when that is
/// absent.
pub async fn list(
    username: Option<&Username>,
    include_revoked: bool,
) -> Result<Vec<Credential>, ApiError> {
    demo!(Ok(fixtures::credentials(username, include_revoked)));

    let mut path = String::from("/credentials?");
    if let Some(username) = username {
        path.push_str(&format!("username={}&", urlencode(username.as_str())));
    }
    path.push_str(&format!("include_revoked={include_revoked}"));

    get_json(&path).await
}

/// Mints one, which is the only time its secret is ever returned.
pub async fn create(request: &CreateCredentialRequest) -> Result<CredentialCreated, ApiError> {
    demo!(fixtures::mint_credential(request));

    post_json("/credentials", request).await
}

/// Revokes one, taking with it the certificates that were issued against it.
pub async fn revoke(id: CredentialId) -> Result<(), ApiError> {
    demo!(fixtures::revoke_credential(id));

    delete_empty(&format!("/credentials/{id}")).await
}

/// The host and username an enrolment token belongs to, with `{token}` still
/// unfilled — because a server that could re-emit a working link would be a
/// server that had kept the secret.
#[allow(dead_code)]
pub async fn enroll_template(id: CredentialId) -> Result<EnrollTemplate, ApiError> {
    demo!(fixtures::enroll_template(id));

    get_json(&format!("/credentials/{id}/enroll-url")).await
}
