//! Turning a verified client certificate into an identity.
//!
//! On `:8443` and on the stream port the certificate *is* the credential: the
//! handshake has already proved the device holds the private key, that the
//! chain reaches our authority, and — through
//! [`RustakClientVerifier`](crate::pki::RustakClientVerifier) — that the
//! fingerprint has not been revoked. What is left is the part the handshake
//! cannot do: tie the certificate to an account, and work out what that account
//! may currently see.
//!
//! # Everything is re-read per request
//!
//! The row, the account and the channel memberships are all read now rather
//! than carried on the connection. A certificate revoked, an account switched
//! off or a channel taken away has to take effect on the next request, not when
//! the client next reconnects — and on a long-lived mTLS connection that could
//! be hours.
//!
//! # Why the row must exist
//!
//! `[pki] require_known_cert` already makes the handshake refuse a certificate
//! with no row, but this path checks again rather than trusting that setting:
//! the answer to "which account is this?" comes from the row, so a certificate
//! without one has no identity to offer whatever the verifier decided.

use chrono::Utc;
use rustak_core::identity::{AuthMethod, Principal};
use rustak_core::prelude::*;

use crate::db::Database;
use crate::db::repos::{CertificateRow, UserRow};
use crate::identity::{members, users};
use crate::pki::PeerCertificate;
use crate::prelude::Services;

use super::resolve::{AuthFailure, Resolved};

/// Resolves the certificate a connection authenticated with.
///
/// # Errors
///
/// [`AuthFailure::Rejected`] when the certificate is not one we have a live
/// record of, names no account, or names one that has been switched off;
/// [`AuthFailure::Forbidden`] when the row belongs to a different account than
/// the common name claims, which is a mismatch an operator has to look at
/// rather than something a client can fix by reconnecting;
/// [`AuthFailure::Unavailable`] when a read fails.
#[instrument("auth.resolve.cert", skip_all, fields(fingerprint = %peer.fingerprint), err(Debug))]
pub async fn client_cert<S: Services>(
    services: &S,
    peer: &PeerCertificate,
) -> Result<Resolved, AuthFailure> {
    let db = services.db();
    let row = live_row(db, peer).await?;
    let user = account(db, &row, peer).await?;

    // Best-effort: knowing when a certificate was last used is what lets an
    // operator retire one nobody carries any more, and failing a request
    // because that write did not land would be the wrong trade.
    touch(db, &row, &user).await;

    let principal = principal(db, services, &user, &row, peer).await?;

    Ok(Resolved {
        principal,
        user,
        claims: None,
    })
}

/// The `certificates` row for this fingerprint, if it is still usable.
async fn live_row(db: &Database, peer: &PeerCertificate) -> Result<CertificateRow, AuthFailure> {
    let Some(row) = db
        .certificates()
        .get_by_fingerprint(&peer.fingerprint)
        .await?
    else {
        debug!("Refused a client certificate we have no record of.");

        return Err(AuthFailure::Rejected);
    };

    if !row.is_valid_at(Utc::now()) {
        debug!(
            revoked = row.revoked_at.is_some(),
            "Refused a client certificate that is revoked or outside its validity window.",
        );

        return Err(AuthFailure::Rejected);
    }

    Ok(row)
}

/// The account the row belongs to, checked against the name the subject claims.
async fn account(
    db: &Database,
    row: &CertificateRow,
    peer: &PeerCertificate,
) -> Result<UserRow, AuthFailure> {
    let Some(user_id) = row.user_id else {
        warn!("A client certificate with no account reached a request; it cannot be an identity.");

        return Err(AuthFailure::Rejected);
    };

    let Some(user) = db.users().get(user_id).await? else {
        return Err(AuthFailure::Rejected);
    };

    if user.disabled {
        debug!(username = %user.username, "Refused a certificate belonging to a disabled account.");

        return Err(AuthFailure::Rejected);
    }

    // We issue the subject ourselves, so a common name that disagrees with the
    // row means the row was rewritten or the certificate was not issued by the
    // path that records one. Either way it is not a request to serve.
    match peer.common_name.as_deref() {
        Some(name) if user.username.eq_ignore_case(name) => Ok(user),
        other => {
            warn!(
                subject = ?other,
                account = %user.username,
                "A client certificate's subject does not name the account it was issued to.",
            );

            Err(AuthFailure::Forbidden(
                "That certificate does not match the account it was issued to.",
            ))
        }
    }
}

/// Records that this certificate and this account were seen just now.
async fn touch(db: &Database, row: &CertificateRow, user: &UserRow) {
    if let Err(err) = db.certificates().touch_last_seen(row.id).await {
        debug!(error = %err, "Could not record when a client certificate was last used.");
    }

    if let Err(err) = db.users().touch_last_seen(user.id).await {
        debug!(error = %err, "Could not record when an account was last seen.");
    }
}

