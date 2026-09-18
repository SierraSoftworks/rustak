//! Deciding whether an ID token really came from the provider.
//!
//! Four refusals matter, and each is a different attack:
//!
//! 1. **A symmetric algorithm.** The key set is public, so a token signed
//!    `HS256` with the published modulus as the secret would verify if we let
//!    the token choose its own algorithm. It does not get to.
//! 2. **No key identifier.** The `kid` is how we decide which public key to
//!    check against; a token without one could only be accepted unverified.
//! 3. **A missing audience or issuer.** `jsonwebtoken` compares `aud` and `iss`
//!    only against a token that carries them, and its defaults require neither
//!    — so a token that simply leaves `aud` out was accepted where one naming
//!    the wrong audience was refused. Harmless only while the provider issues
//!    tokens for nobody but us, which is not a property to depend on.
//! 4. **A nonce that is not the one this flow asked for.** Without it, an ID
//!    token obtained in some other flow could be replayed into this one.

use crate::config::OidcConfig;
use crate::prelude::*;

use super::discovery::{discovery, jwks};

/// What to say to somebody whose sign-in we refused.
///
/// Deliberately the same for every cause: which check a forged token failed is
/// not something the person presenting it should be told.
const ADVICE_SIGN_IN: &[&str] = &["Sign in again to obtain a fresh token."];

