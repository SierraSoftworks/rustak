//! `Authorization: Basic` — the one reusable secret a TAK client can send.
//!
//! ATAK has no way to present a bearer token while it is enrolling: it has a
//! username and a one-time token (or a client password) typed in, or scanned
//! from a QR code, and it puts them in a Basic header. CloudTAK does the same
//! for `signClient/v2` even though it is already holding one of our access
//! tokens. So Basic exists, and `conventions.md`'s security defaults confine it
//! to exactly two places: the enrolment endpoints and `/oauth/token`.
//!
//! # Where it is accepted is not decided here
//!
//! [`basic_credential`] reads a header and [`verify_basic`] checks the secret
//! against the account's credentials for one [`Purpose`]. Neither decides
//! *whether* Basic was allowed on this request — that is
//! [`ListenerAuthPolicy`](super::resolve::ListenerAuthPolicy), so "where may
//! Basic be used" stays a property of the listener rather than of whichever
//! handler happened to read the header.

use actix_web::http::header::{AUTHORIZATION, HeaderMap};
use base64::Engine as _;
use rustak_core::identity::AuthMethod;
use rustak_core::prelude::*;

use crate::identity::secret_cache::VerifiedSecretCache;
use crate::identity::users;
use crate::identity::verify::{Purpose, VerifyError, verify};
use crate::prelude::Services;

use super::resolve::{AuthFailure, Resolved};

/// The realm named in a `WWW-Authenticate` challenge.
///
/// ATAK does not render it and CloudTAK never sees one, but a browser that
/// stumbles onto an enrolment URL will, and an operator reading a packet
/// capture should be told which server is asking.
pub const REALM: &str = "rustak";

/// The `WWW-Authenticate` header value a Basic-accepting path refuses with.
pub const CHALLENGE: &str = concat!("Basic realm=\"", "rustak", "\"");

/// A username and secret read out of a Basic header.
///
/// [`Debug`] is written out: the secret is a credential, and a refusal logged
/// with `?credential` would put it wherever the logs go.
#[derive(Clone)]
pub struct BasicCredential {
    /// The account the caller claims to be.
    pub username: Username,

    /// What they presented: an enrolment token or a client password.
    pub secret: String,
}

impl std::fmt::Debug for BasicCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BasicCredential")
            .field("username", &self.username)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Reads `Authorization: Basic <base64(user:secret)>`, if there is one.
///
/// [`None`] for an absent header, a scheme that is not Basic, base64 we cannot
/// decode, bytes that are not UTF-8, a value with no colon, or a username the
/// identity rules refuse. Every one of those is "no Basic credential was
/// presented" rather than a failure of its own: the caller goes on to try the
/// next credential the listener policy allows, and a malformed header must not
/// be able to short-circuit a request that also carried a certificate.
///
/// The password may itself contain colons — the split is on the **first** one,
/// per RFC 7617.
pub fn basic_credential(headers: &HeaderMap) -> Option<BasicCredential> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let encoded = value
        .strip_prefix("Basic ")
        .or_else(|| value.strip_prefix("basic "))?
        .trim();

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (username, secret) = text.split_once(':')?;

    let username = Username::parse(username).ok()?;

    Some(BasicCredential {
        username,
        secret: secret.to_string(),
    })
}

