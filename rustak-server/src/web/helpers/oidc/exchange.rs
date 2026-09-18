//! Redeeming an authorization code, and renewing with the provider.
//!
//! Both are the confidential half of the flow: the client secret is sent from
//! here and never reaches the browser. The verifier the browser generated is
//! passed through untouched — we cannot check it, only refuse to redeem without
//! it, which is why the field is required by the API even though it is optional
//! on the wire to the provider.

use crate::config::OidcConfig;
use crate::prelude::*;
use crate::services::http::{MAX_JSON_BYTES, body_within};

use super::discovery::OidcDiscovery;

/// What to say when the provider is unreachable or unintelligible.
const ADVICE_PROVIDER: &[&str] = &[
    "Check that [auth.oidc] endpoint names a working OpenID Connect provider.",
    "Check that this server can reach that provider over the network.",
];

/// The token endpoint's response, as much of it as we use.
#[derive(Deserialize)]
struct ProviderTokens {
    id_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// What an exchange produced.
#[derive(Clone)]
pub struct TokenSet {
    /// The ID token, which is what says who signed in.
    pub id_token: String,
    /// The provider's refresh token, when it issued one.
    ///
    /// Held so that a long session can be renewed against the provider rather
    /// than by us alone — otherwise disabling somebody in the directory would
    /// not end their session here until their account was disabled here too.
    pub refresh_token: Option<String>,
}

impl std::fmt::Debug for TokenSet {
    /// Written out because both fields are credentials.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("id_token", &"***")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "***"))
            .finish()
    }
}

/// Exchanges an authorization code for tokens.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the provider rejects the code, and
/// a [`human_errors::Kind::System`] error when it cannot be reached.
#[instrument("web.oidc.exchange", skip_all, err(Display))]
pub async fn exchange_code(
    http: &reqwest::Client,
    oidc: &OidcConfig,
    discovery: &OidcDiscovery,
    code: &str,
    redirect_uri: &str,
    code_verifier: Option<&str>,
) -> Result<TokenSet, Error> {
    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", oidc.client_id.as_str()),
        ("client_secret", oidc.client_secret.as_str()),
    ];

    if let Some(verifier) = code_verifier {
        params.push(("code_verifier", verifier));
    }

    token_request(
        http,
        &discovery.token_endpoint,
        &params,
        "Your identity provider would not accept that sign-in.",
        &["Start signing in again from the beginning."],
    )
    .await
}

/// Renews a session against the provider.
///
/// A provider that does not rotate its refresh tokens simply omits one, so the
/// caller's own token is carried over rather than dropped — otherwise the
/// session would become unrenewable the first time it was renewed.
///
/// # Errors
///
/// As [`exchange_code`].
#[instrument("web.oidc.refresh", skip_all, err(Display))]
pub async fn refresh_tokens(
    http: &reqwest::Client,
    oidc: &OidcConfig,
    discovery: &OidcDiscovery,
    refresh_token: &str,
) -> Result<TokenSet, Error> {
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", oidc.client_id.as_str()),
        ("client_secret", oidc.client_secret.as_str()),
    ];

    let mut tokens = token_request(
        http,
        &discovery.token_endpoint,
        &params,
        "Your identity provider would not renew that session.",
        &["Sign in again to start a new session."],
    )
    .await?;

    if tokens.refresh_token.is_none() {
        tokens.refresh_token = Some(refresh_token.to_string());
    }

    Ok(tokens)
}

