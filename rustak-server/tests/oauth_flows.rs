//! `GET /oauth/authorize`, the `authorization_code` grant, and the `/login/*`
//! federation round trip — against the real `App`.
//!
//! These are the security tests for the browser half of authentication, so
//! nearly every one of them is a **negative**: the positive round trip is here
//! once, to prove the flow works at all, and everything else asserts that
//! removing or tampering with one control breaks it.
//!
//! What is asserted, and why each matters:
//!
//! | Test | The control it holds |
//! |---|---|
//! | `a_code_is_worthless_without_the_verifier_that_made_it` | proof key for code exchange |
//! | `a_code_is_spent_the_first_time_it_is_exchanged` | single use |
//! | `a_code_cannot_be_redeemed_against_another_redirect_uri` | redirect binding |
//! | `an_expired_code_is_refused` | the ten-minute window |
//! | `a_public_client_cannot_start_a_flow_without_a_proof_key` | S256 required |
//! | `an_unregistered_redirect_uri_is_refused_here_rather_than_redirected_to` | no open redirect |
//! | `a_callback_with_a_tampered_state_is_refused` | the `sha256(cookie)` rule |
//! | `a_callback_cannot_be_replayed` | the one-shot pending record |
//! | `an_id_token_from_another_flow_is_refused` | the nonce |
//! | `a_session_cookie_is_refused_on_the_admin_api` | the cookie path rule |
//!
//! There is no node-tak scenario for any of this: CloudTAK 13.90 has no OIDC
//! back end at all (`compat/oauth.md` §4), so the goal is TAK-Server parity —
//! the bodies and cookie names asserted below are the ones TAK Server emits per
//! `research/06` §4.6 — so that CloudTAK works unchanged when it does ship one.
//!
//! Run with `cargo test -p rustak-server --features testing`.

#![cfg(feature = "testing")]

use actix_web::http::StatusCode;
use actix_web::http::header::{LOCATION, SET_COOKIE};
use actix_web::{App, test};
use rustak_server::config::{OAuthClient, OAuthServerConfig};
use rustak_server::prelude::*;
use rustak_server::testing::TestServer;
use rustak_server::testing::oidc::TestIdentityProvider;

/// The client every test registers.
const CLIENT: &str = "webtak";

/// The one URI it is registered for.
const REDIRECT: &str = "https://map.example.com/callback";

/// A verifier a client would keep, and its `S256` challenge.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

/// A server with `CLIENT` registered and, optionally, a provider to federate
/// with.
async fn server_with(provider: Option<&TestIdentityProvider>) -> TestServer {
    let oidc = provider.map(TestIdentityProvider::config);

    TestServer::start_with(move |config| {
        config.auth.oauth = OAuthServerConfig {
            clients: vec![OAuthClient {
                id: CLIENT.to_string(),
                redirect_uris: vec![REDIRECT.to_string()],
                public: true,
            }],
        };
        config.auth.oidc = oidc;
        config.auth.user_acl = Some(filt_rs::Filter::new("true").unwrap());
    })
    .await
}

/// The authorization request a browser would make.
fn authorize_uri(challenge: &str, redirect: &str) -> String {
    format!(
        "/oauth/authorize?response_type=code&client_id={CLIENT}\
         &redirect_uri={}&state=the-clients-state&code_challenge={challenge}\
         &code_challenge_method=S256",
        urlencode(redirect),
    )
}

/// The form a token request is posted as.
fn token_form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{key}={}", urlencode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-encodes a value. Written out rather than pulled in; the values here
/// are URIs and generated codes.
fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// One query parameter of a `Location` header.
fn param(location: &str, name: &str) -> Option<String> {
    url::Url::parse(location)
        .ok()?
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.to_string())
}

/// Every `Set-Cookie` on a response.
fn cookies(response: &actix_web::dev::ServiceResponse) -> Vec<String> {
    response
        .headers()
        .get_all(SET_COOKIE)
        .filter_map(|value| value.to_str().ok().map(str::to_string))
        .collect()
}