/// The rights the request carries.
///
/// The channel set comes from the **device** when the certificate names one, so
/// that a channel the device has switched off is not one it can send to; a
/// certificate with no device falls back to the account's own memberships.
async fn principal<S: Services>(
    db: &Database,
    services: &S,
    user: &UserRow,
    row: &CertificateRow,
    peer: &PeerCertificate,
) -> Result<Principal, AuthFailure> {
    let via = AuthMethod::ClientCert {
        fingerprint: peer.fingerprint.clone(),
        serial: peer.serial_hex.clone(),
    };

    let mut principal = users::principal(db, user, via, false).await?;

    let Some(device_id) = row.device_id else {
        return Ok(principal);
    };

    let anon_by_default = services.config().auth.anon_group_default;
    let groups = members::effective_for_device(db, user.id, device_id, anon_by_default).await?;

    principal = principal.with_groups(std::sync::Arc::new(groups));

    if let Some(device) = db.devices().get(device_id).await? {
        principal = principal.with_device(device.uid);
    }

    Ok(principal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::{CertificateRow, NewCertificate};
    use crate::pki::testing::TestAuthority;
    use crate::testing::TestServer;
    use rustak_api::{CertificateKind, CertificateSource};

    /// Records a certificate for `user`, exactly as enrolment would.
    async fn enrol(
        server: &TestServer,
        user: &UserRow,
        subject: &str,
    ) -> (PeerCertificate, CertificateRow) {
        let authority = TestAuthority::new().await;
        let client = authority.issue(subject);
        let peer = PeerCertificate::from_der(&client.der);

        let row = server
            .db()
            .certificates()
            .create(NewCertificate {
                kind: CertificateKind::Client,
                source: CertificateSource::Enrollment,
                issued_via: Some("enroll_v2_json".to_string()),
                serial_hex: peer.serial_hex.clone(),
                fingerprint: peer.fingerprint.clone(),
                subject_cn: subject.to_string(),
                san: Vec::new(),
                user_id: Some(user.id),
                device_id: None,
                client_uid: None,
                credential_id: None,
                issuer_id: None,
                der: peer.der.to_vec(),
                key_sealed: None,
                not_before: Utc::now() - chrono::Duration::minutes(5),
                not_after: Utc::now() + chrono::Duration::days(1),
            })
            .await
            .expect("record the certificate under test");

        (peer, row)
    }

    #[tokio::test]
    async fn an_enrolled_certificate_resolves_to_the_account_it_was_issued_to() {
        let server = TestServer::start().await;
        let user = server.user("ada", false).await;
        let (peer, _) = enrol(&server, &user, "ada").await;

        let resolved = client_cert(&server.context, &peer).await.unwrap();

        assert_eq!(resolved.user.id, user.id);
        assert!(matches!(
            resolved.principal.via,
            AuthMethod::ClientCert { .. }
        ));
        assert!(
            resolved.claims.is_none(),
            "a certificate carries no token claims to report",
        );
    }

    #[tokio::test]
    async fn a_certificate_we_have_no_record_of_has_no_identity() {
        let server = TestServer::start().await;
        let authority = TestAuthority::new().await;
        let peer = PeerCertificate::from_der(&authority.issue("ada").der);

        assert!(matches!(
            client_cert(&server.context, &peer).await,
            Err(AuthFailure::Rejected)
        ));
    }

    #[tokio::test]
    async fn a_revoked_certificate_stops_being_an_identity_at_once() {
        // The handshake refuses one too, but a connection opened before the
        // revocation is still open, and its next request must not be served.
        let server = TestServer::start().await;
        let user = server.user("ada", false).await;
        let (peer, row) = enrol(&server, &user, "ada").await;

        server
            .db()
            .certificates()
            .revoke(
                row.id,
                crate::db::repos::RevocationDetails {
                    reason: "test".to_string(),
                    by: None,
                },
            )
            .await
            .unwrap();

        assert!(matches!(
            client_cert(&server.context, &peer).await,
            Err(AuthFailure::Rejected)
        ));
    }

    #[tokio::test]
    async fn a_disabled_account_cannot_be_reached_through_its_certificate() {
        let server = TestServer::start().await;
        let user = server.user("ada", false).await;
        let (peer, _) = enrol(&server, &user, "ada").await;

        server
            .db()
            .users()
            .set_disabled(user.id, true)
            .await
            .unwrap();

        assert!(matches!(
            client_cert(&server.context, &peer).await,
            Err(AuthFailure::Rejected)
        ));
    }

    #[tokio::test]
    async fn a_subject_naming_somebody_else_is_a_refusal_rather_than_a_retry() {
        let server = TestServer::start().await;
        let user = server.user("ada", false).await;
        let (peer, _) = enrol(&server, &user, "grace").await;

        assert!(matches!(
            client_cert(&server.context, &peer).await,
            Err(AuthFailure::Forbidden(_))
        ));
    }

    #[tokio::test]
    async fn using_a_certificate_records_when_it_was_last_seen() {
        let server = TestServer::start().await;
        let user = server.user("ada", false).await;
        let (peer, _) = enrol(&server, &user, "ada").await;

        client_cert(&server.context, &peer).await.unwrap();

        let row = server
            .db()
            .certificates()
            .get_by_fingerprint(&peer.fingerprint)
            .await
            .unwrap()
            .unwrap();

        assert!(row.last_seen_at.is_some());
    }
}
