//! Minting, recording and taking back the secrets a client presents.
//!
//! Three kinds and no others ([`CredentialKind`]): a one-time enrolment token,
//! an opt-in expiring client password for what can do nothing better, and a
//! service token for a sidecar. Checking one that has been presented is
//! [`super::verify`]; this module is the lifecycle around it.
//!
//! # A secret exists once
//!
//! [`mint`] generates it, hashes it with argon2id, stores the hash and hands
//! the secret back in a [`MintedSecret`]. Nothing here stores, logs or audits
//! the secret itself: the audit entry names the credential, and
//! [`rustak_core::identity::Secret`] redacts itself in every formatting trait,
//! so a stray `{:?}` in a handler cannot put one in a log file. That is also
//! why [`enroll_url_template`] exists beside [`enroll_url`] — after the minting
//! response there is nothing left to compose a working link from.

use chrono::{DateTime, Utc};
use rustak_api::{Credential, CredentialId, CredentialKind, ENROLL_URL};
use rustak_core::identity::{
    DEFAULT_PASSWORD_GROUPS, DEFAULT_TOKEN_BYTES, Secret, generate_password, generate_token,
    hash_blocking, lookup_hint,
};
use rustak_core::prelude::*;

use crate::config::AuthConfig;
use crate::db::{
    Database,
    repos::{CredentialRow, NewCredential, RevocationDetails, UserRow},
};

use super::secret_cache::VerifiedSecretCache;

/// The prefix a service token carries, so that one found in a configuration
/// file or a log is recognisable for what it is.
const SERVICE_TOKEN_PREFIX: &str = "rsk_";

/// Bytes of entropy behind a service token.
const SERVICE_TOKEN_BYTES: usize = 30;

/// What [`mint`] returns: the credential, and the only copy of its secret.
#[derive(Debug)]
pub struct MintedSecret {
    pub credential: CredentialRow,
    /// Shown once. [`Secret`] redacts itself everywhere it could be printed.
    pub secret: Secret,
}

/// What to mint, for whom, and on whose say-so.
#[derive(Debug, Clone)]
pub struct MintRequest<'a> {
    pub kind: CredentialKind,

    /// What its owner will recognise it by, usually the name of the device it
    /// is for.
    pub label: &'a str,

    /// How long it should last. [`None`] means the default for its kind.
    pub expires_in: Option<chrono::Duration>,

    /// How many times it may be used. [`None`] means the default for its kind,
    /// and an enrolment token is one-time whatever this says.
    pub max_uses: Option<u32>,

    /// Who is minting it, which is not always its owner: an administrator can
    /// mint an enrolment token on somebody's behalf.
    pub actor: &'a Username,
}

impl<'a> MintRequest<'a> {
    /// A credential with the defaults its kind implies.
    pub fn new(kind: CredentialKind, label: &'a str, actor: &'a Username) -> Self {
        Self {
            kind,
            label,
            expires_in: None,
            max_uses: None,
            actor,
        }
    }
}

