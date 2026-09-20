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
//! Parse, check the common name, **claim** a one-time token, record the device,
//! sign and record — and put the claim back if the signing then failed.
//!
//! Spending afterwards was the obvious order and the wrong one: usability was
//! checked in one read and the spend was a later, unconditional write, so two
//! concurrent posts of the same enrolment token both passed and both received a
//! certificate, and a spend that failed left the token live (R-01 M3). The
//! consumption is now the gate — one conditional `UPDATE` that reports whether
//! this caller is the one that got it — and the compensating release is what
//! keeps a token spent for a certificate that was never signed from being lost.
//! A certificate issued before its row is written is one nothing can revoke,
//! which is why [`crate::pki::Pki::enroll`] writes the row itself rather than
//! leaving it to a caller who might not.

use std::sync::Arc;

use actix_web::HttpRequest;
use rustak_api::{AuditCategory, AuditOutcome};
use rustak_core::identity::AuthMethod;

use crate::auth::resolve::Resolved;
use crate::auth::workload::Assertion;
use crate::db::repos::{CredentialRow, DeviceSeen};
use crate::db::{AuditEntry, AuditStore as _};
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

    // Before anything is signed: the claim is what decides whether this caller
    // gets a certificate at all. The device uid goes in with it, because the
    // grace window that follows (`identity::verify::Grace`) answers to that
    // device and no other.
    let claimed = claim(context, resolved, query.client_uid.as_deref()).await?;

    // What the certificate says it came from is the *credential*, not the
    // representation, when the credential is a workload identity: `issued_via`
    // is what `revoke_previous` matches on, and a supersede that had to know
    // whether this sidecar asked for JSON or XML would be a supersede that
    // missed one.
    let issued_via = match assertion(request) {
        Some(_) => IssuedVia::WorkloadIdentity,
        None => issued_via,
    };

    // One fallible block, so that *every* way of failing after the claim — a
    // device uid that is somebody else's, a signature that will not be made —
    // goes through the release below rather than only the ones a `?` here
    // happened to cover.
    let issued: Result<IssuedCert, MartiError> = async {
        let device_id = match query.client_uid.as_deref().map(DeviceUid::from_storage) {
            Some(uid) => device(request, context, resolved, &uid).await?,
            None => None,
        };

        pki.enroll(
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
        .map_err(|err| enrolment_failed(context, &err))
    }
    .await;

    match issued {
        Ok(issued) => {
            record_reusable_use(context, resolved).await;
            workload_enrolled(request, context, pki, resolved, &issued).await;

            Ok(issued)
        }
        Err(refusal) => {
            release(context, claimed.as_ref()).await;

            Err(refusal)
        }
    }
}

/// The workload identity this request enrolled with, when it enrolled with one.
///
/// Read off the request rather than off the principal, because the principal
/// carries the issuer and the subject and the audit trail wants the namespace
/// and the `jti` as well — see [`crate::auth::workload::Assertion`].
pub(super) fn assertion(request: &HttpRequest) -> Option<Arc<Assertion>> {
    crate::auth::workload::assertion_of(request)
}

/// Audits an enrolment made with a workload identity, and supersedes the
/// certificates the same account already held from one.
///
/// Neither half is fatal. The certificate has been issued and recorded; a
/// failure to write the audit entry, or to take back a certificate on a node
/// that may not even exist any more, is something to report rather than
/// something to undo a working enrolment for.
async fn workload_enrolled(
    request: &HttpRequest,
    context: &AppContext,
    pki: &Pki,
    resolved: &Resolved,
    issued: &IssuedCert,
) {
    let Some(assertion) = assertion(request) else {
        return;
    };

    let db = context.db();
    let entry = AuditEntry::new(
        AuditCategory::Enrollment,
        "enrollment.workload",
        AuditOutcome::Success,
    )
    .subject(&resolved.user.username)
    .actor(&resolved.user.username)
    .message(format!(
        "'{}' in namespace '{}' enrolled '{}' with its {} workload identity.",
        assertion.subject, assertion.namespace, resolved.user.username, assertion.issuer_name,
    ))
    // The token itself never appears here; see `Assertion::audit_detail`.
    .detail(merge(
        assertion.audit_detail(),
        serde_json::json!({
            "fingerprint": issued.fingerprint,
            "serial": issued.serial_hex,
        }),
    ));

    if let Err(err) = db.record(entry).await {
        warn!(error = %err, "Could not record a workload enrolment in the audit log.");
    }

    if !context.config().auth.workload.revoke_previous {
        return;
    }

    match crate::pki::supersede_workload(
        db,
        pki.revocations(),
        resolved.user.id,
        &issued.fingerprint,
        Some(&resolved.user.username),
    )
    .await
    {
        Ok(superseded) if !superseded.is_empty() => info!(
            account = %resolved.user.username,
            count = superseded.len(),
            "Took back the workload certificates this account held before.",
        ),
        Ok(_) => {}
        Err(err) => warn!(
            error = %err,
            account = %resolved.user.username,
            "Could not take back the workload certificates this account held before.",
        ),
    }
}

