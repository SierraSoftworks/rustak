//! The HS256 tokens that carry a role on one mission, and nothing else.
//!
//! A mission token is not an identity. It says "whoever holds this may act on
//! *this* mission with *this* role", which is what lets CloudTAK hand a Data
//! Sync's token to an ETL worker that has no account, and what lets an ATAK
//! device act on a mission it was invited to before it has joined a channel.
//! Three kinds exist, and the claim carrying the identifier is *named after the
//! kind* — `SUBSCRIPTION`, `INVITATION` or `ACCESS` — which is TAK Server's
//! shape and what a client replays verbatim.
//!
//! # Why a dedicated secret
//!
//! TAK Server signs these with the bytes of its RSA private key used as an HMAC
//! secret. We do not: reusing one key across two algorithms means a weakness in
//! either one reaches both, and our RSA key rotates on a schedule that has
//! nothing to do with mission tokens. [`MissionTokens::load`] generates 32
//! random bytes once, seals them with
//! [`crate::crypto::SecretContext::MissionTokenKey`]
//! and keeps them in the key/value store.
//!
//! # The two headers
//!
//! CloudTAK sends `MissionAuthorization: Bearer <token>`; ATAK sends the same
//! token in `Authorization`. Both must work at once, because a request may
//! carry an identity *and* a mission token. Design 04 D2 settles it:
//! `MissionAuthorization` is read first, and `Authorization` is only considered
//! when identity resolution did **not** consume it. A token that does not
//! verify means "no token" and never an error — the request may not have been
//! claiming a mission role at all.

use std::collections::HashSet;

use actix_web::HttpRequest;
use chrono::{Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rustak_core::identity::AuthMethod;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::crypto::{Sealed, SecretContext};
use crate::marti::MartiPrincipal;
use crate::prelude::*;

/// The header the mission token is read from first.
pub const MISSION_AUTHORIZATION: &str = "MissionAuthorization";

/// The key/value partition the signing secret is kept in.
const SECRET_PARTITION: &str = "missions";

/// The key it is kept under, which is also its sealing context.
pub const SECRET_KEY: &str = "mission-token-hmac";

/// How many bytes of HMAC key we generate.
const SECRET_BYTES: usize = 32;

/// The clock skew tolerated on an `exp` that is present.
const LEEWAY_SECONDS: u64 = 60;

/// Which of the three kinds a token is.
///
/// The name is both the `sub` claim and the name of the claim carrying the
/// identifier, so it is written out once here rather than at each call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenType {
    /// Minted when a client subscribes; carries that subscription's role.
    Subscription,
    /// Minted with an invitation; honoured only by `PUT …/subscription`.
    Invitation,
    /// Minted by presenting a mission's password; carries the default role.
    Access,
}

impl TokenType {
    /// The wire spelling, used for `sub` and for the identifier's claim name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subscription => "SUBSCRIPTION",
            Self::Invitation => "INVITATION",
            Self::Access => "ACCESS",
        }
    }

    /// The kind a `sub` claim names, when it names one of ours.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "SUBSCRIPTION" => Some(Self::Subscription),
            "INVITATION" => Some(Self::Invitation),
            "ACCESS" => Some(Self::Access),
            _ => None,
        }
    }
}

/// What a verified mission token said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionClaims {
    /// The token's own identifier.
    pub jti: String,
    /// Issued at, seconds since the epoch.
    pub iat: i64,
    /// Expires at, when it expires at all. Most mission tokens do not.
    pub exp: Option<i64>,
    /// Who issued it.
    pub iss: String,
    /// Which of the three kinds this is.
    pub kind: TokenType,
    /// The identifier the kind's own claim carried.
    pub id: String,
    /// The mission the token was minted for, by name.
    pub mission_name: String,
    /// The mission the token was minted for, by guid.
    pub mission_guid: Uuid,
}

/// Why a token was not accepted.
///
/// Every variant means the same thing to a caller — "this is not a mission
/// token we would honour" — and they are distinguished only so that a log line
/// can say which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// The signature, algorithm or expiry did not hold.
    Rejected,
    /// It verified and its claims are not a mission token's.
    Malformed,
}