/// Mints a credential and hands back its secret.
///
/// `expires_in` and `max_uses` default to what the kind means: an enrolment
/// token lasts `[auth] enrollment_token_ttl` and is spent once, a client
/// password lasts `[auth] client_password_ttl`, and a service token lasts until
/// it is revoked. An enrolment token is always one-time whatever the caller
/// asked for, because "one-time" is the whole of its security argument.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when client passwords are switched off
/// and one is asked for, or when the label is empty; a
/// [`human_errors::Kind::System`] error if hashing or the write fails.
#[instrument("identity.credentials.mint", skip_all, fields(user = %user.username, kind = request.kind.as_str()), err(Display))]
pub async fn mint(
    db: &Database,
    config: &AuthConfig,
    user: &UserRow,
    request: MintRequest<'_>,
) -> Result<MintedSecret, Error> {
    let MintRequest {
        kind,
        label,
        expires_in,
        max_uses,
        actor,
    } = request;

    if kind == CredentialKind::ClientPassword && !config.client_passwords_enabled {
        return Err(human_errors::user(
            "Client passwords are switched off on this installation.",
            &[
                "Mint an enrolment token instead — it is what every current client should use.",
                "Set 'client_passwords_enabled = true' under [auth] if a client genuinely cannot enrol.",
            ],
        ));
    }

    let label = label.trim();
    if label.is_empty() {
        return Err(human_errors::user(
            "A credential needs a label so you can tell it from the others.",
            &["Name it after the device it is for."],
        ));
    }

    let secret = generate(kind);
    let hint = lookup_hint(secret.expose());
    let secret_hash = hash_blocking(secret.clone()).await?;

    let credential = db
        .credentials()
        .create(NewCredential {
            user_id: user.id,
            kind,
            label: label.to_string(),
            secret_hash,
            lookup_hint: hint,
            expires_at: expires_at(config, kind, expires_in),
            max_uses: uses_allowed(kind, max_uses),
            created_by: Some(actor.clone()),
        })
        .await?;

    info!(
        credential = %credential.id,
        kind = kind.as_str(),
        "Minted a credential.",
    );

    Ok(MintedSecret { credential, secret })
}

/// Records a successful use, spending the credential when `consumed`.
///
/// `consumed` is true only where the use is meant to be terminal: an enrolment
/// token that has just produced a certificate. A client password's use count
/// rises without ever exhausting it, which is what `consumed = false` is for.
///
/// # Why a one-time token cannot be spent by accident
///
/// The use count is what exhausts a credential, so an ordinary recorded use on
/// a one-time token would spend it — and a token spent by the `tls/config` call
/// that precedes `signClient` strands the person halfway through enrolling. The
/// call is therefore ignored rather than honoured.
///
/// It is logged at `debug` rather than `warn`: every `tls/config` call used to
/// produce one, which is the expected path rather than a mistake (M2-15), and a
/// warning on the ordinary path is a warning an operator learns to skip.
/// [`super::verify`]'s caller no longer makes the call at all for a single-use
/// credential, so what is left here is the guard rather than the noise.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the write fails.
pub async fn record_use(
    db: &Database,
    credential: &CredentialRow,
    consumed: bool,
    cache: &VerifiedSecretCache,
) -> Result<(), Error> {
    if !consumed && credential.kind.is_single_use() {
        debug!(
            credential = %credential.id,
            kind = credential.kind.as_str(),
            "Ignored an ordinary use recorded against a one-time credential.",
        );

        return Ok(());
    }

    db.credentials().record_use(credential.id, consumed).await?;

    if consumed {
        cache.forget(credential.id);
    }

    Ok(())
}

/// Claims a one-time credential before it has bought anything.
///
/// The gate and the spend are the same write (see
/// [`CredentialsRepo::claim_single_use`](crate::db::repos::CredentialsRepo::claim_single_use)),
/// so of two concurrent enrolments carrying the same token exactly one gets a
/// certificate. A reusable credential has nothing to claim and answers `true`.
///
/// `client_uid` is the device the signing request named, recorded with the
/// spend because it is the only device the enrolment grace window
/// ([`super::verify::Grace`]) will answer to afterwards. A request that named
/// none records none, and gets no grace.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the write fails.
pub async fn claim_single_use(
    db: &Database,
    credential: &CredentialRow,
    client_uid: Option<&str>,
    cache: &VerifiedSecretCache,
) -> Result<bool, Error> {
    if !credential.kind.is_single_use() {
        return Ok(true);
    }

    let claimed = db
        .credentials()
        .claim_single_use(credential.id, client_uid)
        .await?;

    if claimed {
        cache.forget(credential.id);
    }

    Ok(claimed)
}

/// Puts back a claim whose issuance then failed.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the write fails.
pub async fn release_single_use(
    db: &Database,
    credential: &CredentialRow,
    cache: &VerifiedSecretCache,
) -> Result<(), Error> {
    if !credential.kind.is_single_use() {
        return Ok(());
    }

    db.credentials().release_single_use(credential.id).await?;
    cache.forget(credential.id);

    Ok(())
}