/// One form-encoded grant, posted and parsed.
async fn token_request(
    http: &reqwest::Client,
    token_endpoint: &str,
    params: &[(&str, &str)],
    rejection: &'static str,
    rejection_advice: &'static [&'static str],
) -> Result<TokenSet, Error> {
    let response = http
        .post(token_endpoint)
        .form(params)
        .send()
        .await
        .wrap_system_err(
            "We could not reach your identity provider's token endpoint.",
            ADVICE_PROVIDER,
        )?
        .error_for_status()
        .wrap_user_err(rejection, rejection_advice)?;

    // Capped rather than `.json()`; see `discovery::fetch`.
    let body = body_within(response, MAX_JSON_BYTES)
        .await
        .wrap_system_err(
            "We could not read your identity provider's token response.",
            ADVICE_PROVIDER,
        )?;

    let tokens: ProviderTokens = serde_json::from_slice(&body).wrap_system_err(
        "Your identity provider's token response was not in a shape we could read.",
        ADVICE_PROVIDER,
    )?;

    Ok(TokenSet {
        id_token: tokens.id_token,
        refresh_token: tokens.refresh_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn oidc(endpoint: String) -> OidcConfig {
        OidcConfig {
            endpoint,
            client_id: "rustak".to_string(),
            client_secret: "a-secret".to_string(),
            scopes: Vec::new(),
            username_claim: "preferred_username".to_string(),
            groups_claim: "groups".to_string(),
            group_prefix: String::new(),
            strip_group_prefix: true,
            read_suffix: "_READ".to_string(),
            write_suffix: "_WRITE".to_string(),
            read_only_group: None,
            auto_create_groups: true,
            link_by_username: false,
            display_name: None,
        }
    }

    fn discovery(base: &str) -> OidcDiscovery {
        OidcDiscovery {
            issuer: base.to_string(),
            authorization_endpoint: format!("{base}/authorize"),
            token_endpoint: format!("{base}/token"),
            jwks_uri: format!("{base}/jwks"),
        }
    }

    #[tokio::test]
    async fn the_verifier_the_browser_kept_is_sent_with_the_code() {
        // Without this the whole of proof key for code exchange is decoration:
        // a code intercepted from the redirect would still be redeemable.
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("code_verifier=the-verifier"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id_token": "header.payload.sig",
                "refresh_token": "refresh-123",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let tokens = exchange_code(
            &reqwest::Client::new(),
            &oidc(server.uri()),
            &discovery(&server.uri()),
            "the-code",
            "https://tak.example.com/auth/callback",
            Some("the-verifier"),
        )
        .await
        .unwrap();

        assert_eq!(tokens.id_token, "header.payload.sig");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-123"));
    }

    #[tokio::test]
    async fn a_rejected_code_is_a_failure_the_caller_can_show_somebody() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "invalid_grant" })),
            )
            .mount(&server)
            .await;

        let failed = exchange_code(
            &reqwest::Client::new(),
            &oidc(server.uri()),
            &discovery(&server.uri()),
            "the-code",
            "https://tak.example.com/auth/callback",
            Some("the-verifier"),
        )
        .await
        .unwrap_err();

        assert!(failed.is(human_errors::Kind::User));
    }

    #[tokio::test]
    async fn a_provider_that_does_not_rotate_keeps_the_session_renewable() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id_token": "renewed.id.token",
            })))
            .mount(&server)
            .await;

        let tokens = refresh_tokens(
            &reqwest::Client::new(),
            &oidc(server.uri()),
            &discovery(&server.uri()),
            "still-good",
        )
        .await
        .unwrap();

        assert_eq!(
            tokens.refresh_token.as_deref(),
            Some("still-good"),
            "dropping it would make the session unrenewable after one renewal",
        );
    }

    #[tokio::test]
    async fn a_rotated_refresh_token_replaces_the_one_we_held() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id_token": "renewed.id.token",
                "refresh_token": "rotated-456",
            })))
            .mount(&server)
            .await;

        let tokens = refresh_tokens(
            &reqwest::Client::new(),
            &oidc(server.uri()),
            &discovery(&server.uri()),
            "about-to-be-spent",
        )
        .await
        .unwrap();

        assert_eq!(tokens.refresh_token.as_deref(), Some("rotated-456"));
    }

    #[test]
    fn neither_token_is_printed_when_a_set_is_rendered() {
        let rendered = format!(
            "{:?}",
            TokenSet {
                id_token: "secret-id-token".to_string(),
                refresh_token: Some("secret-refresh".to_string()),
            }
        );

        assert!(!rendered.contains("secret"));
    }
}