/// Checks a Basic credential against the account it names.
///
/// `purpose` is what the *path* is: an enrolment endpoint accepts a one-time
/// token or a client password, `/oauth/token` accepts a client password and
/// nothing else. The credential's own kind decides whether it answers to that
/// purpose ([`Purpose::accepts`]), so widening what a secret can do is a change
/// there rather than an accident of which endpoint read the header.
///
/// The credential is **not** spent here. A one-time enrolment token is consumed
/// by the handler that actually issues a certificate, because `tls/config` is
/// called first and spending the token there would strand the person half-way
/// through enrolling.
///
/// # Errors
///
/// [`AuthFailure::Rejected`] for every refusal a caller could provoke — no such
/// account, the wrong secret, an expired, exhausted or misapplied credential —
/// all reported identically, because the difference between them is an oracle;
/// [`AuthFailure::Unavailable`] when a read fails.
#[instrument(
    "auth.resolve.basic",
    skip_all,
    fields(username = %credential.username, purpose = ?purpose),
    err(Debug)
)]
pub async fn verify_basic<S: Services>(
    services: &S,
    credential: &BasicCredential,
    purpose: Purpose,
) -> Result<Resolved, AuthFailure> {
    let db = services.db();
    let cache = VerifiedSecretCache::shared();

    let verified = match verify(db, &credential.username, &credential.secret, purpose, cache).await
    {
        Ok(verified) => verified,
        Err(VerifyError::Unavailable(err)) => return Err(AuthFailure::Unavailable(err)),
        Err(refusal) => {
            debug!(reason = refusal.reason(), "Refused a Basic credential.");

            return Err(AuthFailure::Rejected);
        }
    };

    // Ordinary, non-consuming: the count is what exhausts a reusable password,
    // and a one-time token ignores this call by design.
    if let Err(err) =
        crate::identity::credentials::record_use(db, &verified.credential, false, cache).await
    {
        debug!(error = %err, "Could not record the use of a credential that verified.");
    }

    let via = AuthMethod::Basic {
        credential_id: verified.credential.id,
        kind: verified.credential.kind,
    };
    let principal = users::principal(db, &verified.user, via, false).await?;

    Ok(Resolved {
        principal,
        user: verified.user,
        claims: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, value.parse().unwrap());
        headers
    }

    fn encoded(pair: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(pair)
        )
    }

    #[test]
    fn a_username_and_secret_come_back_as_they_were_sent() {
        let parsed = basic_credential(&headers(&encoded("ada:s3cret"))).unwrap();

        assert_eq!(parsed.username.as_str(), "ada");
        assert_eq!(parsed.secret, "s3cret");
    }

    #[test]
    fn a_secret_containing_colons_survives_intact() {
        // RFC 7617 splits on the first colon; a generated secret that happens
        // to contain one would otherwise be silently truncated and refused.
        let parsed = basic_credential(&headers(&encoded("ada:a:b:c"))).unwrap();

        assert_eq!(parsed.secret, "a:b:c");
    }

    #[test]
    fn the_scheme_is_matched_without_regard_to_case() {
        let value = encoded("ada:token").replace("Basic ", "basic ");

        assert!(basic_credential(&headers(&value)).is_some());
    }

    #[test]
    fn anything_we_cannot_read_is_simply_no_credential() {
        for value in [
            "Bearer abc",
            "Basic",
            "Basic !!!not-base64!!!",
            // Base64 of bytes with no colon at all.
            "Basic YWRh",
            // Base64 of a username the identity rules refuse.
            &encoded(":secret"),
        ] {
            assert!(
                basic_credential(&headers(value)).is_none(),
                "{value} should not parse as a credential",
            );
        }

        assert!(basic_credential(&HeaderMap::new()).is_none());
    }

    #[test]
    fn a_debug_rendering_never_carries_the_secret() {
        let parsed = basic_credential(&headers(&encoded("ada:hunter2"))).unwrap();

        let rendered = format!("{parsed:?}");

        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("ada"), "{rendered}");
    }

    #[test]
    fn the_challenge_names_the_realm_this_server_answers_for() {
        assert_eq!(CHALLENGE, format!("Basic realm=\"{REALM}\""));
    }

    /// An account holding a credential of `kind`, and its secret.
    async fn minted(
        server: &crate::testing::TestServer,
        kind: rustak_api::CredentialKind,
    ) -> BasicCredential {
        let user = server.user("ada", false).await;
        let actor = Username::parse("ada").unwrap();

        let secret = crate::identity::credentials::mint(
            server.db(),
            &server.config().auth,
            &user,
            crate::identity::credentials::MintRequest::new(kind, "Phone", &actor),
        )
        .await
        .expect("mint the credential under test");

        BasicCredential {
            username: user.username,
            secret: secret.secret.expose().to_string(),
        }
    }

    #[tokio::test]
    async fn a_credential_that_belongs_here_resolves_to_its_account() {
        let server = crate::testing::TestServer::start().await;
        let credential = minted(&server, rustak_api::CredentialKind::EnrollmentToken).await;

        let resolved = verify_basic(&server.context, &credential, Purpose::Enrollment)
            .await
            .unwrap();

        assert_eq!(resolved.user.username.as_str(), "ada");
        assert!(matches!(resolved.principal.via, AuthMethod::Basic { .. }));
        assert!(resolved.claims.is_none());
    }

    #[tokio::test]
    async fn checking_an_enrolment_token_does_not_spend_it() {
        // `tls/config` is called before `signClient`, with the same token. A
        // token spent by the first call strands somebody half-way through.
        let server = crate::testing::TestServer::start().await;
        let credential = minted(&server, rustak_api::CredentialKind::EnrollmentToken).await;

        verify_basic(&server.context, &credential, Purpose::Enrollment)
            .await
            .unwrap();

        assert!(
            verify_basic(&server.context, &credential, Purpose::Enrollment)
                .await
                .is_ok(),
            "the token has to survive until a certificate has actually been issued",
        );
    }

    #[tokio::test]
    async fn a_credential_is_refused_where_its_kind_does_not_belong() {
        let server = crate::testing::TestServer::start().await;
        let credential = minted(&server, rustak_api::CredentialKind::EnrollmentToken).await;

        assert!(matches!(
            verify_basic(&server.context, &credential, Purpose::OAuthPassword).await,
            Err(AuthFailure::Rejected),
        ));
    }

    #[tokio::test]
    async fn every_refusal_a_caller_can_provoke_looks_the_same() {
        let server = crate::testing::TestServer::start().await;
        let real = minted(&server, rustak_api::CredentialKind::ClientPassword).await;

        for credential in [
            BasicCredential {
                username: real.username.clone(),
                secret: "not-the-secret".to_string(),
            },
            BasicCredential {
                username: Username::parse("nobody").unwrap(),
                secret: real.secret.clone(),
            },
        ] {
            assert!(matches!(
                verify_basic(&server.context, &credential, Purpose::OAuthPassword).await,
                Err(AuthFailure::Rejected),
            ));
        }
    }
}