/// Verifies an ID token and returns its claims.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error for any token we would not accept, and
/// a [`human_errors::Kind::System`] error when the provider could not be
/// reached to find out.
#[instrument("web.oidc.validate", skip_all, err(Display))]
pub async fn validate_token<S: Services>(
    services: &S,
    oidc: &OidcConfig,
    token: &str,
    expected_nonce: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, Error> {
    let discovery = discovery(services, oidc).await?;
    let keys = jwks(services, &discovery, false).await?;

    // A token naming a key we have not seen may mean the provider rotated since
    // we cached; refetch once before refusing it.
    let keys = if needs_jwks_refresh(&keys, token) {
        jwks(services, &discovery, true).await?
    } else {
        keys
    };

    let claims = verify_token(&oidc.client_id, &discovery.issuer, &keys, token)?;

    check_nonce(&claims, expected_nonce)?;

    Ok(claims)
}

/// Whether the token names a signing key this key set does not have.
///
/// A token we cannot decode at all, or one with no `kid`, answers `false`: both
/// are refused by [`verify_token`] anyway, and a refetch would be a request to
/// the provider that anybody could cause by sending us rubbish.
fn needs_jwks_refresh(keys: &jsonwebtoken::jwk::JwkSet, token: &str) -> bool {
    match jsonwebtoken::decode_header(token).ok().and_then(|h| h.kid) {
        Some(kid) => keys.find(&kid).is_none(),
        None => false,
    }
}

/// The part of validation that fetches nothing, so it can be tested on its own.
fn verify_token(
    client_id: &str,
    issuer: &str,
    keys: &jsonwebtoken::jwk::JwkSet,
    token: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, Error> {
    let header = jsonwebtoken::decode_header(token).wrap_user_err(
        "We could not read the token your identity provider issued.",
        ADVICE_SIGN_IN,
    )?;

    if matches!(
        header.alg,
        jsonwebtoken::Algorithm::HS256
            | jsonwebtoken::Algorithm::HS384
            | jsonwebtoken::Algorithm::HS512
    ) {
        warn!(algorithm = ?header.alg, "Refused an ID token signed with a symmetric algorithm.");

        return Err(human_errors::user(
            "Your identity provider's token was signed with an algorithm we do not accept.",
            &[
                "An identity provider must sign its ID tokens with an asymmetric algorithm, such as RS256.",
            ],
        ));
    }

    let kid = header.kid.ok_or_else(|| {
        human_errors::user(
            "Your identity provider's token does not say which key signed it.",
            ADVICE_SIGN_IN,
        )
    })?;

    let jwk = keys.find(&kid).ok_or_else(|| {
        human_errors::user(
            "Your identity provider's token was signed with a key it does not publish.",
            ADVICE_SIGN_IN,
        )
    })?;

    let key = jsonwebtoken::DecodingKey::from_jwk(jwk).wrap_system_err(
        "We could not build a verification key from your identity provider's key set.",
        &["This usually means the provider published a key in a form we do not support."],
    )?;

    let mut validation = jsonwebtoken::Validation::new(header.alg);
    validation.set_audience(&[client_id]);
    validation.set_issuer(&[issuer]);
    validation.validate_exp = true;
    validation.validate_nbf = true;
    validation.set_required_spec_claims(&["exp", "aud", "iss"]);

    let data = jsonwebtoken::decode::<serde_json::Map<String, serde_json::Value>>(
        token,
        &key,
        &validation,
    )
    .wrap_user_err(
        "Your identity provider's token was not one we could accept.",
        ADVICE_SIGN_IN,
    )?;

    Ok(data.claims)
}

/// Binds an ID token to the flow that asked for it.
fn check_nonce(
    claims: &serde_json::Map<String, serde_json::Value>,
    expected: Option<&str>,
) -> Result<(), Error> {
    let Some(expected) = expected else {
        return Ok(());
    };

    if claims.get("nonce").and_then(|value| value.as_str()) == Some(expected) {
        return Ok(());
    }

    warn!("Refused an ID token whose nonce was not the one this sign-in asked for.");

    Err(human_errors::user(
        "That sign-in did not match the one this browser started.",
        &["Start signing in again from this page rather than reusing an old link."],
    ))
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    use super::*;
    use crate::services::AppContext;

    const B64: base64::engine::general_purpose::GeneralPurpose =
        base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn empty_keys() -> jsonwebtoken::jwk::JwkSet {
        jsonwebtoken::jwk::JwkSet { keys: vec![] }
    }

    fn unsigned(header: jsonwebtoken::Header, claims: serde_json::Value) -> String {
        format!(
            "{}.{}.not-a-signature",
            B64.encode(serde_json::to_vec(&header).unwrap()),
            B64.encode(serde_json::to_vec(&claims).unwrap()),
        )
    }

    fn hs256(kid: Option<&str>) -> String {
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
        header.kid = kid.map(str::to_string);

        jsonwebtoken::encode(
            &header,
            &serde_json::json!({
                "sub": "ada",
                "aud": "rustak",
                "iss": "https://id.example.com",
                "exp": chrono::Utc::now().timestamp() + 3600,
            }),
            &jsonwebtoken::EncodingKey::from_secret(b"the-public-modulus"),
        )
        .unwrap()
    }

    fn rs256_with_kid(kid: &str) -> String {
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some(kid.to_string());

        unsigned(header, serde_json::json!({ "sub": "ada" }))
    }

    #[test]
    fn a_symmetric_algorithm_is_refused_before_anything_is_verified() {
        // The published key set is public, so an attacker can sign HS256 with
        // the modulus as the secret. This is the whole of the defence.
        assert!(
            verify_token(
                "rustak",
                "https://id.example.com",
                &empty_keys(),
                &hs256(Some("any"))
            )
            .is_err()
        );
    }

    #[test]
    fn a_token_that_names_no_key_cannot_be_verified_so_it_is_refused() {
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        let token = unsigned(header, serde_json::json!({ "sub": "ada" }));

        assert!(verify_token("rustak", "https://id.example.com", &empty_keys(), &token).is_err());
    }

    #[test]
    fn rubbish_is_refused_rather_than_panicked_over() {
        assert!(
            verify_token(
                "rustak",
                "https://id.example.com",
                &empty_keys(),
                "not-a-jwt"
            )
            .is_err()
        );
    }

    #[test]
    fn only_an_unrecognised_key_identifier_sends_us_back_to_the_provider() {
        assert!(needs_jwks_refresh(
            &empty_keys(),
            &rs256_with_kid("rotated")
        ));
        assert!(!needs_jwks_refresh(&empty_keys(), "not-a-jwt"));
        assert!(!needs_jwks_refresh(&empty_keys(), &hs256(None)));
    }

    #[test]
    fn a_nonce_is_only_checked_when_this_flow_issued_one() {
        let claims = serde_json::Map::new();

        assert!(check_nonce(&claims, None).is_ok());
        assert!(check_nonce(&claims, Some("expected")).is_err());
    }

    #[test]
    fn a_token_from_another_flow_is_refused() {
        let mut claims = serde_json::Map::new();
        claims.insert("nonce".to_string(), serde_json::json!("some-other-flow"));

        assert!(check_nonce(&claims, Some("this-flow")).is_err());

        claims.insert("nonce".to_string(), serde_json::json!("this-flow"));
        assert!(check_nonce(&claims, Some("this-flow")).is_ok());
    }

    // Everything below drives `validate_token` against a real identity
    // provider, which is the only way most of it can be reached at all: until
    // something could mint a token we would accept, every path through here
    // that ends in "yes" was unreachable, and the ones that end in "no" could
    // only be reached with tokens too malformed to get as far as a signature
    // check.

    use crate::testing::TestIdentityProvider;

    async fn trusting(provider: &TestIdentityProvider) -> (AppContext, OidcConfig) {
        let oidc = provider.config();
        let context = AppContext::new_mock({
            let oidc = oidc.clone();
            move |config| config.auth.oidc = Some(oidc)
        })
        .await
        .unwrap();

        (context, oidc)
    }

    #[tokio::test]
    async fn a_token_the_provider_signed_is_accepted_and_its_claims_come_back_intact() {
        // The positive control every refusal below rests on: without it they
        // would all be satisfied by a validator that refused everything.
        let provider = TestIdentityProvider::start().await;
        let (context, oidc) = trusting(&provider).await;

        let claims = validate_token(&context, &oidc, &provider.sign_in_as("alice"), None)
            .await
            .expect("a token this provider signed should be accepted");

        assert_eq!(claims["preferred_username"], "alice");
        assert_eq!(claims["sub"], "subject-id-for-alice");
        assert_eq!(claims["email"], "alice@example.com");
    }

    #[tokio::test]
    async fn a_token_signed_by_a_key_the_provider_does_not_publish_is_refused() {
        // The forgery that matters. The key set is public, so a `kid` naming a
        // legitimate key is not a secret and an attacker will reuse it; what
        // they cannot do is produce a signature that verifies against it.
        // Everything else about this token is correct.
        let provider = TestIdentityProvider::start().await;
        let (context, oidc) = trusting(&provider).await;

        let forged = provider.forge(provider.claims_for("alice"));

        assert!(
            validate_token(&context, &oidc, &forged, None)
                .await
                .is_err(),
            "a token signed by anybody other than the provider must not sign somebody in",
        );
    }

    #[tokio::test]
    async fn a_token_naming_a_key_the_provider_never_published_is_refused() {
        // The complement: this signature really is the provider's and only the
        // label is wrong. It has to be refused anyway, because the label is how
        // we decide which key to check against — accepting one we could not
        // name would mean accepting one we never verified.
        let provider = TestIdentityProvider::start().await;
        let (context, oidc) = trusting(&provider).await;

        let mislabelled = provider.issue_with_kid(
            Some("a-key-nobody-has-heard-of"),
            provider.claims_for("alice"),
        );

        assert!(
            validate_token(&context, &oidc, &mislabelled, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_expired_token_is_refused() {
        // Sessions ending is the whole point of `exp`. Ignoring it would turn
        // every ID token we ever saw into a permanent credential, and the only
        // remedy left would be rotating the provider's key.
        let provider = TestIdentityProvider::start().await;
        let (context, oidc) = trusting(&provider).await;

        let mut claims = provider.claims_for("alice");
        claims["exp"] = serde_json::json!(chrono::Utc::now().timestamp() - 3600);

        assert!(
            validate_token(&context, &oidc, &provider.issue(claims), None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_token_minted_for_another_application_is_refused() {
        // The same provider may serve several applications. A token obtained
        // for a different one is genuinely signed, in date and genuinely
        // theirs — but it was never presented to them as a credential for this
        // server, and accepting it would let any other client of the same
        // provider sign people in here.
        let provider = TestIdentityProvider::start().await;
        let (context, oidc) = trusting(&provider).await;

        let mut claims = provider.claims_for("alice");
        claims["aud"] = serde_json::json!("some-other-application");

        assert!(
            validate_token(&context, &oidc, &provider.issue(claims), None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_token_that_simply_leaves_out_its_audience_is_refused_too() {
        // `jsonwebtoken` compares `aud` and `iss` only against a token that
        // carries them, so omitting one used to be accepted where naming the
        // wrong one was refused.
        let provider = TestIdentityProvider::start().await;
        let (context, oidc) = trusting(&provider).await;

        let mut claims = provider.claims_for("alice");
        let stripped = claims.as_object_mut().unwrap();
        stripped.remove("aud");
        stripped.remove("iss");

        assert!(
            validate_token(&context, &oidc, &provider.issue(claims), None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_token_from_another_sign_in_is_refused_when_this_one_issued_a_nonce() {
        let provider = TestIdentityProvider::start().await;
        let (context, oidc) = trusting(&provider).await;

        let mut claims = provider.claims_for("alice");
        claims["nonce"] = serde_json::json!("some-other-flow");

        assert!(
            validate_token(
                &context,
                &oidc,
                &provider.issue(claims.clone()),
                Some("this-flow")
            )
            .await
            .is_err()
        );

        claims["nonce"] = serde_json::json!("this-flow");

        assert!(
            validate_token(&context, &oidc, &provider.issue(claims), Some("this-flow"))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn the_keys_are_fetched_once_and_an_unknown_one_forces_exactly_one_refetch() {
        // Both halves matter and they pull against each other. Fetching the key
        // set per request would put the provider in the path of every API call;
        // never refetching would mean a rotation locked everybody out for a
        // day. The compromise only works if the refetch happens once — a token
        // naming a key that will never exist must not become a request to the
        // provider every time it is presented.
        let provider = TestIdentityProvider::start().await;
        let (context, oidc) = trusting(&provider).await;

        validate_token(&context, &oidc, &provider.sign_in_as("alice"), None)
            .await
            .unwrap();
        assert_eq!(provider.jwks_fetches().await, 1);
        assert_eq!(provider.discovery_fetches().await, 1);

        validate_token(&context, &oidc, &provider.sign_in_as("alice"), None)
            .await
            .unwrap();
        assert_eq!(
            provider.jwks_fetches().await,
            1,
            "a second sign-in should be served from the cached key set",
        );

        let rotated = provider.issue_with_kid(
            Some("a-key-nobody-has-heard-of"),
            provider.claims_for("alice"),
        );
        assert!(
            validate_token(&context, &oidc, &rotated, None)
                .await
                .is_err()
        );
        assert_eq!(
            provider.jwks_fetches().await,
            2,
            "an unrecognised key should send us back to the provider exactly once",
        );

        validate_token(&context, &oidc, &provider.sign_in_as("alice"), None)
            .await
            .unwrap();
        assert_eq!(provider.jwks_fetches().await, 2);
        assert_eq!(provider.discovery_fetches().await, 1);
    }
}
