//! `POST /oauth/token` and the key endpoints beside it, against the real `App`.
//!
//! CloudTAK's whole login is this one exchange, and its JWT "parser" does no
//! signature verification and no JWKS fetch. It base64-decodes the *entire*
//! token as one blob — Node's decoder silently drops the `.` separators — and
//! then finds the payload by splitting on the first `}`. That only lands on a
//! payload boundary when the header's base64url encoding is a multiple of four
//! characters, and only yields valid JSON when every claim is a flat scalar.
//!
//! Both rules are asserted here directly rather than only through a round trip,
//! so that a failure says *which* one was broken instead of the
//! `Unexpected TAK JWT Format` a client would report.
//!
//! Run with `cargo test -p rustak-server --features testing`.

#![cfg(feature = "testing")]

use actix_web::http::header::CONTENT_TYPE;
use actix_web::{App, test};
use base64::Engine as _;
use rustak_api::CredentialKind;
use rustak_server::identity::credentials::{MintRequest, mint};
use rustak_server::prelude::*;
use rustak_server::testing::TestServer;

/// An account with a client password, which is the only credential
/// `/oauth/token` accepts.
async fn with_password(server: &TestServer, username: &str, is_admin: bool) -> String {
    let user = server.user(username, is_admin).await;
    let actor = Username::parse("ada").expect("a usable actor name");

    mint(
        server.db(),
        &server.config().auth,
        &user,
        MintRequest::new(CredentialKind::ClientPassword, "CloudTAK", &actor),
    )
    .await
    .expect("mint the client password under test")
    .secret
    .expose()
    .to_string()
}

/// The form body a grant is posted as.
fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{key}={}", urlencoding(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-encodes a form value. Written out rather than pulled in: the only
/// values here are usernames and generated secrets.
fn urlencoding(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// A `POST /oauth/token` request carrying `body`.
fn grant(body: String) -> test::TestRequest {
    test::TestRequest::post()
        .uri("/oauth/token")
        .insert_header((CONTENT_TYPE, "application/x-www-form-urlencoded"))
        .set_payload(body)
}

#[actix_web::test]
async fn a_client_password_is_exchanged_for_exactly_the_body_cloudtak_reads() {
    let server = TestServer::start().await;
    let password = with_password(&server, "ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let response = test::call_service(
        &app,
        grant(form(&[
            ("grant_type", "password"),
            ("username", "ada"),
            ("password", &password),
        ]))
        .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/json",
        "node-tak compares this header with `===`; a charset parameter hands it a raw string",
    );
    assert_eq!(
        response
            .headers()
            .get(actix_web::http::header::CACHE_CONTROL)
            .unwrap(),
        "no-store",
    );

    let body: serde_json::Value = test::read_body_json(response).await;

    assert!(body["access_token"].is_string());
    assert_eq!(body["token_type"], "Bearer");
    assert!(body["expires_in"].is_number());
    assert!(
        body.get("refresh_token").is_none(),
        "the password grant issues no refresh token: {body}",
    );
    assert!(
        body.get("scope").is_none(),
        "TAK Server's own password grant returns no scope, and CloudTAK expects none: {body}",
    );
}

#[actix_web::test]
async fn the_token_survives_cloudtaks_parser() {
    let server = TestServer::start().await;
    let password = with_password(&server, "ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let body: serde_json::Value = test::call_and_read_body_json(
        &app,
        grant(form(&[
            ("grant_type", "password"),
            ("username", "ada"),
            ("password", &password),
        ]))
        .to_request(),
    )
    .await;

    let token = body["access_token"].as_str().expect("an access token");
    let mut segments = token.split('.');
    let header = segments.next().expect("a header segment");
    let payload = segments.next().expect("a payload segment");

    assert_eq!(
        header.len() % 4,
        0,
        "the header's base64url encoding must be a multiple of four characters, \
         or CloudTAK's whole-token decode does not land on the payload boundary",
    );

    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .expect("the payload decodes");
    let claims: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&decoded).expect("the payload is JSON");

    assert_eq!(
        claims["sub"], "ada",
        "`sub` is the only claim CloudTAK reads"
    );

    for (name, value) in &claims {
        assert!(
            !value.is_object(),
            "claim `{name}` is a nested object, which breaks CloudTAK's brace-splitting parser",
        );
    }

    assert_eq!(
        String::from_utf8_lossy(&decoded).matches('}').count(),
        1,
        "the payload must contain exactly one closing brace",
    );
}

#[actix_web::test]
async fn a_password_that_is_not_the_one_minted_is_an_invalid_grant() {
    let server = TestServer::start().await;
    with_password(&server, "ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let response = test::call_service(
        &app,
        grant(form(&[
            ("grant_type", "password"),
            ("username", "ada"),
            ("password", "not-the-password"),
        ]))
        .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 401);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/json",
        "CloudTAK requires the failure body to parse as JSON",
    );

    let body: serde_json::Value = test::read_body_json(response).await;

    assert_eq!(body["error"], "invalid_grant");
    assert!(
        body["error_description"]
            .as_str()
            .is_some_and(|text| text.contains("Bad credentials")),
        "CloudTAK sniffs this substring for a friendlier message: {body}",
    );
    assert!(body.get("access_token").is_none());
}

