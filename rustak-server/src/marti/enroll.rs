//! Performing an enrolment: read the request, check it is the caller's, sign it.
//!
//! Split from [`super::tls`] by responsibility rather than by length: that file
//! is the *wire contract* — which paths exist, what `Accept` means, what the
//! documents look like — and this one is what the server actually does when
//! somebody enrols. The two signing endpoints differ only in how they render
//! what comes back, so everything up to the certificate lives here once.
//!
//! # The order matters
//!
//! Parse, check the common name, record the device, sign and record, and only
//! then spend a one-time token. A token spent before the certificate exists is
//! one somebody cannot enrol with and cannot get back; a certificate issued
//! before its row is written is one nothing can revoke — which is why
//! [`crate::pki::Pki::enroll`] writes the row itself rather than leaving it to
//! a caller who might not.

use actix_web::HttpRequest;
use rustak_core::identity::AuthMethod;

use crate::auth::resolve::Resolved;
use crate::db::repos::DeviceSeen;
use crate::identity::secret_cache::VerifiedSecretCache;
use crate::identity::{credentials, devices};
use crate::pki::{Enrollment, IssuedCert, IssuedVia, Pki, parse_csr};
use crate::prelude::*;
use crate::web::helpers::request::client_address;

use super::error::MartiError;

/// What an enrolment request said about itself.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignQuery {
    /// The device uid. Recorded, never validated as a shape: CloudTAK sends
    /// `"alice (ETL)"`, complete with the space and the parentheses.
    #[serde(default)]
    pub client_uid: Option<String>,

    /// The client's version string. Its value is never inspected; its
    /// *presence* is TAK's marker for a client that understands channels.
    #[serde(default)]
    pub version: Option<String>,
}

/// Issues a certificate, records the device, and spends a one-time token.
///
/// Shared by both signing endpoints, which differ only in how they render what
/// comes back.
#[allow(clippy::too_many_arguments)]
pub(super) async fn issue(
    request: &HttpRequest,
    context: &AppContext,
    pki: &Pki,
    resolved: &Resolved,
    query: &SignQuery,
    body: &[u8],
    issued_via: IssuedVia,
) -> Result<IssuedCert, MartiError> {
    let db = context.db();

    // Parsed here as well as inside `Pki::enroll` so that a request for
    // somebody else is a `403` — "not with that credential" — rather than the
    // `400` a malformed request gets. A client that cannot tell them apart
    // retries forever.
    let csr = parse_csr(body).map_err(|err| {
        debug!(error = %err, "Refused a signing request we could not read.");

        MartiError::InvalidRequest("the certificate signing request could not be read".to_string())
    })?;

    match csr.common_name.as_deref() {
        Some(name) if resolved.user.username.eq_ignore_case(name) => {}
        _ => {
            warn!(
                account = %resolved.user.username,
                "Refused a signing request naming a different user.",
            );

            return Err(MartiError::Forbidden(
                "that signing request is for a different user".to_string(),
            ));
        }
    }

    let device_id = match query.client_uid.as_deref().map(DeviceUid::from_storage) {
        Some(uid) => device(request, context, resolved, &uid).await,
        None => None,
    };

    let issued = pki
        .enroll(
            db,
            Enrollment {
                username: &resolved.user.username,
                csr_body: body,
                client_uid: query.client_uid.as_deref(),
                user_id: Some(resolved.user.id),
                device_id,
                credential_id: credential_of(resolved),
                issued_via,
                channels_capable: query.version.is_some(),
            },
        )
        .await
        .map_err(|err| enrolment_failed(context, &err))?;

    spend(context, resolved).await;

    Ok(issued)
}

/// Records the device the certificate belongs to.
///
/// A failure here is logged rather than fatal: the certificate is still the
/// caller's and refusing to issue it because a descriptive row would not write
/// would be the wrong trade.
async fn device(
    request: &HttpRequest,
    context: &AppContext,
    resolved: &Resolved,
    uid: &DeviceUid,
) -> Option<DeviceId> {
    let seen = DeviceSeen {
        // The version string, when there is one, is the client's own — recorded
        // as description, never trusted.
        last_ip: client_address(
            context.config().server.trust_proxy,
            request.headers(),
            request.peer_addr(),
        ),
        ..DeviceSeen::default()
    };

    match devices::upsert_seen(context.db(), uid, resolved.user.id, seen).await {
        Ok(row) => Some(row.id),
        Err(err) => {
            warn!(error = %err, device = %uid, "Could not record the device that enrolled.");

            None
        }
    }
}