/// Takes a credential back, and with it every certificate bought with it.
///
/// Returns whether anything was live to revoke, so a caller can answer `404`
/// for a credential that was already gone rather than claiming to have done
/// something.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if either write fails.
#[instrument("identity.credentials.revoke", skip_all, fields(credential = %id), err(Display))]
pub async fn revoke(
    db: &Database,
    id: CredentialId,
    actor: &Username,
    cache: &VerifiedSecretCache,
) -> Result<bool, Error> {
    let revoked = db.credentials().revoke(id).await?;

    // Forgotten whether or not the row was live: an entry for a credential
    // somebody is trying to revoke must not outlive the attempt.
    cache.forget(id);

    let certificates = db
        .certificates()
        .revoke_for_credential(
            id,
            RevocationDetails {
                reason: "the credential it was issued with was revoked".to_string(),
                by: Some(actor.clone()),
            },
        )
        .await?;

    if certificates > 0 {
        // TODO(M2-01/M2-03): tell `pki::RevocationCache` so the rustls client
        // verifier refuses these at the next handshake, and close the stream
        // connections already holding them. Until that hook exists the rows are
        // revoked in storage and a live mTLS session survives until it drops.
        warn!(
            credential = %id,
            certificates,
            "Revoked certificates in storage; live connections holding them are not yet closed.",
        );
    }

    Ok(revoked)
}

/// A credential as the API describes it.
///
/// `username` is filled in for an administrator listing somebody else's and
/// left out of a person's own listing, which is what the DTO's field means.
pub fn to_dto(row: &CredentialRow, username: Option<Username>) -> Credential {
    Credential {
        id: row.id,
        kind: row.kind,
        label: row.label.clone(),
        username,
        created_at: row.created_at,
        created_by: row.created_by.clone(),
        expires_at: row.expires_at,
        max_uses: row.max_uses,
        uses: row.uses,
        last_used_at: row.last_used_at,
        revoked_at: row.revoked_at,
    }
}

/// The `tak://` URL an ATAK client reads out of an enrolment QR code.
///
/// Composed here rather than in the browser so that the host is the server's
/// own canonical domain rather than whatever the UI was loaded from.
pub fn enroll_url(host: &str, username: &Username, secret: &str) -> String {
    ENROLL_URL
        .replace("{host}", &urlencode(host))
        .replace("{username}", &urlencode(username.as_str()))
        .replace("{token}", &urlencode(secret))
}

/// The same URL with `{token}` left in place, for the UI to fill from whichever
/// mint response it is still holding.
pub fn enroll_url_template(host: &str, username: &Username) -> String {
    ENROLL_URL
        .replace("{host}", &urlencode(host))
        .replace("{username}", &urlencode(username.as_str()))
}

/// Percent-encodes everything that is not unreserved in a query value.
///
/// Written out rather than pulled in: the only inputs are a host name, a
/// username and a base64url token, and a dependency for three character classes
/// would be more surface than the three lines it replaces.
fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());

    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }

    encoded
}

/// Generates a secret of the shape its kind is read in.
///
/// An enrolment token and a service token are read by machines, so density
/// beats legibility; a client password is typed into a phone by somebody
/// reading it off a screen, so it is Crockford base32 in groups of four.
fn generate(kind: CredentialKind) -> Secret {
    match kind {
        CredentialKind::EnrollmentToken => generate_token(DEFAULT_TOKEN_BYTES),
        CredentialKind::ClientPassword => generate_password(DEFAULT_PASSWORD_GROUPS),
        CredentialKind::ServiceToken => Secret::new(format!(
            "{SERVICE_TOKEN_PREFIX}{}",
            generate_token(SERVICE_TOKEN_BYTES).expose()
        )),
    }
}

