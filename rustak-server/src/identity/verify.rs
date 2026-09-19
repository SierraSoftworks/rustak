//! Checking a secret somebody has presented, and where it is accepted.
//!
//! The endpoint says what the secret is being presented *for* — a [`Purpose`] —
//! and the credential's kind says which purposes it answers to. Neither gets to
//! decide on its own, so a credential that is otherwise perfectly valid is
//! refused where it does not belong. Widening what a secret can do is therefore
//! a change to [`Purpose::accepts`] rather than an accident of which endpoint
//! happened to call [`verify`].
//!
//! # Why an unknown user costs a hash
//!
//! [`verify`] runs one argon2 verification on every path that fails, including
//! "there is no such account". Without it the endpoint answers "does this
//! username exist?" to anybody with a stopwatch, which is the first step of
//! every credential-stuffing run. The dummy hash is real work at the real
//! parameters — see [`rustak_core::identity::password`].
//!
//! # The one relaxation: a spent enrolment token
//!
//! ATAK enrols in three calls carrying the same one-time token, and the third —
//! `GET /Marti/api/tls/profile/enrollment?clientUid=` — arrives 0.4 s after the
//! second has spent it (M2-15 field report). [`Grace`] is what answers it:
//! inside `[auth] enrollment_grace` of the spend, for the `clientUid` that
//! spent it, on [`Purpose::EnrollmentProfile`] and nowhere else. Every other
//! presentation of a spent token is refused exactly as before, and the caller
//! has to ask for the relaxation by name — [`verify`] cannot grant it.
//!
//! # Why a refusal is detailed here and vague on the wire
//!
//! [`VerifyError`] distinguishes an expired credential from an exhausted one
//! from a wrong secret, because the audit log has to be able to say what
//! happened. The caller records that and tells the client only that the
//! credential was refused: the difference between "no such account" and "wrong
//! secret" is an oracle, and the log is where it belongs.

use chrono::{DateTime, Utc};
use rustak_api::CredentialKind;
use rustak_core::identity::{Secret, lookup_hint, verify_blocking, verify_dummy_blocking};
use rustak_core::prelude::*;

use crate::db::{
    Database,
    repos::{CredentialRow, UserRow},
};

use super::secret_cache::VerifiedSecretCache;

/// What a credential is being presented for.
///
/// The endpoint says which of these it is; the credential's kind says which of
/// these it answers to. Neither gets to decide on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// `GET /Marti/api/tls/config` and `POST /Marti/api/tls/signClient*`: the
    /// only place a one-time enrolment token is spent.
    Enrollment,

    /// `GET /Marti/api/tls/profile/**`: the device profile ATAK fetches
    /// straight after enrolling, with the token it has just spent. The only
    /// purpose [`Grace`] applies to.
    EnrollmentProfile,

    /// The password grant on `/oauth/token`, which is the one thing CloudTAK
    /// can do.
    OAuthPassword,

    /// Basic authentication on the rest of the Marti API.
    Marti,

    /// The `<auth>` block a TCP stream client opens with.
    StreamAuth,

    /// A sidecar's bearer token on `/api/v1/services/*`.
    ServiceApi,
}

impl Purpose {
    /// Whether a credential of this kind may be presented here.
    ///
    /// An enrolment token enrols and nothing else; a client password is the
    /// compatibility credential and is accepted only where a client genuinely
    /// has no alternative — never for the admin UI, never on the stream;
    /// a service token speaks only to the control API.
    pub fn accepts(self, kind: CredentialKind) -> bool {
        match self {
            Self::Enrollment | Self::EnrollmentProfile => matches!(
                kind,
                CredentialKind::EnrollmentToken | CredentialKind::ClientPassword
            ),
            Self::OAuthPassword | Self::Marti => kind == CredentialKind::ClientPassword,
            Self::StreamAuth => kind == CredentialKind::ClientPassword,
            Self::ServiceApi => kind == CredentialKind::ServiceToken,
        }
    }
}

/// What a **spent** one-time enrolment token may still be presented for.
///
/// Built by the caller from the request itself — the device it names and the
/// configured window — and passed to [`verify_with_grace`], which is the only
/// function that honours it. Holding the uid rather than reading it here is
/// deliberate: "which device is asking" is a property of the route's query
/// string, and a verifier that went looking for one could be pointed at a route
/// where it means something else.
#[derive(Debug, Clone, Copy)]
pub struct Grace<'a> {
    /// The `clientUid` this request named. Must equal the one recorded with
    /// the spend, or there is no grace.
    pub client_uid: &'a str,

    /// How long after the spend the window stays open. Zero switches the
    /// whole relaxation off.
    pub window: chrono::Duration,
}