/// The claims as they appear on the wire.
///
/// The identifier lives under a claim named after the kind, so all three names
/// are declared and exactly one is ever populated.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawClaims {
    jti: String,
    iat: i64,
    sub: String,
    iss: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exp: Option<i64>,
    #[serde(
        rename = "SUBSCRIPTION",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    subscription: Option<String>,
    #[serde(
        rename = "INVITATION",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    invitation: Option<String>,
    #[serde(rename = "ACCESS", default, skip_serializing_if = "Option::is_none")]
    access: Option<String>,
    #[serde(rename = "MISSION_NAME")]
    mission_name: String,
    #[serde(rename = "MISSION_GUID")]
    mission_guid: String,
}

/// Issues and verifies mission tokens.
///
/// `Debug` is written out so that a context holding one cannot print the
/// secret.
pub struct MissionTokens {
    secret: Zeroizing<Vec<u8>>,
    issuer: String,
}

impl std::fmt::Debug for MissionTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MissionTokens")
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

impl MissionTokens {
    /// Reads the signing secret back, generating and sealing one the first time.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the key/value store or the
    /// secret store refuses.
    pub async fn load(services: &impl Services) -> Result<Self, Error> {
        let issuer = services
            .config()
            .auth
            .issuer
            .clone()
            .unwrap_or_else(|| format!("rustak/{}", services.config().server.name));

        Ok(Self {
            secret: Zeroizing::new(load_secret(services).await?),
            issuer,
        })
    }

    /// A token backed by a caller-supplied secret, for tests and for tools.
    #[cfg(any(test, feature = "testing"))]
    pub fn with_secret(secret: Vec<u8>, issuer: impl Into<String>) -> Self {
        Self {
            secret: Zeroizing::new(secret),
            issuer: issuer.into(),
        }
    }

    /// Mints a token naming `id` as the kind's identifier.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the token cannot be signed,
    /// which only happens if the secret is unusable.
    pub fn issue(
        &self,
        id: &str,
        kind: TokenType,
        mission_name: &str,
        mission_guid: Uuid,
        ttl: Option<Duration>,
    ) -> Result<String, Error> {
        let now = Utc::now().timestamp();
        let identifier = Some(id.to_string());
        let claims = RawClaims {
            jti: Uuid::new_v4().to_string(),
            iat: now,
            sub: kind.as_str().to_string(),
            iss: self.issuer.clone(),
            exp: ttl.map(|ttl| now + ttl.num_seconds()),
            subscription: (kind == TokenType::Subscription)
                .then(|| identifier.clone())
                .flatten(),
            invitation: (kind == TokenType::Invitation)
                .then(|| identifier.clone())
                .flatten(),
            access: (kind == TokenType::Access).then_some(identifier).flatten(),
            mission_name: mission_name.to_string(),
            mission_guid: mission_guid.to_string(),
        };

        jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(&self.secret),
        )
        .or_system_err(&["This is unexpected; please report it with the surrounding log entries."])
    }

    /// Verifies a token and reports what it claimed.
    ///
    /// # Errors
    ///
    /// [`TokenError`], which the caller turns into "no mission role" rather
    /// than into a refusal: the bearer may have been an identity token that
    /// never claimed a mission at all.
    pub fn verify(&self, token: &str) -> Result<MissionClaims, TokenError> {
        let mut validation = Validation::new(Algorithm::HS256);

        // HS256 and nothing else, and no required claims: `exp` is optional on
        // a mission token, so demanding it would reject every token TAK mints.
        validation.algorithms = vec![Algorithm::HS256];
        validation.leeway = LEEWAY_SECONDS;
        validation.validate_exp = true;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        validation.required_spec_claims = HashSet::new();

        let decoded = jsonwebtoken::decode::<RawClaims>(
            token,
            &DecodingKey::from_secret(&self.secret),
            &validation,
        )
        .map_err(|_| TokenError::Rejected)?;

        claims_of(decoded.claims)
    }
}