/// The `Location` a response redirects to.
fn location(response: &actix_web::dev::ServiceResponse) -> String {
    response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

mod authorization_code {
    use super::*;

    /// Signs somebody in, asks for a code, and returns it.
    ///
    /// A macro rather than a function because `test::call_service` takes
    /// `actix_http::Request`, which this crate does not depend on directly and
    /// therefore cannot name in a signature.
    macro_rules! code_for {
        ($server:expr, $app:expr, $redirect:expr $(,)?) => {{
            let (_, session) = $server.signed_in("ada", false).await;

            let response = test::call_service(
                $app,
                test::TestRequest::get()
                    .uri(&authorize_uri(CHALLENGE, $redirect))
                    .insert_header(("cookie", format!("access_token_0={}", session.token)))
                    .to_request(),
            )
            .await;

            assert_eq!(
                response.status(),
                StatusCode::FOUND,
                "{}",
                location(&response)
            );

            let location = location(&response);

            assert_eq!(
                param(&location, "state").as_deref(),
                Some("the-clients-state"),
                "the client's state has to come back untouched or it cannot match the request",
            );

            param(&location, "code").expect("the redirect carries a code")
        }};
    }

    #[actix_web::test]
    async fn a_signed_in_browser_gets_a_code_and_exchanges_it_for_a_session() {
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let code = code_for!(&server, &app, REDIRECT);

        let body: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/oauth/token")
                .insert_header(("content-type", "application/x-www-form-urlencoded"))
                .set_payload(token_form(&[
                    ("grant_type", "authorization_code"),
                    ("code", &code),
                    ("redirect_uri", REDIRECT),
                    ("client_id", CLIENT),
                    ("code_verifier", VERIFIER),
                ]))
                .to_request(),
        )
        .await;

        let token = body["access_token"].as_str().expect("an access token");

        assert_eq!(body["token_type"], "Bearer");
        assert!(body["refresh_token"].is_string());
        assert_eq!(
            server.jwt().unwrap().verify(token).unwrap().sub,
            "ada",
            "the session is ours, issued to the account the code was minted for",
        );
    }

    /// Posts a token request and returns its status and body.
    macro_rules! exchange {
        ($app:expr, $pairs:expr $(,)?) => {{
            let response = test::call_service(
                $app,
                test::TestRequest::post()
                    .uri("/oauth/token")
                    .insert_header(("content-type", "application/x-www-form-urlencoded"))
                    .set_payload(token_form($pairs))
                    .to_request(),
            )
            .await;

            let status = response.status();
            let body: serde_json::Value = test::read_body_json(response).await;

            (status, body)
        }};
    }

    #[actix_web::test]
    async fn a_code_is_worthless_without_the_verifier_that_made_it() {
        // The control that makes a code lifted from an address bar, a referrer
        // or a proxy log buy nothing at all.
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let code = code_for!(&server, &app, REDIRECT);

        let (status, body) = exchange!(
            &app,
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", REDIRECT),
                ("client_id", CLIENT),
                ("code_verifier", "a-verifier-somebody-else-made-up"),
            ],
        );

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");

        let (status, _) = exchange!(
            &app,
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", REDIRECT),
                ("client_id", CLIENT),
                ("code_verifier", VERIFIER),
            ],
        );

        assert_eq!(
            status,
            StatusCode::OK,
            "a wrong verifier must not have burnt the code the real client holds",
        );
    }

    #[actix_web::test]
    async fn a_code_with_no_verifier_at_all_is_refused() {
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let code = code_for!(&server, &app, REDIRECT);

        let (status, body) = exchange!(
            &app,
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", REDIRECT),
                ("client_id", CLIENT),
            ],
        );

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_request");
    }

    #[actix_web::test]
    async fn a_code_is_spent_the_first_time_it_is_exchanged() {
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let code = code_for!(&server, &app, REDIRECT);
        let grant = [
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", REDIRECT),
            ("client_id", CLIENT),
            ("code_verifier", VERIFIER),
        ];

        assert_eq!(exchange!(&app, &grant).0, StatusCode::OK);

        let (status, body) = exchange!(&app, &grant);

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
    }

    #[actix_web::test]
    async fn a_code_cannot_be_redeemed_against_another_redirect_uri() {
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let code = code_for!(&server, &app, REDIRECT);

        let (status, body) = exchange!(
            &app,
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", "https://map.example.com/callback/elsewhere"),
                ("client_id", CLIENT),
                ("code_verifier", VERIFIER),
            ],
        );

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
    }

    #[actix_web::test]
    async fn a_code_cannot_be_redeemed_by_another_client() {
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let code = code_for!(&server, &app, REDIRECT);

        let (status, body) = exchange!(
            &app,
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", REDIRECT),
                ("client_id", "some-other-client"),
                ("code_verifier", VERIFIER),
            ],
        );

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
    }

    #[actix_web::test]
    async fn an_expired_code_is_refused() {
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let code = code_for!(&server, &app, REDIRECT);

        // Reaching into the row rather than waiting ten minutes; the expiry is
        // the thing under test, not the clock.
        server
            .db()
            .write(|tx| {
                tx.execute(
                    "UPDATE oauth_codes SET expires_at = ?1",
                    [rustak_server::db::Timestamp::from(
                        chrono::Utc::now() - chrono::Duration::seconds(1),
                    )],
                )
            })
            .await
            .unwrap();

        let (status, body) = exchange!(
            &app,
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", REDIRECT),
                ("client_id", CLIENT),
                ("code_verifier", VERIFIER),
            ],
        );

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
    }

    #[actix_web::test]
    async fn the_password_grant_still_answers_exactly_what_cloudtak_parses() {
        // The new grant is additive: `compat/oauth.md` §1 pins the password
        // grant's body, and a `scope` or `refresh_token` leaking into it would
        // break node-tak's parser.
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let (status, body) = exchange!(&app, &[("grant_type", "client_credentials")]);

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "unsupported_grant_type");
        assert!(
            body["error_description"]
                .as_str()
                .is_some_and(|text| text.contains("authorization_code")),
            "{body}",
        );
    }
}