impl Grace<'_> {
    /// Whether this credential's spend is inside the window, and this device's.
    ///
    /// The token's own `expires_at` is deliberately not consulted again: the
    /// spend is proof it was live when it was used, `spent_at` is written by
    /// the claim and by nothing else, and a revocation clears it — so the
    /// window measured from the spend is the only clock left that matters. An
    /// enrolment token lasts 15 minutes by default, and re-applying that to a
    /// device fetching its profile one second later is how a person who
    /// scanned the code at 14:59 gets stranded.
    fn covers(&self, candidate: &CredentialRow, now: DateTime<Utc>) -> bool {
        let Some(spent_at) = candidate.spent_at else {
            return false;
        };

        candidate.kind.is_single_use()
            && self.window > chrono::Duration::zero()
            && candidate.spent_uid.as_deref() == Some(self.client_uid)
            && now < spent_at + self.window
    }
}

/// A credential that was accepted, and whose account it is.
#[derive(Debug, Clone)]
pub struct Verified {
    pub user: UserRow,
    pub credential: CredentialRow,
    /// Whether the answer came from [`VerifiedSecretCache`] rather than from a
    /// fresh argon2 verification.
    pub from_cache: bool,
}

/// Why a credential was not accepted.
///
/// Distinguished here so the audit log can say what happened; a caller must
/// still tell the client only that the credential was refused, because the
/// difference between "no such account" and "wrong secret" is an oracle.
#[derive(Debug)]
pub enum VerifyError {
    /// There is no account by that name.
    NoSuchUser,
    /// The account exists and has been switched off.
    Disabled,
    /// No live credential of that account matched the secret.
    BadSecret,
    /// The credential matched and its lifetime has run out.
    Expired,
    /// The credential matched and every use it was given has been taken.
    Exhausted,
    /// The credential matched and is not accepted on this path.
    WrongPurpose(CredentialKind),
    /// Something of ours failed.
    Unavailable(Error),
}

impl VerifyError {
    /// A short word naming the refusal, for the audit log.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NoSuchUser => "no-such-user",
            Self::Disabled => "disabled",
            Self::BadSecret => "bad-secret",
            Self::Expired => "expired",
            Self::Exhausted => "exhausted",
            Self::WrongPurpose(_) => "wrong-purpose",
            Self::Unavailable(_) => "unavailable",
        }
    }
}

impl From<Error> for VerifyError {
    fn from(err: Error) -> Self {
        Self::Unavailable(err)
    }
}

/// Checks a secret against the credentials an account holds.
///
/// Every failing path costs one argon2 verification, including the one where
/// the account does not exist, so that the endpoint cannot be used to learn
/// which usernames are real.
///
/// # Errors
///
/// A [`VerifyError`] naming what was wrong, which the caller records and does
/// not repeat to the client.
pub async fn verify(
    db: &Database,
    username: &Username,
    secret: &str,
    purpose: Purpose,
    cache: &VerifiedSecretCache,
) -> Result<Verified, VerifyError> {
    verify_with_grace(db, username, secret, purpose, cache, None).await
}