/// When a credential of this kind should stop working.
fn expires_at(
    config: &AuthConfig,
    kind: CredentialKind,
    requested: Option<chrono::Duration>,
) -> Option<DateTime<Utc>> {
    let lifetime = match (requested, kind) {
        (Some(requested), _) => Some(requested),
        (None, CredentialKind::EnrollmentToken) => Some(config.enrollment_token_ttl),
        (None, CredentialKind::ClientPassword) => Some(config.client_password_ttl),
        // A sidecar's token is rotated by replacing it, not by an expiry it
        // would fail health checks in the middle of the night over.
        (None, CredentialKind::ServiceToken) => None,
    }?;

    Some(Utc::now() + lifetime)
}

/// How many uses a credential of this kind gets.
fn uses_allowed(kind: CredentialKind, requested: Option<u32>) -> Option<u32> {
    if kind.is_single_use() {
        // Not negotiable: "spent on first use" is the entire reason an
        // enrolment token can be shown in a QR code on somebody's screen.
        return Some(1);
    }

    requested
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::db::repos::NewUser;
    use crate::identity::verify::{Purpose, VerifyError, verify};

    async fn fixture() -> (Database, AuthConfig, UserRow, VerifiedSecretCache) {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(NewUser::person(Username::parse("alice").unwrap()))
            .await
            .unwrap();

        (
            db,
            AuthConfig::default(),
            user,
            VerifiedSecretCache::new(Duration::from_secs(300), 64),
        )
    }

    fn actor() -> Username {
        Username::parse("ada").unwrap()
    }

    async fn minted(
        db: &Database,
        config: &AuthConfig,
        user: &UserRow,
        kind: CredentialKind,
    ) -> MintedSecret {
        mint(db, config, user, MintRequest::new(kind, "Phone", &actor()))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_minted_secret_verifies_once_and_is_never_stored() {
        let (db, config, user, cache) = fixture().await;

        let minted = minted(&db, &config, &user, CredentialKind::EnrollmentToken).await;

        // Nothing but the hash and the hint reaches storage.
        let stored = db
            .credentials()
            .get(minted.credential.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!stored.secret_hash.as_str().contains(minted.secret.expose()));
        assert_ne!(stored.lookup_hint, minted.secret.expose());
        assert!(!format!("{minted:?}").contains(minted.secret.expose()));

        let verified = verify(
            &db,
            &user.username,
            minted.secret.expose(),
            Purpose::Enrollment,
            &cache,
        )
        .await
        .unwrap();

        assert_eq!(verified.credential.id, minted.credential.id);
        assert_eq!(verified.user.id, user.id);
        assert!(!verified.from_cache, "the first verification does the work");
    }

    #[tokio::test]
    async fn a_one_time_token_stays_one_time_however_many_uses_were_asked_for() {
        let (db, config, user, _) = fixture().await;

        let minted = mint(
            &db,
            &config,
            &user,
            MintRequest {
                max_uses: Some(50),
                ..MintRequest::new(CredentialKind::EnrollmentToken, "Phone", &actor())
            },
        )
        .await
        .unwrap();

        assert_eq!(minted.credential.max_uses, Some(1));
    }

    #[tokio::test]
    async fn revoking_a_credential_takes_back_the_certificates_bought_with_it() {
        let (db, config, user, cache) = fixture().await;
        let minted = minted(&db, &config, &user, CredentialKind::EnrollmentToken).await;
        let secret = minted.secret.expose().to_string();

        // Warm the cache, so the revocation has something to invalidate.
        verify(&db, &user.username, &secret, Purpose::Enrollment, &cache)
            .await
            .unwrap();

        assert!(
            revoke(&db, minted.credential.id, &actor(), &cache)
                .await
                .unwrap()
        );
        assert!(
            !cache.contains(minted.credential.id, &secret),
            "a revoked credential must stop working now, not in five minutes",
        );

        let refused = verify(&db, &user.username, &secret, Purpose::Enrollment, &cache)
            .await
            .unwrap_err();
        assert!(matches!(refused, VerifyError::BadSecret), "{refused:?}");

        assert!(
            !revoke(&db, minted.credential.id, &actor(), &cache)
                .await
                .unwrap(),
            "revoking twice should report that there was nothing live to take back",
        );
    }

    #[tokio::test]
    async fn a_client_password_cannot_be_minted_when_the_installation_refuses_them() {
        let (db, _, user, _) = fixture().await;
        let config = AuthConfig {
            client_passwords_enabled: false,
            ..AuthConfig::default()
        };

        let refused = mint(
            &db,
            &config,
            &user,
            MintRequest::new(CredentialKind::ClientPassword, "CloudTAK", &actor()),
        )
        .await
        .unwrap_err();

        assert!(refused.is(human_errors::Kind::User), "{refused}");

        // The kind that should be reached for instead is unaffected.
        assert!(
            mint(
                &db,
                &config,
                &user,
                MintRequest::new(CredentialKind::EnrollmentToken, "Phone", &actor()),
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn each_kind_gets_the_lifetime_and_the_shape_it_is_read_in() {
        let (db, config, user, _) = fixture().await;

        let token = minted(&db, &config, &user, CredentialKind::EnrollmentToken).await;
        let password = minted(&db, &config, &user, CredentialKind::ClientPassword).await;
        let service = minted(&db, &config, &user, CredentialKind::ServiceToken).await;

        assert!(token.credential.expires_at.is_some());
        assert!(password.credential.expires_at.is_some());
        assert_eq!(
            service.credential.expires_at, None,
            "a sidecar's token is rotated by replacing it, not by expiring mid-shift",
        );

        assert!(
            password.secret.expose().contains('-'),
            "a client password is typed off a screen: {:?}",
            password.secret.expose().len(),
        );
        assert!(service.secret.expose().starts_with("rsk_"));
        assert!(
            !token.secret.expose().contains('+') && !token.secret.expose().contains('/'),
            "an enrolment token travels in a query string",
        );
    }

    #[tokio::test]
    async fn a_credential_needs_a_label_somebody_can_recognise_it_by() {
        let (db, config, user, _) = fixture().await;

        let refused = mint(
            &db,
            &config,
            &user,
            MintRequest::new(CredentialKind::EnrollmentToken, "   ", &actor()),
        )
        .await
        .unwrap_err();

        assert!(refused.is(human_errors::Kind::User), "{refused}");
    }

    #[test]
    fn an_enrolment_url_escapes_everything_a_query_string_would_swallow() {
        let username = Username::parse("j.smith").unwrap();

        assert_eq!(
            enroll_url("tak.example.com", &username, "abc+def/ghi="),
            "tak://com.atakmap.app/enroll?host=tak.example.com&username=j.smith&token=abc%2Bdef%2Fghi%3D",
        );

        let template = enroll_url_template("tak.example.com", &username);
        assert!(template.ends_with("&token={token}"));
        assert_eq!(
            template.replace("{token}", "abc%2Bdef%2Fghi%3D"),
            enroll_url("tak.example.com", &username, "abc+def/ghi="),
        );
    }

    #[tokio::test]
    async fn the_dto_carries_what_an_administrator_needs_and_not_the_secret() {
        let (db, config, user, _) = fixture().await;
        let minted = minted(&db, &config, &user, CredentialKind::EnrollmentToken).await;

        let dto = to_dto(&minted.credential, Some(user.username.clone()));
        let rendered = serde_json::to_string(&dto).unwrap();

        assert!(!rendered.contains(minted.secret.expose()), "{rendered}");
        assert!(!rendered.contains("hash"), "{rendered}");
        assert_eq!(dto.created_by.as_ref().unwrap().as_str(), "ada");
        assert_eq!(dto.label, "Phone");

        assert_eq!(
            to_dto(&minted.credential, None).username,
            None,
            "a person listing their own credentials does not need to be told whose they are",
        );
    }
}