/// Turns the wire claims into the checked form, or reports why not.
fn claims_of(raw: RawClaims) -> Result<MissionClaims, TokenError> {
    let kind = TokenType::parse(&raw.sub).ok_or(TokenError::Malformed)?;
    let id = match kind {
        TokenType::Subscription => raw.subscription,
        TokenType::Invitation => raw.invitation,
        TokenType::Access => raw.access,
    }
    .ok_or(TokenError::Malformed)?;
    let mission_guid = Uuid::parse_str(raw.mission_guid.trim_matches(['{', '}']))
        .map_err(|_| TokenError::Malformed)?;

    Ok(MissionClaims {
        jti: raw.jti,
        iat: raw.iat,
        exp: raw.exp,
        iss: raw.iss,
        kind,
        id,
        mission_name: raw.mission_name,
        mission_guid,
    })
}

/// The token a request carries, under design 04's D2 precedence.
///
/// `MissionAuthorization` wins; `Authorization` is only read when identity
/// resolution did not already consume it. The `Bearer` prefix is accepted in
/// either case, and a header with no prefix is taken as the bare token, which
/// is what some ETL clients send.
pub fn mission_bearer(request: &HttpRequest, identity_used_authorization: bool) -> Option<String> {
    if let Some(token) = header_token(request, MISSION_AUTHORIZATION) {
        return Some(token);
    }

    if identity_used_authorization {
        return None;
    }

    header_token(request, actix_web::http::header::AUTHORIZATION.as_str())
}

/// Whether identity resolution took this request's `Authorization` header.
///
/// A client certificate or a passkey leaves the header free for a mission
/// token; a bearer or Basic credential does not.
pub fn identity_used_authorization(who: &MartiPrincipal) -> bool {
    who.principal().is_some_and(|principal| {
        matches!(
            principal.via,
            AuthMethod::Bearer { .. } | AuthMethod::Basic { .. }
        )
    })
}

/// One header's value with the optional `Bearer` prefix removed.
fn header_token(request: &HttpRequest, name: &str) -> Option<String> {
    let raw = request.headers().get(name)?.to_str().ok()?.trim();

    let token = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .unwrap_or(raw)
        .trim();

    (!token.is_empty()).then(|| token.to_string())
}

/// Reads the sealed secret back, generating one the first time it is asked for.
async fn load_secret(services: &impl Services) -> Result<Vec<u8>, Error> {
    if let Some(sealed) = services
        .kv()
        .get::<Sealed>(SECRET_PARTITION, SECRET_KEY)
        .await?
    {
        return services
            .secrets()
            .open(&sealed, SecretContext::MissionTokenKey { kid: SECRET_KEY });
    }

    let generated: Vec<u8> = (0..SECRET_BYTES).map(|_| rand::random::<u8>()).collect();
    let sealed = services.secrets().seal(
        &generated,
        SecretContext::MissionTokenKey { kid: SECRET_KEY },
    )?;

    // `insert` rather than `set`: two workers racing on a first request must
    // agree on one secret rather than each overwriting the other's, which would
    // invalidate every token minted a moment earlier.
    if services
        .kv()
        .insert(SECRET_PARTITION, SECRET_KEY, sealed)
        .await?
    {
        info!("Generated this installation's mission-token signing secret.");

        return Ok(generated);
    }

    let stored = services
        .kv()
        .get::<Sealed>(SECRET_PARTITION, SECRET_KEY)
        .await?
        .ok_or_else(|| {
            human_errors::system(
                "The mission-token signing secret could not be read back after it was written.",
                crate::db::ADVICE_REPORT_DEV,
            )
        })?;

    services
        .secrets()
        .open(&stored, SecretContext::MissionTokenKey { kid: SECRET_KEY })
}

#[cfg(test)]
mod tests {
    use actix_web::test::TestRequest;
    use base64::Engine as _;

    use super::*;

    fn tokens() -> MissionTokens {
        MissionTokens::with_secret(vec![7; SECRET_BYTES], "rustak/test")
    }