/// As [`verify`], with the spent-token relaxation a caller has asked for.
///
/// `grace` is honoured only on [`Purpose::EnrollmentProfile`], so a caller that
/// builds one for the wrong route gets the ordinary answer rather than a
/// quietly widened one.
///
/// # Errors
///
/// As [`verify`].
#[instrument("identity.credentials.verify", skip_all, fields(username = %username, purpose = ?purpose), err(Debug))]
pub async fn verify_with_grace(
    db: &Database,
    username: &Username,
    secret: &str,
    purpose: Purpose,
    cache: &VerifiedSecretCache,
    grace: Option<Grace<'_>>,
) -> Result<Verified, VerifyError> {
    // Belt and braces: the route decides the purpose, and only one purpose can
    // spend a grace, so a `Grace` that reached the wrong path does nothing.
    let grace = grace.filter(|_| purpose == Purpose::EnrollmentProfile);

    let Some(user) = db.users().get_by_username(username).await? else {
        verify_dummy_blocking(Secret::new(secret)).await?;

        return Err(VerifyError::NoSuchUser);
    };

    if user.disabled {
        verify_dummy_blocking(Secret::new(secret)).await?;

        return Err(VerifyError::Disabled);
    }

    let now = Utc::now();
    let hint = lookup_hint(secret);

    // A spent row is revoked, and `find_by_hint` does not return revoked rows —
    // which is why the field's profile fetch was answered `BadSecret` rather
    // than `Exhausted`. Only the grace path asks for them, and only back to the
    // start of its own window.
    let candidates = match grace {
        Some(grace) => {
            db.credentials()
                .find_by_hint_including_spent(&hint, now - grace.window)
                .await?
        }
        None => db.credentials().find_by_hint(&hint).await?,
    };
    let mut refusal = None;

    for candidate in candidates {
        if candidate.user_id != user.id {
            continue;
        }

        let Some(from_cache) = matched(cache, &candidate, secret).await? else {
            continue;
        };

        // Matched: from here the refusals are about this credential rather
        // than about the secret, so the first one found is the answer.
        match usable(&candidate, purpose, now, grace) {
            Ok(()) => {
                cache.remember(candidate.id, secret);

                return Ok(Verified {
                    user,
                    credential: candidate,
                    from_cache,
                });
            }
            Err(err) => refusal = Some(err),
        }
    }

    // No candidate row matched at all, so nothing has been hashed yet on the
    // paths where the hint found nothing. Spend the time anyway.
    if refusal.is_none() {
        verify_dummy_blocking(Secret::new(secret)).await?;
    }

    Err(refusal.unwrap_or(VerifyError::BadSecret))
}

/// Whether the secret is this credential's, asking the cache before argon2.
///
/// `Some(true)` means the cache answered and no hash was computed, which is
/// what [`Verified::from_cache`] reports and what the tests assert on.
async fn matched(
    cache: &VerifiedSecretCache,
    candidate: &CredentialRow,
    secret: &str,
) -> Result<Option<bool>, VerifyError> {
    if cache.contains(candidate.id, secret) {
        return Ok(Some(true));
    }

    let verified = verify_blocking(Secret::new(secret), candidate.secret_hash.clone()).await?;

    Ok(verified.then_some(false))
}