/// The credential this request authenticated with, when it was a Basic one.
fn credential_of(resolved: &Resolved) -> Option<CredentialId> {
    credential_for(&resolved.principal)
}

/// As [`credential_of`], over the principal alone, so a test can build one.
fn credential_for(principal: &rustak_core::identity::Principal) -> Option<CredentialId> {
    match &principal.via {
        AuthMethod::Basic { credential_id, .. } => Some(*credential_id),
        _ => None,
    }
}

/// Spends a one-time enrolment token, now that it has bought something.
///
/// Anything other than a Basic credential has nothing to spend, and
/// [`credentials::record_use`] ignores a consuming call against a reusable one.
async fn spend(context: &AppContext, resolved: &Resolved) {
    let Some(id) = credential_of(resolved) else {
        return;
    };

    let db = context.db();

    match db.credentials().get(id).await {
        Ok(Some(row)) if row.kind.is_single_use() => {
            let cache = VerifiedSecretCache::shared();

            if let Err(err) = credentials::record_use(db, &row, true, cache).await {
                warn!(error = %err, "Could not spend the enrolment token a certificate was issued against.");
            }
        }
        Ok(_) => {}
        Err(err) => {
            warn!(error = %err, "Could not read the credential an enrolment was made with.")
        }
    }
}

/// Turns a refusal from the authority into the status it deserves.
fn enrolment_failed(context: &AppContext, err: &Error) -> MartiError {
    if err.is(human_errors::Kind::User) {
        debug!(error = %err, "Refused an enrolment.");

        return MartiError::InvalidRequest(err.description());
    }

    internal_error(context, err)
}

/// Reports one of our own failures and generalises it.
pub(super) fn internal_error(context: &AppContext, err: &Error) -> MartiError {
    error!(error = %err, "An enrolment could not be completed.");
    context.session().record_human_error(err);

    MartiError::Internal("enrollment".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_api::CredentialKind;
    use rustak_core::identity::{Principal, PrincipalKind};

    /// A resolved caller whose credential was `via`.
    fn caller(via: AuthMethod) -> Principal {
        Principal::new(
            UserId::new(1),
            Username::parse("ada").unwrap(),
            PrincipalKind::Person,
            via,
        )
    }

    #[test]
    fn only_a_basic_credential_is_one_an_enrolment_can_spend() {
        // A bearer token and a client certificate both authenticate an
        // enrolment; neither is a one-time token with a use to consume.
        let basic = caller(AuthMethod::Basic {
            credential_id: CredentialId::new(7),
            kind: CredentialKind::EnrollmentToken,
        });
        let bearer = caller(AuthMethod::Bearer {
            jti: "a-token".to_string(),
            scope: "api".to_string(),
        });
        let cert = caller(AuthMethod::ClientCert {
            fingerprint: "ab".to_string(),
            serial: "01".to_string(),
        });

        assert_eq!(credential_for(&basic), Some(CredentialId::new(7)));
        assert_eq!(credential_for(&bearer), None);
        assert_eq!(credential_for(&cert), None);
    }

    #[test]
    fn a_query_with_nothing_in_it_still_parses() {
        // ATAK sends both parameters, CloudTAK sends both, and a hand-made
        // request may send neither; none of the three is an error.
        let query = actix_web::web::Query::<SignQuery>::from_query("").unwrap();

        assert_eq!(query.client_uid, None);
        assert_eq!(query.version, None);

        let query =
            actix_web::web::Query::<SignQuery>::from_query("clientUid=ada%20(ETL)&version=3")
                .unwrap();

        assert_eq!(query.client_uid.as_deref(), Some("ada (ETL)"));
        assert_eq!(query.version.as_deref(), Some("3"));
    }
}