mod authorize_endpoint {
    use super::*;

    #[actix_web::test]
    async fn an_unregistered_redirect_uri_is_refused_here_rather_than_redirected_to() {
        // The difference between an authorization server and an open
        // redirector. `error=` in a query string does not make a bounce off a
        // trusted host any less of one.
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for redirect in [
            "https://evil.example.com/callback",
            "https://map.example.com/callback/evil",
            "https://map.example.com/callback?x=1",
        ] {
            let response = test::call_service(
                &app,
                test::TestRequest::get()
                    .uri(&authorize_uri(CHALLENGE, redirect))
                    .to_request(),
            )
            .await;

            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{redirect}");
            assert!(
                response.headers().get(LOCATION).is_none(),
                "{redirect} must not become a redirect",
            );
        }
    }

    #[actix_web::test]
    async fn an_unregistered_client_is_refused_without_redirecting() {
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!(
                    "/oauth/authorize?response_type=code&client_id=nobody&redirect_uri={}\
                     &code_challenge={CHALLENGE}&code_challenge_method=S256",
                    urlencode(REDIRECT),
                ))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response.headers().get(LOCATION).is_none());
    }

    #[actix_web::test]
    async fn a_public_client_cannot_start_a_flow_without_a_proof_key() {
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for query in [
            format!(
                "/oauth/authorize?response_type=code&client_id={CLIENT}&redirect_uri={}\
                 &state=s",
                urlencode(REDIRECT),
            ),
            format!(
                "/oauth/authorize?response_type=code&client_id={CLIENT}&redirect_uri={}\
                 &state=s&code_challenge={CHALLENGE}&code_challenge_method=plain",
                urlencode(REDIRECT),
            ),
        ] {
            let response =
                test::call_service(&app, test::TestRequest::get().uri(&query).to_request()).await;

            assert_eq!(response.status(), StatusCode::FOUND, "{query}");

            let location = location(&response);

            assert_eq!(
                param(&location, "error").as_deref(),
                Some("invalid_request")
            );
            assert_eq!(param(&location, "state").as_deref(), Some("s"));
            assert!(
                param(&location, "code").is_none(),
                "a refused request must not carry a code back: {location}",
            );
        }
    }

    #[actix_web::test]
    async fn a_response_type_we_do_not_serve_goes_back_to_the_client_as_an_error() {
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!(
                    "/oauth/authorize?response_type=token&client_id={CLIENT}\
                     &redirect_uri={}&state=s&code_challenge={CHALLENGE}",
                    urlencode(REDIRECT),
                ))
                .to_request(),
        )
        .await;

        assert_eq!(
            param(&location(&response), "error").as_deref(),
            Some("unsupported_response_type"),
        );
    }

    #[actix_web::test]
    async fn an_installation_with_nobody_signed_in_and_no_provider_refuses_rather_than_hangs() {
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&authorize_uri(CHALLENGE, REDIRECT))
                .to_request(),
        )
        .await;

        assert_eq!(
            param(&location(&response), "error").as_deref(),
            Some("access_denied"),
        );
    }
}