/// Whether a matched credential may be used here and now.
///
/// The kind check comes first on every path, including the grace one: a
/// credential that does not belong on this route is refused whether or not it
/// would otherwise be live, so the relaxation can never widen *which* kinds a
/// route takes.
fn usable(
    candidate: &CredentialRow,
    purpose: Purpose,
    now: DateTime<Utc>,
    grace: Option<Grace<'_>>,
) -> Result<(), VerifyError> {
    if !purpose.accepts(candidate.kind) {
        return Err(VerifyError::WrongPurpose(candidate.kind));
    }

    // The spend, and only the spend, is forgiven — for this device, on this
    // route, inside this window. Everything a revocation touches has had its
    // `spent_at` cleared, so a token an administrator took back is refused
    // here like any other.
    if grace.is_some_and(|grace| grace.covers(candidate, now)) {
        return Ok(());
    }

    if candidate.revoked_at.is_some() {
        return Err(VerifyError::BadSecret);
    }

    if candidate.expires_at.is_some_and(|expires| expires <= now) {
        return Err(VerifyError::Expired);
    }

    if candidate.max_uses.is_some_and(|max| candidate.uses >= max) {
        return Err(VerifyError::Exhausted);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rustak_api::UserKind;

    use super::*;
    use crate::config::AuthConfig;
    use crate::db::repos::NewUser;
    use crate::identity::credentials::{
        MintRequest, MintedSecret, claim_single_use, mint, record_use,
    };

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
    async fn a_minted_secret_verifies_against_the_account_that_holds_it() {
        let (db, config, user, cache) = fixture().await;
        let minted = minted(&db, &config, &user, CredentialKind::EnrollmentToken).await;

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
    async fn a_second_verification_of_the_same_secret_skips_argon2() {
        let (db, config, user, cache) = fixture().await;
        let minted = minted(&db, &config, &user, CredentialKind::ClientPassword).await;

        for expected in [false, true] {
            let verified = verify(
                &db,
                &user.username,
                minted.secret.expose(),
                Purpose::OAuthPassword,
                &cache,
            )
            .await
            .unwrap();

            assert_eq!(
                verified.from_cache, expected,
                "the cache is what keeps a re-presented secret from costing 19 MiB",
            );
        }
    }
    #[tokio::test]
    async fn a_wrong_secret_is_refused_and_says_nothing_more_than_that() {
        let (db, config, user, cache) = fixture().await;
        minted(&db, &config, &user, CredentialKind::EnrollmentToken).await;

        let refused = verify(
            &db,
            &user.username,
            "not-the-secret",
            Purpose::Enrollment,
            &cache,
        )
        .await
        .unwrap_err();

        assert!(matches!(refused, VerifyError::BadSecret), "{refused:?}");
    }
    #[tokio::test]
    async fn an_unknown_account_still_costs_a_verification() {
        // The point of the dummy hash: "no such user" must not be faster than
        // "wrong password", or the endpoint enumerates accounts for free.
        let (db, _, _, cache) = fixture().await;
        let nobody = Username::parse("nobody").unwrap();

        let started = std::time::Instant::now();
        let refused = verify(&db, &nobody, "anything", Purpose::Enrollment, &cache)
            .await
            .unwrap_err();
        let elapsed = started.elapsed();

        assert!(matches!(refused, VerifyError::NoSuchUser), "{refused:?}");
        assert!(
            elapsed > Duration::from_millis(1),
            "an unknown account answered in {elapsed:?}, which is not a real argon2 verification",
        );
    }
    #[tokio::test]
    async fn a_switched_off_account_cannot_present_a_credential_that_still_exists() {
        let (db, config, user, cache) = fixture().await;
        let minted = minted(&db, &config, &user, CredentialKind::ClientPassword).await;

        db.users().set_disabled(user.id, true).await.unwrap();

        let refused = verify(
            &db,
            &user.username,
            minted.secret.expose(),
            Purpose::OAuthPassword,
            &cache,
        )
        .await
        .unwrap_err();

        assert!(matches!(refused, VerifyError::Disabled), "{refused:?}");
    }
    #[tokio::test]
    async fn an_enrolment_token_is_spent_only_when_a_certificate_was_issued() {
        let (db, config, user, cache) = fixture().await;
        let minted = minted(&db, &config, &user, CredentialKind::EnrollmentToken).await;
        let secret = minted.secret.expose().to_string();

        assert_eq!(minted.credential.max_uses, Some(1));

        // `GET /Marti/api/tls/config` verifies the token and records nothing;
        // an ordinary use recorded against it would spend it and strand the
        // person halfway through enrolling, so it is ignored rather than
        // honoured.
        record_use(&db, &minted.credential, false, &cache)
            .await
            .unwrap();
        assert!(
            verify(&db, &user.username, &secret, Purpose::Enrollment, &cache)
                .await
                .is_ok(),
            "fetching the certificate configuration must not spend the token",
        );

        record_use(&db, &minted.credential, true, &cache)
            .await
            .unwrap();

        let refused = verify(&db, &user.username, &secret, Purpose::Enrollment, &cache)
            .await
            .unwrap_err();

        assert!(matches!(refused, VerifyError::BadSecret), "{refused:?}");
    }
    #[tokio::test]
    async fn an_expired_credential_says_so_rather_than_looking_like_a_wrong_secret() {
        let (db, config, user, cache) = fixture().await;

        let minted = mint(
            &db,
            &config,
            &user,
            MintRequest {
                expires_in: Some(chrono::Duration::milliseconds(-1)),
                ..MintRequest::new(CredentialKind::ClientPassword, "Old laptop", &actor())
            },
        )
        .await
        .unwrap();

        let refused = verify(
            &db,
            &user.username,
            minted.secret.expose(),
            Purpose::OAuthPassword,
            &cache,
        )
        .await
        .unwrap_err();

        assert!(matches!(refused, VerifyError::Expired), "{refused:?}");
        assert_eq!(refused.reason(), "expired");
    }
    #[tokio::test]
    async fn a_credential_with_its_uses_spent_is_exhausted_rather_than_wrong() {
        let (db, config, user, cache) = fixture().await;

        let minted = mint(
            &db,
            &config,
            &user,
            MintRequest {
                max_uses: Some(1),
                ..MintRequest::new(CredentialKind::ClientPassword, "Bridge", &actor())
            },
        )
        .await
        .unwrap();

        record_use(&db, &minted.credential, false, &cache)
            .await
            .unwrap();

        let refused = verify(
            &db,
            &user.username,
            minted.secret.expose(),
            Purpose::OAuthPassword,
            &cache,
        )
        .await
        .unwrap_err();

        assert!(matches!(refused, VerifyError::Exhausted), "{refused:?}");
    }
    #[tokio::test]
    async fn a_credential_is_refused_where_its_kind_does_not_belong() {
        // The whole argument for enrolment tokens: an enrolment token cannot
        // become a password grant, and a service token cannot enrol a device.
        let (db, config, user, cache) = fixture().await;
        let token = minted(&db, &config, &user, CredentialKind::EnrollmentToken).await;
        let service = minted(&db, &config, &user, CredentialKind::ServiceToken).await;

        for (secret, purpose) in [
            (token.secret.expose(), Purpose::OAuthPassword),
            (token.secret.expose(), Purpose::StreamAuth),
            (service.secret.expose(), Purpose::Enrollment),
            (service.secret.expose(), Purpose::Marti),
        ] {
            let refused = verify(&db, &user.username, secret, purpose, &cache)
                .await
                .unwrap_err();

            assert!(
                matches!(refused, VerifyError::WrongPurpose(_)),
                "{purpose:?} accepted a credential it should not: {refused:?}",
            );
        }
    }
    #[test]
    fn every_purpose_names_the_kinds_it_takes_and_no_others() {
        assert!(Purpose::ServiceApi.accepts(CredentialKind::ServiceToken));
        assert!(!Purpose::ServiceApi.accepts(CredentialKind::ClientPassword));
        assert!(Purpose::Enrollment.accepts(CredentialKind::EnrollmentToken));
        assert!(Purpose::EnrollmentProfile.accepts(CredentialKind::EnrollmentToken));
        assert!(!Purpose::EnrollmentProfile.accepts(CredentialKind::ServiceToken));
        assert!(!Purpose::OAuthPassword.accepts(CredentialKind::EnrollmentToken));
        assert!(!Purpose::Marti.accepts(CredentialKind::ServiceToken));
    }
    /// The `clientUid` ATAK sends, and the window the defaults give it.
    const UID: &str = "ANDROID-7e0bf5df978a87d8";

    fn grace(client_uid: &str) -> Grace<'_> {
        Grace {
            client_uid,
            window: chrono::Duration::minutes(10),
        }
    }

    /// Mints an enrolment token and spends it for `uid`, as `signClient/v2`
    /// does, answering the secret it was minted with.
    async fn spent(
        db: &Database,
        config: &AuthConfig,
        user: &UserRow,
        uid: Option<&str>,
        cache: &VerifiedSecretCache,
    ) -> (String, CredentialId) {
        let minted = minted(db, config, user, CredentialKind::EnrollmentToken).await;

        assert!(
            claim_single_use(db, &minted.credential, uid, cache)
                .await
                .unwrap(),
            "the token was live, so the claim is the one that got it",
        );

        (minted.secret.expose().to_string(), minted.credential.id)
    }

    #[tokio::test]
    async fn a_spent_token_still_fetches_the_profile_of_the_device_that_spent_it() {
        // The field failure: ATAK asks for its enrolment profile 0.4 s after
        // the certificate, with the token the certificate spent (M2-15).
        let (db, config, user, cache) = fixture().await;
        let (secret, id) = spent(&db, &config, &user, Some(UID), &cache).await;

        let verified = verify_with_grace(
            &db,
            &user.username,
            &secret,
            Purpose::EnrollmentProfile,
            &cache,
            Some(grace(UID)),
        )
        .await
        .expect("the profile fetch is what the grace window exists for");

        assert_eq!(verified.credential.id, id);
    }
    #[tokio::test]
    async fn the_grace_answers_the_device_that_spent_the_token_and_no_other() {
        let (db, config, user, cache) = fixture().await;
        let (secret, _) = spent(&db, &config, &user, Some(UID), &cache).await;

        for uid in ["ANDROID-somebody-else", ""] {
            let refused = verify_with_grace(
                &db,
                &user.username,
                &secret,
                Purpose::EnrollmentProfile,
                &cache,
                Some(grace(uid)),
            )
            .await
            .unwrap_err();

            assert!(
                matches!(refused, VerifyError::BadSecret),
                "{uid}: {refused:?}"
            );
        }
    }
    #[tokio::test]
    async fn a_token_spent_without_naming_a_device_has_no_grace_to_give() {
        // A grace that cannot name the device it is for is a grace for anybody.
        let (db, config, user, cache) = fixture().await;
        let (secret, _) = spent(&db, &config, &user, None, &cache).await;

        let refused = verify_with_grace(
            &db,
            &user.username,
            &secret,
            Purpose::EnrollmentProfile,
            &cache,
            Some(grace(UID)),
        )
        .await
        .unwrap_err();

        assert!(matches!(refused, VerifyError::BadSecret), "{refused:?}");
    }
    #[tokio::test]
    async fn the_grace_closes_when_its_window_does() {
        let (db, config, user, cache) = fixture().await;
        let (secret, _) = spent(&db, &config, &user, Some(UID), &cache).await;

        for window in [chrono::Duration::milliseconds(1), chrono::Duration::zero()] {
            tokio::time::sleep(Duration::from_millis(5)).await;

            let refused = verify_with_grace(
                &db,
                &user.username,
                &secret,
                Purpose::EnrollmentProfile,
                &cache,
                Some(Grace {
                    client_uid: UID,
                    window,
                }),
            )
            .await
            .unwrap_err();

            assert!(
                matches!(refused, VerifyError::BadSecret),
                "{window} left the window open: {refused:?}",
            );
        }
    }
    #[tokio::test]
    async fn a_spent_token_buys_the_profile_and_nothing_else() {
        // The whole security argument for the window: it is not a token that
        // works again, it is one route that answers one device.
        let (db, config, user, cache) = fixture().await;
        let (secret, _) = spent(&db, &config, &user, Some(UID), &cache).await;

        for purpose in [
            Purpose::Enrollment,
            Purpose::OAuthPassword,
            Purpose::Marti,
            Purpose::StreamAuth,
            Purpose::ServiceApi,
        ] {
            let refused = verify_with_grace(
                &db,
                &user.username,
                &secret,
                purpose,
                &cache,
                Some(grace(UID)),
            )
            .await
            .unwrap_err();

            assert!(
                !matches!(refused, VerifyError::Unavailable(_)),
                "{purpose:?} accepted a spent enrolment token: {refused:?}",
            );
        }
    }
    #[tokio::test]
    async fn revoking_a_spent_token_ends_its_grace() {
        // A revocation is an administrator saying "not that one, now"; a window
        // that outlived it would be a ten-minute hole in the answer.
        let (db, config, user, cache) = fixture().await;
        let (secret, id) = spent(&db, &config, &user, Some(UID), &cache).await;

        db.credentials().revoke(id).await.unwrap();

        let refused = verify_with_grace(
            &db,
            &user.username,
            &secret,
            Purpose::EnrollmentProfile,
            &cache,
            Some(grace(UID)),
        )
        .await
        .unwrap_err();

        assert!(matches!(refused, VerifyError::BadSecret), "{refused:?}");
    }
    #[tokio::test]
    async fn the_relaxation_has_to_be_asked_for_by_name() {
        // `verify` cannot grant it: a caller that has not built a `Grace` gets
        // the answer a spent token has always had.
        let (db, config, user, cache) = fixture().await;
        let (secret, _) = spent(&db, &config, &user, Some(UID), &cache).await;

        let refused = verify(
            &db,
            &user.username,
            &secret,
            Purpose::EnrollmentProfile,
            &cache,
        )
        .await
        .unwrap_err();

        assert!(matches!(refused, VerifyError::BadSecret), "{refused:?}");
    }
    #[tokio::test]
    async fn a_secret_belonging_to_somebody_else_is_not_accepted_for_this_account() {
        // The hint lookup returns rows across every account, so the owner check
        // is what stops one person's password signing in as another.
        let (db, config, alice, cache) = fixture().await;
        let bob = db
            .users()
            .create(NewUser {
                kind: UserKind::Person,
                ..NewUser::person(Username::parse("bob").unwrap())
            })
            .await
            .unwrap();

        let hers = minted(&db, &config, &alice, CredentialKind::ClientPassword).await;

        let refused = verify(
            &db,
            &bob.username,
            hers.secret.expose(),
            Purpose::OAuthPassword,
            &cache,
        )
        .await
        .unwrap_err();

        assert!(matches!(refused, VerifyError::BadSecret), "{refused:?}");
    }
}