/// Folds two JSON objects into one, for the audit detail above.
fn merge(mut base: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
    let (Some(base_map), Some(extra_map)) = (base.as_object_mut(), extra.as_object()) else {
        return base;
    };

    for (key, value) in extra_map {
        base_map.insert(key.clone(), value.clone());
    }

    base
}

/// Records the device the certificate belongs to.
///
/// A `clientUid` that belongs to a **different** account is a `403` and stops
/// the enrolment: the uid is public, and letting it change hands hands the
/// original owner's per-device channel state to whoever asked last (R-01 M4).
/// Any other failure is logged rather than fatal — the certificate is still the
/// caller's, and refusing to issue it because a descriptive row would not write
/// would be the wrong trade.
async fn device(
    request: &HttpRequest,
    context: &AppContext,
    resolved: &Resolved,
    uid: &DeviceUid,
) -> Result<Option<DeviceId>, MartiError> {
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
        Ok(row) => Ok(Some(row.id)),
        Err(err) if err.is(human_errors::Kind::User) => {
            Err(MartiError::Forbidden(err.description()))
        }
        Err(err) => {
            warn!(error = %err, device = %uid, "Could not record the device that enrolled.");

            Ok(None)
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

/// Spends a one-time enrolment token, before it has bought anything.
///
/// Answers the row that was claimed, so a failed issuance can put it back.
/// Anything other than a Basic credential has nothing to claim, and a reusable
/// one is recorded after the fact by [`record_reusable_use`] instead.
///
/// The spend records `client_uid` beside it: ATAK fetches its enrolment profile
/// with this same token moments later, and the grace window that allows it is
/// scoped to the device that spent it (M2-15).
///
/// # Errors
///
/// [`MartiError::Forbidden`] when the token has already been spent — including
/// by the request racing this one — and [`MartiError::Internal`] when the write
/// fails, which fails closed rather than issuing against a token we could not
/// consume.
async fn claim(
    context: &AppContext,
    resolved: &Resolved,
    client_uid: Option<&str>,
) -> Result<Option<CredentialRow>, MartiError> {
    let Some(id) = credential_of(resolved) else {
        return Ok(None);
    };

    let db = context.db();

    let row = match db.credentials().get(id).await {
        Ok(Some(row)) if row.kind.is_single_use() => row,
        Ok(_) => return Ok(None),
        Err(err) => return Err(internal_error(context, &err)),
    };

    match credentials::claim_single_use(db, &row, client_uid, VerifiedSecretCache::shared()).await {
        Ok(true) => Ok(Some(row)),
        Ok(false) => {
            warn!(credential = %id, "Refused an enrolment against a token that was already spent.");

            Err(MartiError::Forbidden(
                "that enrolment token has already been used".to_string(),
            ))
        }
        Err(err) => Err(internal_error(context, &err)),
    }
}

/// Puts a claim back when the issuance it was made for failed.
async fn release(context: &AppContext, claimed: Option<&CredentialRow>) {
    let Some(row) = claimed else {
        return;
    };

    if let Err(err) =
        credentials::release_single_use(context.db(), row, VerifiedSecretCache::shared()).await
    {
        warn!(
            error = %err,
            credential = %row.id,
            "Could not put back an enrolment token whose certificate was never issued.",
        );
    }
}

/// Records the use of a **reusable** credential after it has bought something.
///
/// A client password is reusable by design, so recording its use is what lets
/// an administrator see it being used rather than what stops it being used
/// again.
async fn record_reusable_use(context: &AppContext, resolved: &Resolved) {
    let Some(id) = credential_of(resolved) else {
        return;
    };

    let db = context.db();

    match db.credentials().get(id).await {
        Ok(Some(row)) if !row.kind.is_single_use() => {
            if let Err(err) =
                credentials::record_use(db, &row, false, VerifiedSecretCache::shared()).await
            {
                debug!(error = %err, "Could not record the use of an enrolment credential.");
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