mod federation {
    use super::*;

    /// The `state` cookie a `Set-Cookie` header carries.
    fn state_of(cookies: &[String]) -> String {
        cookies
            .iter()
            .find_map(|cookie| cookie.strip_prefix("state="))
            .and_then(|rest| rest.split(';').next())
            .expect("the sign-in sets a state cookie")
            .to_string()
    }

    /// Drives `/login/auth` and the provider, returning the callback query and
    /// the `state` cookie the browser is holding.
    macro_rules! through_the_provider {
        ($app:expr, $start:expr $(,)?) => {{
            let response =
                test::call_service($app, test::TestRequest::get().uri($start).to_request()).await;

            assert_eq!(response.status(), StatusCode::FOUND);

            let set = cookies(&response);
            let state = state_of(&set);
            let at_provider = location(&response);

            assert_eq!(
                param(&at_provider, "state").as_deref(),
                Some(sha256_b64(&state).as_str()),
                "TAK Server's rule: the provider is sent the digest of the cookie",
            );
            assert_eq!(
                param(&at_provider, "code_challenge_method").as_deref(),
                Some("S256"),
            );
            assert!(param(&at_provider, "nonce").is_some());

            let redirect = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap()
                .get(&at_provider)
                .send()
                .await
                .unwrap();

            let back = redirect
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .expect("the provider sends the browser back")
                .to_string();

            let query = url::Url::parse(&back).unwrap();

            (
                format!("/login/redirect?{}", query.query().unwrap_or_default()),
                state,
            )
        }};
    }

    /// The base64url SHA-256 of a value, as the `state` rule uses it.
    fn sha256_b64(value: &str) -> String {
        use base64::Engine as _;
        use sha2::{Digest as _, Sha256};

        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
    }

    #[actix_web::test]
    async fn a_browser_signs_in_through_the_provider_and_comes_back_with_a_session_cookie() {
        let provider = TestIdentityProvider::start().await;
        let server = server_with(Some(&provider)).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let (callback, state) = through_the_provider!(&app, "/login/auth");

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&callback)
                .insert_header(("cookie", format!("state={state}")))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(location(&response), "/");

        let set = cookies(&response);
        let token = set
            .iter()
            .find_map(|cookie| cookie.strip_prefix("access_token_0="))
            .and_then(|rest| rest.split(';').next())
            .expect("the session arrives as TAK's chunked access_token cookie");

        assert_eq!(server.jwt().unwrap().verify(token).unwrap().sub, "alice");

        let attributes = set
            .iter()
            .find(|cookie| cookie.starts_with("access_token_0="))
            .unwrap();