#[actix_web::test]
async fn an_account_that_does_not_exist_is_refused_the_same_way() {
    // The difference between "no such account" and "wrong secret" is an oracle.
    let server = TestServer::start().await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let body: serde_json::Value = test::call_and_read_body_json(
        &app,
        grant(form(&[
            ("grant_type", "password"),
            ("username", "nobody"),
            ("password", "whatever"),
        ]))
        .to_request(),
    )
    .await;

    assert_eq!(body["error"], "invalid_grant");
}

#[actix_web::test]
async fn an_enrolment_token_cannot_become_a_password_grant() {
    // `Purpose` is the whole of "where a secret is accepted": a one-time token
    // enrols and does nothing else.
    let server = TestServer::start().await;
    let user = server.user("ada", false).await;
    let actor = Username::parse("ada").unwrap();
    let token = mint(
        server.db(),
        &server.config().auth,
        &user,
        MintRequest::new(CredentialKind::EnrollmentToken, "Phone", &actor),
    )
    .await
    .unwrap();

    let app = test::init_service(App::new().configure(server.app())).await;

    let response = test::call_service(
        &app,
        grant(form(&[
            ("grant_type", "password"),
            ("username", "ada"),
            ("password", token.secret.expose()),
        ]))
        .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 401);
}

#[actix_web::test]
async fn the_grants_this_endpoint_does_not_serve_say_so() {
    let server = TestServer::start().await;
    let app = test::init_service(App::new().configure(server.app())).await;

    for (body, status, error) in [
        (
            form(&[("grant_type", "client_credentials")]),
            400,
            "unsupported_grant_type",
        ),
        (form(&[("grant_type", "password")]), 400, "invalid_request"),
        (
            form(&[("grant_type", "refresh_token")]),
            400,
            "invalid_request",
        ),
    ] {
        let response = test::call_service(&app, grant(body.clone()).to_request()).await;

        assert_eq!(response.status().as_u16(), status, "{body}");
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json",
            "{body}",
        );

        let parsed: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(parsed["error"], error, "{body}");
    }
}

#[actix_web::test]
async fn guessing_a_password_is_rate_limited() {
    let server = TestServer::start_with(|config| {
        config.auth.rate_limit.attempts = 3;
    })
    .await;
    with_password(&server, "ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let mut statuses = Vec::new();

    for _ in 0..6 {
        let response = test::call_service(
            &app,
            grant(form(&[
                ("grant_type", "password"),
                ("username", "ada"),
                ("password", "wrong"),
            ]))
            .to_request(),
        )
        .await;

        statuses.push(response.status().as_u16());
    }

    assert!(
        statuses.contains(&429),
        "a password endpoint with no lockout is a password endpoint being brute forced: {statuses:?}",
    );

    let refused = test::call_service(
        &app,
        grant(form(&[
            ("grant_type", "password"),
            ("username", "ada"),
            ("password", "wrong"),
        ]))
        .to_request(),
    )
    .await;

    assert_eq!(refused.status().as_u16(), 429);
    assert!(
        refused
            .headers()
            .get(actix_web::http::header::RETRY_AFTER)
            .is_some(),
        "a lockout has to say when to come back",
    );
    assert_eq!(
        refused.headers().get(CONTENT_TYPE).unwrap(),
        "application/json",
    );
}

#[actix_web::test]
async fn the_signing_key_is_published_in_both_shapes() {
    let server = TestServer::start().await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let spring: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/oauth/token_key")
            .to_request(),
    )
    .await;

    assert_eq!(spring["alg"], "SHA256withRSA");
    assert!(
        spring["value"]
            .as_str()
            .is_some_and(|pem| pem.contains("BEGIN PUBLIC KEY")),
        "token_key carries the public key as PEM: {spring}",
    );

    let jwks: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get().uri("/oauth/jwks").to_request(),
    )
    .await;

    let keys = jwks["keys"].as_array().expect("a key set");
    assert!(!keys.is_empty());
    assert_eq!(keys[0]["kty"], "RSA");
    assert_eq!(keys[0]["alg"], "RS256");
    assert!(
        keys.iter().all(|key| key.get("d").is_none()),
        "a key set must never carry a private exponent: {jwks}",
    );
}

#[actix_web::test]
async fn nothing_under_oauth_ever_redirects() {
    // node-tak reads any status below 400 as success and parses the redirect's
    // empty body as the payload.
    let server = TestServer::start().await;
    let app = test::init_service(App::new().configure(server.app())).await;

    for (method, uri) in [
        ("POST", "/oauth/token/"),
        ("POST", "//oauth/token"),
        ("GET", "/oauth/token"),
        ("GET", "/oauth/nothing-here"),
        ("GET", "/oauth/token_key/"),
    ] {
        let response = test::call_service(
            &app,
            test::TestRequest::default()
                .method(method.parse().unwrap())
                .uri(uri)
                .to_request(),
        )
        .await;

        assert!(
            !response.status().is_redirection(),
            "{method} {uri} answered {}",
            response.status(),
        );
    }
}

#[actix_web::test]
async fn a_token_from_the_password_grant_is_one_the_marti_surface_accepts() {
    // The point of the grant: CloudTAK uses the token it gets here on
    // `GET /Marti/api/tls/config` during enrolment.
    let server = TestServer::start().await;
    let password = with_password(&server, "ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let body: serde_json::Value = test::call_and_read_body_json(
        &app,
        grant(form(&[
            ("grant_type", "password"),
            ("username", "ada"),
            ("password", &password),
        ]))
        .to_request(),
    )
    .await;

    let token = body["access_token"].as_str().unwrap().to_string();

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/util/user/roles")
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);
}
