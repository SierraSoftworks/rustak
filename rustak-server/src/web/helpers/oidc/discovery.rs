//! Finding the provider's endpoints and its signing keys, and remembering them.
//!
//! Both are fetched from the provider and cached in the key/value store, so
//! that signing somebody in does not put a round trip to the identity provider
//! in front of every API call the browser makes.
//!
//! The two have different lifetimes for the same reason: the discovery document
//! changes rarely and is cheap to refetch, while the key set changes exactly
//! when the provider rotates a key — which is picked up on demand by
//! [`validate`](super::validate), not by expiry. A long key cache with an
//! on-demand refetch is strictly better than a short one that would put the
//! provider in the hot path forever in exchange for noticing a rotation an hour
//! sooner.

use crate::config::OidcConfig;
use crate::prelude::*;

/// Where a cached discovery document lives.
pub const DISCOVERY_PARTITION: &str = "oidc:discovery";

/// Where a cached key set lives.
pub const JWKS_PARTITION: &str = "oidc:jwks";

/// How long a discovery document is kept.
const DISCOVERY_TTL_HOURS: i64 = 1;

/// How long a key set is kept. See the module documentation.
const JWKS_TTL_HOURS: i64 = 24;

/// What to tell somebody whose provider will not answer.
const ADVICE_PROVIDER: &[&str] = &[
    "Check that [auth.oidc] endpoint names a working OpenID Connect provider.",
    "Check that this server can reach that provider over the network.",
];

/// The part of the provider's discovery document we use.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OidcDiscovery {
    /// The issuer every ID token must name. Read from here rather than assumed
    /// to equal the configured endpoint, because a provider is entitled to
    /// publish a different one.
    pub issuer: String,
    /// Where the browser sends the person to sign in.
    pub authorization_endpoint: String,
    /// Where we redeem the code.
    pub token_endpoint: String,
    /// Where the signing keys are published.
    pub jwks_uri: String,
}

/// The provider's discovery document, from the cache or from the provider.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the provider cannot be reached
/// or answers with something we cannot read.
#[instrument("web.oidc.discovery", skip_all, err(Display))]
pub async fn discovery<S: Services>(
    services: &S,
    oidc: &OidcConfig,
) -> Result<OidcDiscovery, Error> {
    let endpoint = oidc.endpoint.trim_end_matches('/').to_string();
    let url = format!("{endpoint}/.well-known/openid-configuration");
    let http = services.http_client();

    services
        .cache()
        .cached(
            DISCOVERY_PARTITION,
            endpoint,
            move || Box::pin(async move { fetch(&http, &url, "discovery document").await }),
            chrono::Duration::hours(DISCOVERY_TTL_HOURS),
        )
        .await
}

/// The provider's signing keys.
///
/// `force_refresh` drops the cached copy first, which is what happens when a
/// token names a key we do not know: a rotation must not lock everybody out for
/// the lifetime of the cache entry.
///
/// # Errors
///
/// As [`discovery`].
#[instrument("web.oidc.jwks", skip_all, fields(refresh = force_refresh), err(Display))]
pub async fn jwks<S: Services>(
    services: &S,
    discovery: &OidcDiscovery,
    force_refresh: bool,
) -> Result<jsonwebtoken::jwk::JwkSet, Error> {
    let uri = discovery.jwks_uri.clone();

    if force_refresh {
        // The cache and the key/value store are the same table, so removing the
        // entry here is what makes the call below rebuild it.
        services.kv().remove(JWKS_PARTITION, uri.clone()).await?;
    }

    let url = uri.clone();
    let http = services.http_client();

    services
        .cache()
        .cached(
            JWKS_PARTITION,
            uri,
            move || Box::pin(async move { fetch(&http, &url, "signing keys").await }),
            chrono::Duration::hours(JWKS_TTL_HOURS),
        )
        .await
}

/// One GET, parsed, with the provider named in every failure.
async fn fetch<T: DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    what: &'static str,
) -> Result<T, Error> {
    http.get(url)
        .send()
        .await
        .wrap_system_err(
            format!("We could not reach your identity provider to fetch its {what}."),
            ADVICE_PROVIDER,
        )?
        .error_for_status()
        .wrap_system_err(
            format!("Your identity provider refused to hand over its {what}."),
            ADVICE_PROVIDER,
        )?
        .json()
        .await
        .wrap_system_err(
            format!("Your identity provider's {what} was not in a shape we could read."),
            ADVICE_PROVIDER,
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn provider() -> MockServer {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "authorization_endpoint": format!("{}/authorize", server.uri()),
                "token_endpoint": format!("{}/token", server.uri()),
                "jwks_uri": format!("{}/jwks", server.uri()),
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "keys": [] })),
            )
            .mount(&server)
            .await;

        server
    }

    fn config(endpoint: String) -> OidcConfig {
        OidcConfig {
            endpoint,
            client_id: "rustak".to_string(),
            client_secret: "secret".to_string(),
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

    async fn requests_to(server: &MockServer, wanted: &str) -> usize {
        server
            .received_requests()
            .await
            .expect("the mock server records what it was asked for")
            .iter()
            .filter(|request| request.url.path() == wanted)
            .count()
    }

    #[tokio::test]
    async fn the_document_is_fetched_once_and_then_served_from_the_cache() {
        let server = provider().await;
        let services = AppContext::new_mock(|_| {}).await.unwrap();
        let oidc = config(server.uri());

        let first = discovery(&services, &oidc).await.unwrap();
        let second = discovery(&services, &oidc).await.unwrap();

        assert_eq!(first.issuer, second.issuer);
        assert_eq!(
            requests_to(&server, "/.well-known/openid-configuration").await,
            1,
            "an API call per request must not become a provider call per request",
        );
    }

    #[tokio::test]
    async fn a_trailing_slash_on_the_endpoint_does_not_produce_a_double_slash() {
        let server = provider().await;
        let services = AppContext::new_mock(|_| {}).await.unwrap();

        assert!(
            discovery(&services, &config(format!("{}/", server.uri())))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_forced_refresh_goes_back_to_the_provider_exactly_once() {
        let server = provider().await;
        let services = AppContext::new_mock(|_| {}).await.unwrap();
        let oidc = config(server.uri());
        let document = discovery(&services, &oidc).await.unwrap();

        jwks(&services, &document, false).await.unwrap();
        jwks(&services, &document, false).await.unwrap();
        assert_eq!(requests_to(&server, "/jwks").await, 1);

        jwks(&services, &document, true).await.unwrap();
        assert_eq!(requests_to(&server, "/jwks").await, 2);

        jwks(&services, &document, false).await.unwrap();
        assert_eq!(
            requests_to(&server, "/jwks").await,
            2,
            "the refetched key set is cached in turn",
        );
    }

    #[tokio::test]
    async fn a_provider_that_will_not_answer_is_reported_rather_than_cached() {
        let services = AppContext::new_mock(|_| {}).await.unwrap();
        let oidc = config("http://127.0.0.1:1".to_string());

        assert!(discovery(&services, &oidc).await.is_err());
    }
}