        assert!(attributes.contains("HttpOnly"), "{attributes}");
        assert!(attributes.contains("Secure"), "{attributes}");
        assert!(attributes.contains("SameSite=Lax"), "{attributes}");
        assert!(attributes.contains("Path=/"), "{attributes}");

        // And the account the provider vouched for exists, with its claimed
        // channel mapped.
        let me: rustak_api::Me = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", format!("Bearer {token}")))
                .to_request(),
        )
        .await;

        assert_eq!(me.username.as_str(), "alice");
        assert!(me.groups.iter().any(|held| held.group.as_str() == "ops"));
    }

    #[actix_web::test]
    async fn a_callback_with_a_tampered_state_is_refused() {
        let provider = TestIdentityProvider::start().await;
        let server = server_with(Some(&provider)).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let (callback, state) = through_the_provider!(&app, "/login/auth");

        for cookie in [
            String::new(),
            "state=a-state-from-another-browser".to_string(),
            format!("state={state}x"),
        ] {
            let mut request = test::TestRequest::get().uri(&callback);

            if !cookie.is_empty() {
                request = request.insert_header(("cookie", cookie.clone()));
            }

            let response = test::call_service(&app, request.to_request()).await;

            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{cookie}");
            assert!(response.headers().get(LOCATION).is_none(), "{cookie}");
        }
    }

    #[actix_web::test]
    async fn a_callback_cannot_be_replayed() {
        // Even by the browser that legitimately made it: the pending record is
        // claimed once, so a second presentation finds nothing.
        let provider = TestIdentityProvider::start().await;
        let server = server_with(Some(&provider)).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let (callback, state) = through_the_provider!(&app, "/login/auth");
        let request = || {
            test::TestRequest::get()
                .uri(&callback)
                .insert_header(("cookie", format!("state={state}")))
                .to_request()
        };

        assert_eq!(
            test::call_service(&app, request()).await.status(),
            StatusCode::FOUND,
        );
        assert_eq!(
            test::call_service(&app, request()).await.status(),
            StatusCode::BAD_REQUEST,
        );
    }

    #[actix_web::test]
    async fn an_id_token_from_another_flow_is_refused() {
        // The nonce binds the token to the flow that asked for it. The provider
        // under test echoes whatever nonce it was given, so a callback pointed
        // at a *different* flow's pending record produces a token whose nonce
        // does not match — which is exactly what a replayed ID token looks
        // like.
        let provider = TestIdentityProvider::start().await;
        let server = server_with(Some(&provider)).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let (first, first_state) = through_the_provider!(&app, "/login/auth");
        let (second, _) = through_the_provider!(&app, "/login/auth");

        // The second flow's code, presented against the first flow's state and
        // cookie: the `state` check passes and the nonce check does not.
        let code = param(
            &format!("https://x{}", second.trim_start_matches("/login/redirect")),
            "code",
        )
        .expect("the second flow has a code");
        let tampered = format!(
            "{}&code={code}",
            first.split("&code=").next().unwrap_or(&first),
        );

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&tampered)
                .insert_header(("cookie", format!("state={first_state}")))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn an_authorization_request_with_no_session_completes_through_the_provider() {
        // `/oauth/authorize` hands off to the provider and `/login/redirect`
        // finishes the client's request: the browser ends up back at the
        // registered redirect URI with a code and its own state.
        let provider = TestIdentityProvider::start().await;
        let server = server_with(Some(&provider)).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let (callback, state) = through_the_provider!(&app, &authorize_uri(CHALLENGE, REDIRECT));

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&callback)
                .insert_header(("cookie", format!("state={state}")))
                .to_request(),
        )
        .await;

        let back = location(&response);

        assert!(back.starts_with(REDIRECT), "{back}");
        assert_eq!(
            param(&back, "state").as_deref(),
            Some("the-clients-state"),
            "the client's own state, not ours",
        );
        assert!(param(&back, "code").is_some(), "{back}");
    }

    #[actix_web::test]
    async fn the_endpoints_a_tak_client_probes_answer_in_tak_shapes() {
        let provider = TestIdentityProvider::start().await;
        let server = server_with(Some(&provider)).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        // `/login/authserver`: an ApiResponse whose `type` is the fully
        // qualified Java class name (research 06 §4.6).
        let body: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/login/authserver")
                .to_request(),
        )
        .await;

        assert_eq!(body["type"], "java.lang.String");
        assert_eq!(body["version"], "3");
        assert_eq!(body["data"], "The provider under test");
        assert!(body["nodeId"].is_string());

        // `/login/.well-known/openid-configuration`: a bare object with exactly
        // the upstream provider's two endpoints, and no envelope.
        let body: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/login/.well-known/openid-configuration")
                .to_request(),
        )
        .await;

        assert_eq!(
            body["authorization_endpoint"],
            format!("{}/authorize", provider.issuer()),
        );
        assert_eq!(
            body["token_endpoint"],
            format!("{}/token", provider.issuer())
        );
        assert!(body.get("version").is_none(), "no envelope: {body}");
    }

    #[actix_web::test]
    async fn an_installation_with_no_provider_says_so_the_way_tak_server_does() {
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for path in [
            "/login/authserver",
            "/login/.well-known/openid-configuration",
            "/login/auth",
        ] {
            let response =
                test::call_service(&app, test::TestRequest::get().uri(path).to_request()).await;

            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }
}