    #[test]
    fn a_subscription_token_round_trips_with_its_named_claim() {
        let guid = Uuid::new_v4();
        let issued = tokens()
            .issue("sub-1", TokenType::Subscription, "Alpha", guid, None)
            .unwrap();
        let claims = tokens().verify(&issued).unwrap();

        assert_eq!(claims.kind, TokenType::Subscription);
        assert_eq!(claims.id, "sub-1");
        assert_eq!(claims.mission_name, "Alpha");
        assert_eq!(claims.mission_guid, guid);
        assert_eq!(claims.exp, None, "mission tokens do not expire by default");
    }

    #[test]
    fn the_identifier_claim_is_named_after_the_kind() {
        let issued = tokens()
            .issue("a-1", TokenType::Access, "Alpha", Uuid::new_v4(), None)
            .unwrap();
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(issued.split('.').nth(1).unwrap())
            .map(String::from_utf8)
            .unwrap()
            .unwrap();

        assert!(payload.contains(r#""ACCESS":"a-1""#), "{payload}");
        assert!(payload.contains(r#""sub":"ACCESS""#), "{payload}");
        assert!(payload.contains(r#""MISSION_NAME":"Alpha""#), "{payload}");
        assert!(!payload.contains("SUBSCRIPTION"), "{payload}");
    }

    #[test]
    fn a_token_signed_with_another_secret_is_rejected() {
        let issued = tokens()
            .issue(
                "sub-1",
                TokenType::Subscription,
                "Alpha",
                Uuid::new_v4(),
                None,
            )
            .unwrap();
        let other = MissionTokens::with_secret(vec![9; SECRET_BYTES], "rustak/test");

        assert_eq!(other.verify(&issued), Err(TokenError::Rejected));
    }

    #[test]
    fn an_expired_token_is_rejected() {
        let issued = tokens()
            .issue(
                "sub-1",
                TokenType::Subscription,
                "Alpha",
                Uuid::new_v4(),
                Some(Duration::seconds(-3600)),
            )
            .unwrap();

        assert_eq!(tokens().verify(&issued), Err(TokenError::Rejected));
    }

    #[test]
    fn a_ttl_is_carried_as_an_expiry() {
        let issued = tokens()
            .issue(
                "inv-1",
                TokenType::Invitation,
                "Alpha",
                Uuid::new_v4(),
                Some(Duration::hours(1)),
            )
            .unwrap();

        assert!(tokens().verify(&issued).unwrap().exp.is_some());
    }

    #[test]
    fn mission_authorization_wins_over_authorization() {
        let request = TestRequest::default()
            .insert_header((MISSION_AUTHORIZATION, "Bearer mission"))
            .insert_header(("Authorization", "Bearer identity"))
            .to_http_request();

        assert_eq!(mission_bearer(&request, false).as_deref(), Some("mission"));
        assert_eq!(mission_bearer(&request, true).as_deref(), Some("mission"));
    }

    #[test]
    fn authorization_is_only_read_when_identity_did_not_take_it() {
        let request = TestRequest::default()
            .insert_header(("Authorization", "Bearer identity"))
            .to_http_request();

        assert_eq!(mission_bearer(&request, false).as_deref(), Some("identity"));
        assert_eq!(mission_bearer(&request, true), None);
    }

    #[test]
    fn neither_header_means_no_token() {
        let request = TestRequest::default().to_http_request();

        assert_eq!(mission_bearer(&request, false), None);
    }

    #[test]
    fn a_lowercase_prefix_and_a_bare_token_are_both_accepted() {
        let lower = TestRequest::default()
            .insert_header((MISSION_AUTHORIZATION, "bearer abc"))
            .to_http_request();
        let bare = TestRequest::default()
            .insert_header((MISSION_AUTHORIZATION, "abc"))
            .to_http_request();

        assert_eq!(mission_bearer(&lower, false).as_deref(), Some("abc"));
        assert_eq!(mission_bearer(&bare, false).as_deref(), Some("abc"));
    }
}
