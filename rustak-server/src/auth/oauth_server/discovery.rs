//! `GET /.well-known/openid-configuration` — rustak describing **itself** as an
//! OpenID provider.
//!
//! # Two documents at two paths, answering two questions
//!
//! `/login/.well-known/openid-configuration` ([`super::session`]) is TAK
//! Server's invention and publishes the **upstream** provider's endpoints, in
//! its bare two-field shape, for a WebTAK page that wants to know where rustak
//! federates to. This one is the path OpenID Connect Discovery reserves, and it
//! publishes **rustak's own** endpoints, for a relying party that wants rustak
//! to be its identity provider. Neither is a variant of the other and changing
//! either to look like the other breaks a client that reads it.
//!
//! # The issuer is the one in the tokens
//!
//! `issuer` here is `[auth] issuer` — the same string the access token and the
//! ID token carry as `iss`, which is exactly what a relying party compares them
//! against. An installation that has never been told its own URL cannot answer
//! this document at all, and says so rather than serving one built from a
//! `Host` header somebody else chose.
//!
//! Every endpoint is an absolute URL built from that issuer, because a relying
//! party resolves them without a base and a relative path would be resolved
//! against its own origin.

use actix_web::http::StatusCode;
use actix_web::http::header::{CACHE_CONTROL, HeaderValue};
use actix_web::{HttpResponse, web};

use crate::marti::response;
use crate::prelude::*;

use super::scopes;

/// How long a relying party may keep the document.
///
/// An hour: the contents change when the configuration does, which is a
/// restart, and a library that cached it for a day would be pointing at old
/// endpoints for a day.
const CACHE: HeaderValue = HeaderValue::from_static("public, max-age=3600");

/// `GET /.well-known/openid-configuration`.
pub async fn openid_configuration(context: web::Data<AppContext>) -> HttpResponse {
    let Some(issuer) = context.config().issuer() else {
        warn!(
            "An OpenID discovery document was asked for before this installation knows its own URL.",
        );

        return response::bare_json_with(
            StatusCode::NOT_FOUND,
            &serde_json::json!({ "error": "not_configured" }),
        );
    };

    let mut response = response::bare_json(&document(&issuer));

    response.headers_mut().insert(CACHE_CONTROL, CACHE);

    response
}

/// The document served for `issuer`.
///
/// Split out so that the URLs can be asserted without an `App`, and so that
/// `compat/oauth.md` §6 can be checked against one place.
fn document(issuer: &str) -> serde_json::Value {
    let base = issuer.trim_end_matches('/');
    let at = |path: &str| format!("{base}{path}");

    serde_json::json!({
        "issuer": base,
        "authorization_endpoint": at("/oauth/authorize"),
        "token_endpoint": at("/oauth/token"),
        "userinfo_endpoint": at("/oauth/userinfo"),
        "jwks_uri": at("/oauth/jwks"),
        "end_session_endpoint": at("/logout"),
        "response_types_supported": ["code"],
        // `password` is declared because it is genuinely served and CloudTAK
        // uses it today (`compat/oauth.md` §1); a relying party ignores it.
        "grant_types_supported": ["authorization_code", "refresh_token", "password"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "scopes_supported": scopes::SUPPORTED,
        "token_endpoint_auth_methods_supported": [
            "client_secret_post",
            "client_secret_basic",
            "none",
        ],
        // `S256` and nothing else. `plain` puts the verifier in the request an
        // interceptor already has, so it is not offered even as a fallback.
        "code_challenge_methods_supported": ["S256"],
        "claims_supported": [
            "sub",
            "iss",
            "aud",
            "exp",
            "iat",
            "auth_time",
            "nonce",
            "preferred_username",
            "name",
            "email",
            "groups",
        ],
    })
}

#[cfg(test)]
mod tests {
    use actix_web::App;
    use actix_web::http::header::CONTENT_TYPE;
    use actix_web::test::{self as http, TestRequest};

    use super::*;
    use crate::testing::TestServer;

    #[test]
    fn every_endpoint_is_absolute_and_built_from_the_issuer() {
        // A relying party resolves these without a base, so a relative path
        // would be resolved against its own origin.
        let document = document("https://tak.example.com");

        for name in [
            "authorization_endpoint",
            "token_endpoint",
            "userinfo_endpoint",
            "jwks_uri",
            "end_session_endpoint",
        ] {
            let url = document[name].as_str().expect(name);

            assert!(
                url.starts_with("https://tak.example.com/"),
                "{name} is {url}",
            );
            assert!(url::Url::parse(url).is_ok(), "{name} is {url}");
        }

        assert_eq!(document["issuer"], "https://tak.example.com");
        assert_eq!(
            document["authorization_endpoint"],
            "https://tak.example.com/oauth/authorize",
        );
        assert_eq!(
            document["userinfo_endpoint"],
            "https://tak.example.com/oauth/userinfo",
        );
        assert_eq!(document["jwks_uri"], "https://tak.example.com/oauth/jwks");
        assert_eq!(
            document["end_session_endpoint"],
            "https://tak.example.com/logout"
        );
    }

    #[test]
    fn a_trailing_slash_on_the_issuer_does_not_become_a_double_one() {
        let document = document("https://tak.example.com/");

        assert_eq!(document["issuer"], "https://tak.example.com");
        assert_eq!(
            document["token_endpoint"],
            "https://tak.example.com/oauth/token",
        );
    }

    #[test]
    fn plain_is_never_offered_even_as_a_fallback() {
        let document = document("https://tak.example.com");

        assert_eq!(document["code_challenge_methods_supported"][0], "S256");
        assert_eq!(
            document["code_challenge_methods_supported"]
                .as_array()
                .unwrap()
                .len(),
            1,
        );
        assert_eq!(
            document["id_token_signing_alg_values_supported"][0],
            "RS256"
        );
        assert_eq!(document["response_types_supported"][0], "code");
    }

    #[actix_web::test]
    async fn the_document_is_served_at_the_path_discovery_reserves() {
        let server = TestServer::start().await;
        let app = http::init_service(App::new().configure(server.app())).await;

        let response = http::call_service(
            &app,
            TestRequest::get()
                .uri("/.well-known/openid-configuration")
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&response::JSON),
            "exactly application/json, with no charset parameter",
        );
        assert_eq!(response.headers().get(CACHE_CONTROL).unwrap(), &CACHE);

        let body: serde_json::Value = http::read_body_json(response).await;

        assert_eq!(
            body["issuer"],
            server.jwt().unwrap().issuer(),
            "the issuer published has to be the one the tokens carry",
        );
    }

    #[actix_web::test]
    async fn the_upstream_providers_document_is_still_a_different_document() {
        // `/login/.well-known/…` is TAK Server's invention and answers a
        // different question; turning either into the other breaks a client.
        let server = TestServer::start().await;
        let app = http::init_service(App::new().configure(server.app())).await;

        let response = http::call_service(
            &app,
            TestRequest::get()
                .uri("/login/.well-known/openid-configuration")
                .to_request(),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "with no provider configured it says so, rather than describing us",
        );
    }
}