mod cookie_scope {
    use super::*;

    #[actix_web::test]
    async fn a_session_cookie_is_refused_on_the_admin_api() {
        // `/api/v1` is bearer-only by construction. A cookie there would be a
        // credential the browser attaches to every cross-site request.
        let server = server_with(None).await;
        let (_, session) = server.signed_in("ada", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for path in ["/api/v1/me", "/api/v1/users"] {
            let response = test::call_service(
                &app,
                test::TestRequest::get()
                    .uri(path)
                    .insert_header(("cookie", format!("access_token_0={}", session.token)))
                    .to_request(),
            )
            .await;

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
    }

    #[actix_web::test]
    async fn a_session_cookie_authenticates_the_tak_surface() {
        // A browser-based TAK client has no other way to call it, which is the
        // same trade TAK Server makes on its own non-mTLS port.
        let server = server_with(None).await;
        let (_, session) = server.signed_in("ada", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        // `/Marti/api/util/isAdmin` answers a bare boolean, not an envelope.
        let body: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/Marti/api/util/isAdmin")
                .insert_header(("cookie", format!("access_token_0={}", session.token)))
                .to_request(),
        )
        .await;

        assert_eq!(body, serde_json::json!(true));
    }

    #[actix_web::test]
    async fn a_cookie_that_is_not_one_of_our_tokens_establishes_nobody() {
        let server = server_with(None).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let body: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/Marti/api/util/isAdmin")
                .insert_header(("cookie", "access_token_0=not.a.token"))
                .to_request(),
        )
        .await;

        assert_eq!(body, serde_json::json!(false));
    }

    #[actix_web::test]
    async fn signing_out_clears_every_chunk_and_revokes_the_session() {
        let server = server_with(None).await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/logout")
                .insert_header((
                    "cookie",
                    format!("access_token_0={}; state=leftover", session.token),
                ))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let cleared = cookies(&response);

        assert!(
            cleared.iter().any(|cookie| cookie.starts_with("access_token_0=")
                && cookie.contains("Max-Age=0")),
            "{cleared:?}",
        );
        assert!(cleared.iter().any(|cookie| cookie.starts_with("state=")));

        let after = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/Marti/api/util/isAdmin")
                .insert_header(("cookie", format!("access_token_0={}", session.token)))
                .to_request(),
        )
        .await;

        let body: serde_json::Value = test::read_body_json(after).await;

        assert_eq!(
            body,
            serde_json::json!(false),
            "the token has to stop working, not just leave the browser",
        );
    }
}
