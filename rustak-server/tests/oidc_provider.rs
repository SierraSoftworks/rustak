//! rustak as an **OpenID Connect provider**, played against by a generic
//! relying party.
//!
//! The point of this suite is the last two steps, not the first five. CloudTAK's
//! forthcoming single sign-on (dfpc-coe/CloudTAK#661) makes CloudTAK a relying
//! party: it discovers rustak, sends the browser to `/oauth/authorize`,
//! exchanges the code at `/oauth/token` with a client secret and **no** proof
//! key, reads `/oauth/userinfo` — and then enrols a TAK certificate with the
//! access token it ended up holding as a `Bearer` on `/Marti/api/tls/config`
//! and `/Marti/api/tls/signClient/v2`. That token is rustak's own, so the
//! enrolment routes already accept it; `a_relying_party_signs_somebody_in_and_
//! enrols_them` proves the whole chain, ending at a real mutually
//! authenticated handshake with the certificate it produced.
//!
//! Everything else here is a negative, because the positive path working says
//! nothing about whether the controls are still in place:
//!
//! | Test | The control it holds |
//! |---|---|
//! | `a_wrong_secret_is_invalid_client` | client authentication |
//! | `a_missing_secret_is_invalid_client` | …and it is not optional |
//! | `a_public_client_still_cannot_skip_its_proof_key` | PKCE stays mandatory where it matters |
//! | `a_confidential_client_that_offered_a_proof_key_is_held_to_it` | opting in is not opting out |
//! | `a_flow_without_openid_gets_no_id_token` | scope decides the token |
//! | `the_id_token_echoes_the_nonce_byte_for_byte` | the nonce binds the token to the flow |
//! | `userinfo_without_a_token_is_refused` | userinfo is not public |
//! | `an_unregistered_sign_out_uri_redirects_nowhere` | no open redirector on `/logout` |
//! | `the_password_grant_body_is_exactly_the_three_keys_it_has_always_been` | CloudTAK's login is untouched |
//! | `a_half_configured_client_is_refused_before_the_server_starts` | `--check` |
//!
//! `TestIdentityProvider` stands in for the upstream identity provider rustak
//! federates to, exactly as in `oauth_flows.rs`: the relying party under test
//! is downstream of rustak, and rustak is still a relying party of something
//! else. Both halves run in one request chain here.
//!
//! Run with `cargo test -p rustak-server --features testing`.

#![cfg(feature = "testing")]

use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use actix_web::http::StatusCode;
use actix_web::http::header::{ACCEPT, AUTHORIZATION, LOCATION, SET_COOKIE};
use actix_web::{App, test};
use base64::Engine as _;
use rustak_core::config::ListenAddr;
use rustak_server::config::{OAuthClient, OAuthServerConfig};
use rustak_server::pki::{KeyType, Pki, generate_key, pem};
use rustak_server::prelude::*;
use rustak_server::testing::TestServer;
use rustak_server::testing::oidc::TestIdentityProvider;

/// The confidential client the relying party is registered as.
const CLIENT: &str = "cloudtak";

/// Its secret, as `[auth.oauth] clients … secret` would carry it.
const SECRET: &str = "a-client-secret-nobody-else-has";

/// The one URI a code may be delivered to.
const REDIRECT: &str = "https://map.example.com/api/login/oidc/callback";

/// The one URI a sign-out may return to.
const POST_LOGOUT: &str = "https://map.example.com/";

/// A public client registered beside it, for the negatives that need one.
const PUBLIC_CLIENT: &str = "webtak";
const PUBLIC_REDIRECT: &str = "https://tak.example.com/login/redirect.html";

/// The scopes a relying party asks for by default.
const SCOPES: &str = "openid profile email groups";

/// A verifier a client would keep, and its `S256` challenge.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

/// How long any single network exchange may take before the test fails rather
/// than hangs.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The two registered clients every test here starts with.
fn clients() -> OAuthServerConfig {
    OAuthServerConfig {
        clients: vec![
            OAuthClient {
                id: CLIENT.to_string(),
                redirect_uris: vec![REDIRECT.to_string()],
                public: false,
                secret: Some(SECRET.to_string()),
                post_logout_redirect_uris: vec![POST_LOGOUT.to_string()],
            },
            OAuthClient {
                id: PUBLIC_CLIENT.to_string(),
                redirect_uris: vec![PUBLIC_REDIRECT.to_string()],
                public: true,
                secret: None,
                post_logout_redirect_uris: Vec::new(),
            },
        ],
        ..OAuthServerConfig::default()
    }
}

/// A server with both clients registered and a provider to federate with.
async fn server_with(provider: &TestIdentityProvider) -> TestServer {
    let oidc = provider.config();

    TestServer::start_with(move |config| {
        config.auth.oauth = clients();
        config.auth.oidc = Some(oidc);
        config.auth.user_acl = Some(filt_rs::Filter::new("true").unwrap());
    })
    .await
}

/// The authorization request a relying party builds: `scope`, `state`, `nonce`
/// and, deliberately, **no** `code_challenge`.
fn authorize_uri(scope: Option<&str>, nonce: Option<&str>) -> String {
    let mut uri = format!(
        "/oauth/authorize?response_type=code&client_id={CLIENT}&redirect_uri={}&state=rp-state",
        urlencode(REDIRECT),
    );

    if let Some(scope) = scope {
        uri.push_str(&format!("&scope={}", urlencode(scope)));
    }

    if let Some(nonce) = nonce {
        uri.push_str(&format!("&nonce={}", urlencode(nonce)));
    }

    uri
}

/// The form a token request is posted as.
fn token_form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{key}={}", urlencode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-encodes a value. Written out rather than pulled in.
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

/// The `Location` a response redirects to.
fn location(response: &actix_web::dev::ServiceResponse) -> String {
    response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// One `Set-Cookie` value, by name.
fn cookie_value(response: &actix_web::dev::ServiceResponse, prefix: &str) -> String {
    response
        .headers()
        .get_all(SET_COOKIE)
        .filter_map(|value| value.to_str().ok())
        .find_map(|cookie| cookie.strip_prefix(prefix))
        .and_then(|rest| rest.split(';').next())
        .unwrap_or_else(|| panic!("the sign-in sets a {prefix} cookie"))
        .to_string()
}

/// The base64url SHA-256 of a value, as the `state` rule uses it.
fn sha256_b64(value: &str) -> String {
    use sha2::{Digest as _, Sha256};

    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}

/// `Authorization: Basic <base64(client_id:client_secret)>`.
fn basic(id: &str, secret: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{id}:{secret}"))
    )
}

/// Drives an authorization request all the way to the relying party's redirect
/// and returns the code it carries, plus the `state` that came back.
///
/// A macro rather than a function because `test::call_service` takes
/// `actix_http::Request`, which this crate cannot name in a signature.
macro_rules! code_for {
    ($app:expr, $start:expr $(,)?) => {{
        // 1. The browser arrives with no session, so rustak starts a flow of
        //    its own against the upstream provider.
        let response =
            test::call_service($app, test::TestRequest::get().uri($start).to_request()).await;

        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "{}",
            location(&response)
        );

        let state = cookie_value(&response, "state=");
        let binding = cookie_value(&response, "__Host-rustak_login=");
        let at_provider = location(&response);

        assert_eq!(
            param(&at_provider, "state").as_deref(),
            Some(sha256_b64(&state).as_str()),
        );

        // 2. The provider signs somebody in and sends the browser back.
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

        // 3. `/login/redirect` finishes the relying party's request.
        let response = test::call_service(
            $app,
            test::TestRequest::get()
                .uri(&format!(
                    "/login/redirect?{}",
                    query.query().unwrap_or_default()
                ))
                .insert_header((
                    "cookie",
                    format!("state={state}; __Host-rustak_login={binding}"),
                ))
                .to_request(),
        )
        .await;

        let back = location(&response);

        assert!(back.starts_with(REDIRECT), "{back}");

        (
            param(&back, "code").expect("the redirect carries a code"),
            param(&back, "state"),
        )
    }};
}

/// Posts a token request and returns its status and body.
macro_rules! exchange {
    ($app:expr, $pairs:expr $(,)?) => {
        exchange!($app, $pairs, None::<String>)
    };
    ($app:expr, $pairs:expr, $authorization:expr $(,)?) => {{
        let mut request = test::TestRequest::post()
            .uri("/oauth/token")
            .insert_header(("content-type", "application/x-www-form-urlencoded"));

        if let Some(value) = $authorization {
            request = request.insert_header((AUTHORIZATION, value));
        }

        let response =
            test::call_service($app, request.set_payload(token_form($pairs)).to_request()).await;
        let status = response.status();
        let body: serde_json::Value = test::read_body_json(response).await;

        (status, body)
    }};
}

/// The fields a confidential client posts to redeem `code`.
fn confidential_grant<'a>(code: &'a str, secret: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", REDIRECT),
        ("client_id", CLIENT),
    ];

    if let Some(secret) = secret {
        form.push(("client_secret", secret));
    }

    form
}

/// The claims of a token, without checking its signature.
fn payload(token: &str) -> serde_json::Value {
    let segment = token.split('.').nth(1).expect("a JWT has three segments");
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .expect("the payload is base64url");

    serde_json::from_slice(&decoded).expect("the payload is JSON")
}

mod relying_party {
    use super::*;

    /// A server with an authority and a Marti listener on a real socket.
    ///
    /// The socket is bound here and handed to the server rather than described
    /// to it, and the listener is probed below TLS until it answers — both for
    /// the reasons `enroll_flows.rs` sets out at length.
    struct Harness {
        server: TestServer,
        pki: Arc<Pki>,
        port: u16,
        handle: actix_web::dev::ServerHandle,
    }

    impl Harness {
        fn url(&self, path: &str) -> String {
            format!("https://localhost:{}{path}", self.port)
        }

        fn root(&self) -> reqwest::Certificate {
            reqwest::Certificate::from_pem(
                pem::pem_certificate(self.pki.ca().certificate()).as_bytes(),
            )
            .expect("our own authority is a certificate reqwest can read")
        }

        async fn stop(self) {
            self.handle.stop(false).await;
        }
    }

    async fn harness(provider: &TestIdentityProvider) -> Harness {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free port on loopback");
        let port = listener.local_addr().expect("the port we bound").port();
        let oidc = provider.config();

        let server = TestServer::start_with(move |config| {
            config.auth.oauth = clients();
            config.auth.oidc = Some(oidc);
            config.auth.user_acl = Some(filt_rs::Filter::new("true").unwrap());
            // Elliptic curve: this test creates an authority, a server
            // certificate and a client key, and RSA would dominate the run
            // time without testing anything else.
            config.pki.key_type = KeyType::EcdsaP256;
            config.pki.server_ips = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
            config.web.marti.listen = ListenAddr::new("127.0.0.1", port);
        })
        .await;

        let config = server.config();
        let pki = Pki::load(
            server.db(),
            server.secrets(),
            &config.pki,
            &config.server.data_dir,
            &["localhost".to_string()],
            &config.pki.server_ips,
        )
        .await
        .expect("an authority for the test server");

        server
            .context
            .install_pki(Arc::clone(&pki))
            .expect("the authority is installed once");

        let marti = rustak_server::web::build_marti_on(server.context.clone(), listener)
            .expect("the Marti listener serves on the socket it was handed");
        let handle = marti.handle();

        actix_web::rt::spawn(marti);
        await_marti(port).await;

        Harness {
            server,
            pki,
            port,
            handle,
        }
    }

    /// Waits until the Marti listener stops closing connections as they arrive.
    /// See `enroll_flows.rs::await_marti` for why the probe sits below TLS.
    async fn await_marti(port: u16) {
        use tokio::io::AsyncReadExt as _;

        let deadline = std::time::Instant::now() + Duration::from_secs(20);

        loop {
            if let Ok(mut stream) =
                tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await
            {
                let mut byte = [0u8; 1];

                match tokio::time::timeout(Duration::from_millis(250), stream.read(&mut byte)).await
                {
                    Err(_) | Ok(Ok(1..)) => return,
                    Ok(Ok(_) | Err(_)) => {}
                }
            }

            assert!(
                std::time::Instant::now() < deadline,
                "the Marti listener on {port} never started answering",
            );

            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// A signing request for `cn`, and the key that goes with it.
    fn signing_request(cn: &str) -> (String, rcgen::KeyPair) {
        let key = generate_key(KeyType::EcdsaP256).expect("a client key");
        let mut params = rcgen::CertificateParams::default();

        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, cn);

        let csr = params.serialize_request(&key).expect("a signing request");

        (csr.pem().expect("the request as PEM"), key)
    }

    /// Rebuilds the PEM armour a client adds around the bare base64 we return.
    fn armour(bare: &str) -> String {
        let body: String = bare.split_whitespace().collect::<Vec<_>>().join("\n");

        format!("-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n")
    }

    /// Verifies an ID token the way a relying-party library does: fetch the key
    /// set, find the key the header names, check the signature, the issuer, the
    /// audience and the expiry.
    fn verified(id_token: &str, jwks: &serde_json::Value, issuer: &str) -> serde_json::Value {
        let header = jsonwebtoken::decode_header(id_token).expect("a readable header");
        let kid = header.kid.expect("the header names a key");

        assert_eq!(header.alg, jsonwebtoken::Algorithm::RS256);

        let key = jwks["keys"]
            .as_array()
            .expect("a key set")
            .iter()
            .find(|key| key["kid"] == kid)
            .unwrap_or_else(|| panic!("no published key is called {kid}"));

        assert_eq!(key["kty"], "RSA");
        assert_eq!(key["use"], "sig");
        assert_eq!(key["alg"], "RS256");

        let decoding = jsonwebtoken::DecodingKey::from_rsa_components(
            key["n"].as_str().expect("a modulus"),
            key["e"].as_str().expect("an exponent"),
        )
        .expect("the published key is usable");

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);

        validation.set_audience(&[CLIENT]);
        validation.set_issuer(&[issuer]);
        validation.set_required_spec_claims(&["exp", "aud", "iss", "sub"]);

        jsonwebtoken::decode::<serde_json::Value>(id_token, &decoding, &validation)
            .expect("the ID token verifies against the published key set")
            .claims
    }

    #[actix_web::test]
    async fn a_relying_party_signs_somebody_in_and_enrols_them() {
        let provider = TestIdentityProvider::start().await;
        let harness = harness(&provider).await;
        let app = test::init_service(App::new().configure(harness.server.app())).await;

        // 1. Discovery, at the path OpenID Connect reserves.
        let discovery: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/.well-known/openid-configuration")
                .to_request(),
        )
        .await;

        let issuer = discovery["issuer"].as_str().expect("an issuer").to_string();

        assert_eq!(
            discovery["authorization_endpoint"],
            format!("{issuer}/oauth/authorize"),
        );
        assert_eq!(discovery["token_endpoint"], format!("{issuer}/oauth/token"));
        assert_eq!(
            discovery["userinfo_endpoint"],
            format!("{issuer}/oauth/userinfo"),
        );
        assert_eq!(discovery["jwks_uri"], format!("{issuer}/oauth/jwks"));

        // 2. The authorization request, with a nonce and no proof key at all.
        let (code, state) = code_for!(&app, &authorize_uri(Some(SCOPES), Some("rp-nonce")));

        assert_eq!(
            state.as_deref(),
            Some("rp-state"),
            "the relying party's own state has to come back untouched",
        );

        // 3. The exchange, with `client_secret_post`.
        let (status, body) = exchange!(&app, &confidential_grant(&code, Some(SECRET)));

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["token_type"], "Bearer");
        assert_eq!(
            body["scope"], SCOPES,
            "the granted OpenID scopes, space-separated: {body}",
        );

        let token = body["access_token"].as_str().expect("an access token");
        let id_token = body["id_token"].as_str().expect("an ID token");

        // 4. The ID token, verified against the published key set.
        let jwks: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get().uri("/oauth/jwks").to_request(),
        )
        .await;

        let claims = verified(id_token, &jwks, &issuer);

        assert_eq!(claims["sub"], "alice");
        assert_eq!(claims["nonce"], "rp-nonce");
        assert_eq!(claims["preferred_username"], "alice");
        assert!(claims["auth_time"].is_i64());

        // 5. Userinfo, with the access token as a bearer.
        let userinfo: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/oauth/userinfo")
                .insert_header((AUTHORIZATION, format!("Bearer {token}")))
                .to_request(),
        )
        .await;

        assert_eq!(userinfo["sub"], "alice");
        assert_eq!(
            userinfo["preferred_username"], "alice",
            "the claim CloudTAK reads first: {userinfo}",
        );
        assert_eq!(userinfo["email"], "alice@example.com");
        assert!(
            userinfo["groups"]
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == "ops")),
            "the channels the provider's claim mapped: {userinfo}",
        );
        assert_eq!(
            userinfo["sub"], claims["sub"],
            "one identity, or the certificate below names somebody else",
        );

        // 6. Enrolment, with that same access token as a `Bearer` — which is
        //    the whole reason rustak is the provider rather than a proxy.
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/Marti/api/tls/config")
                .insert_header((AUTHORIZATION, format!("Bearer {token}")))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);

        let (csr, key) = signing_request("alice");
        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/Marti/api/tls/signClient/v2?clientUid=alice%20(ETL)&version=3")
                .insert_header((AUTHORIZATION, format!("Bearer {token}")))
                .insert_header((ACCEPT, "application/json"))
                .set_payload(csr)
                .to_request(),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "an OpenID access token has to enrol, or CloudTAK's SSO stops here",
        );

        let body: serde_json::Value = test::read_body_json(response).await;
        let signed = body["signedCert"].as_str().expect("a signed certificate");

        // 7. And the certificate against the real mutually authenticated
        //    socket, which is the only thing that proves it all worked.
        let certificate = armour(signed);
        let identity =
            reqwest::Identity::from_pem(format!("{certificate}{}", key.serialize_pem()).as_bytes())
                .expect("the issued certificate and its key make an identity");

        let client = reqwest::Client::builder()
            .add_root_certificate(harness.root())
            .identity(identity)
            .timeout(TIMEOUT)
            .build()
            .expect("a client for the Marti listener");

        let response = client
            .get(harness.url("/Marti/api/version"))
            .send()
            .await
            .expect("the enrolled certificate completes the handshake");

        assert_eq!(response.status().as_u16(), 200);
        assert!(response.text().await.unwrap().contains("TAK Server"));

        harness.stop().await;
    }
}

mod client_authentication {
    use super::*;

    #[actix_web::test]
    async fn a_wrong_secret_is_invalid_client() {
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let (code, _) = code_for!(&app, &authorize_uri(Some(SCOPES), None));

        let (status, body) = exchange!(
            &app,
            &confidential_grant(&code, Some("not-the-secret-it-was-registered-with")),
        );

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "invalid_client");
        assert!(body.get("access_token").is_none(), "{body}");

        // And the good code is still there for the client that really holds
        // the secret: a wrong guess must not burn it.
        let (status, _) = exchange!(&app, &confidential_grant(&code, Some(SECRET)));

        assert_eq!(status, StatusCode::OK);
    }

    #[actix_web::test]
    async fn a_missing_secret_is_invalid_client() {
        // A confidential client's authentication is not optional, and the
        // refusal is the same one a wrong secret gets.
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let (code, _) = code_for!(&app, &authorize_uri(Some(SCOPES), None));

        let (status, body) = exchange!(&app, &confidential_grant(&code, None));

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "invalid_client");
    }

    #[actix_web::test]
    async fn client_secret_basic_is_accepted_beside_client_secret_post() {
        // A standard library may use either; the discovery document says so.
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let (code, _) = code_for!(&app, &authorize_uri(Some(SCOPES), None));

        let (status, body) = exchange!(
            &app,
            &confidential_grant(&code, None),
            Some(basic(CLIENT, SECRET)),
        );

        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["access_token"].is_string());
        assert!(body["id_token"].is_string());
    }

    #[actix_web::test]
    async fn a_basic_header_with_the_wrong_secret_is_refused_the_same_way() {
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let (code, _) = code_for!(&app, &authorize_uri(Some(SCOPES), None));

        let (status, body) = exchange!(
            &app,
            &confidential_grant(&code, None),
            Some(basic(CLIENT, "not-the-secret")),
        );

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "invalid_client");
    }

    #[actix_web::test]
    async fn a_public_client_still_cannot_skip_its_proof_key() {
        // It holds no secret, so nothing would stand between an intercepted
        // code and a session. The refusal is at the authorization endpoint.
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!(
                    "/oauth/authorize?response_type=code&client_id={PUBLIC_CLIENT}\
                     &redirect_uri={}&state=s&scope={}",
                    urlencode(PUBLIC_REDIRECT),
                    urlencode(SCOPES),
                ))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            param(&location(&response), "error").as_deref(),
            Some("invalid_request"),
        );
        assert!(param(&location(&response), "code").is_none());
    }

    #[actix_web::test]
    async fn a_confidential_client_that_offered_a_proof_key_is_held_to_it() {
        // Opting in must not be a way of opting out: a client that registered
        // a challenge has to present the verifier for it, secret or no secret.
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let (code, _) = code_for!(
            &app,
            &format!(
                "{}&code_challenge={CHALLENGE}&code_challenge_method=S256",
                authorize_uri(Some(SCOPES), None),
            ),
        );

        let mut wrong = confidential_grant(&code, Some(SECRET));

        wrong.push(("code_verifier", "a-verifier-somebody-else-made-up"));

        let (status, body) = exchange!(&app, &wrong);

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");

        let mut right = confidential_grant(&code, Some(SECRET));

        right.push(("code_verifier", VERIFIER));

        assert_eq!(
            exchange!(&app, &right).0,
            StatusCode::OK,
            "and the wrong verifier must not have burnt the code",
        );
    }
}

mod tokens {
    use super::*;

    #[actix_web::test]
    async fn a_flow_without_openid_gets_no_id_token() {
        // `openid` is what asks for one; a plain OAuth2 client that asked for
        // nothing must not be handed an identity assertion it never requested.
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for scope in [None, Some("profile email")] {
            let (code, _) = code_for!(&app, &authorize_uri(scope, None));
            let (status, body) = exchange!(&app, &confidential_grant(&code, Some(SECRET)));

            assert_eq!(status, StatusCode::OK, "{body}");
            assert!(body.get("id_token").is_none(), "{scope:?}: {body}");
            assert!(body["access_token"].is_string());
        }
    }

    #[actix_web::test]
    async fn a_request_that_asked_for_no_scope_still_reports_the_rustak_one() {
        // What this grant has always answered, so nothing that reads it breaks.
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let (code, _) = code_for!(&app, &authorize_uri(None, None));

        let (_, body) = exchange!(&app, &confidential_grant(&code, Some(SECRET)));

        assert_eq!(body["scope"], "api");
    }

    #[actix_web::test]
    async fn the_id_token_echoes_the_nonce_byte_for_byte() {
        // Comparing it is the relying party's job; echoing it exactly is ours,
        // and anything normalised is a mismatch nobody could see.
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for nonce in ["a-plain-nonce", "one with spaces", "Ünicode+/=", "0"] {
            let (code, _) = code_for!(&app, &authorize_uri(Some("openid"), Some(nonce)));
            let (_, body) = exchange!(&app, &confidential_grant(&code, Some(SECRET)));

            let id_token = body["id_token"].as_str().expect("an ID token");

            assert_eq!(payload(id_token)["nonce"], nonce, "{nonce}");
        }
    }

    #[actix_web::test]
    async fn a_flow_with_no_nonce_produces_a_token_with_no_nonce_claim() {
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let (code, _) = code_for!(&app, &authorize_uri(Some("openid"), None));

        let (_, body) = exchange!(&app, &confidential_grant(&code, Some(SECRET)));
        let claims = payload(body["id_token"].as_str().expect("an ID token"));

        assert!(claims.get("nonce").is_none(), "{claims}");
    }

    #[actix_web::test]
    async fn an_id_token_only_carries_the_claims_its_scopes_asked_for() {
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let (code, _) = code_for!(&app, &authorize_uri(Some("openid email"), None));

        let (_, body) = exchange!(&app, &confidential_grant(&code, Some(SECRET)));
        let claims = payload(body["id_token"].as_str().expect("an ID token"));

        assert_eq!(claims["email"], "alice@example.com");
        assert!(claims.get("name").is_none(), "{claims}");
        assert!(claims.get("groups").is_none(), "{claims}");
    }

    #[actix_web::test]
    async fn the_password_grant_body_is_exactly_the_three_keys_it_has_always_been() {
        // `compat/oauth.md` §1 pins it, and everything above is additive.
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let user = server.user("ada", false).await;
        let password = rustak_server::identity::credentials::mint(
            server.db(),
            &server.config().auth,
            &user,
            rustak_server::identity::credentials::MintRequest::new(
                rustak_api::CredentialKind::ClientPassword,
                "CloudTAK",
                &Username::parse("ada").unwrap(),
            ),
        )
        .await
        .expect("a client password")
        .secret
        .expose()
        .to_string();

        let app = test::init_service(App::new().configure(server.app())).await;
        let (status, body) = exchange!(
            &app,
            &[
                ("grant_type", "password"),
                ("username", "ada"),
                ("password", &password),
            ],
        );

        assert_eq!(status, StatusCode::OK, "{body}");

        let mut keys: Vec<&str> = body
            .as_object()
            .expect("a JSON object")
            .keys()
            .map(String::as_str)
            .collect();

        keys.sort_unstable();

        assert_eq!(
            keys,
            ["access_token", "expires_in", "token_type"],
            "no scope, no id_token, no refresh_token: {body}",
        );
    }
}

mod userinfo_and_sign_out {
    use super::*;

    #[actix_web::test]
    async fn userinfo_without_a_token_is_refused() {
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get().uri("/oauth/userinfo").to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            response
                .headers()
                .get(actix_web::http::header::WWW_AUTHENTICATE)
                .is_some(),
            "RFC 6750 §3: a library has to be told what to present",
        );
    }

    #[actix_web::test]
    async fn a_registered_sign_out_uri_is_returned_to_with_its_state() {
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!(
                    "/logout?client_id={CLIENT}&post_logout_redirect_uri={}&state=rp-state",
                    urlencode(POST_LOGOUT),
                ))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(location(&response), format!("{POST_LOGOUT}?state=rp-state"));
    }

    #[actix_web::test]
    async fn an_unregistered_sign_out_uri_redirects_nowhere() {
        // A sign-out that redirected wherever it was told is an open
        // redirector on a path every session ends at.
        let provider = TestIdentityProvider::start().await;
        let server = server_with(&provider).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for uri in [
            "https://evil.example.com/",
            "https://map.example.com/elsewhere",
            POST_LOGOUT,
        ] {
            let client = if uri == POST_LOGOUT {
                PUBLIC_CLIENT
            } else {
                CLIENT
            };
            let response = test::call_service(
                &app,
                test::TestRequest::get()
                    .uri(&format!(
                        "/logout?client_id={client}&post_logout_redirect_uri={}",
                        urlencode(uri),
                    ))
                    .to_request(),
            )
            .await;

            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{uri}");
            assert!(response.headers().get(LOCATION).is_none(), "{uri}");
        }
    }
}

mod configuration {
    use rustak_server::config::Config;

    /// What `rustak --check` would say about `[auth.oauth]` written this way.
    fn refusal(clients: &str) -> String {
        let text = format!("[auth.oauth]\nclients = [{clients}]\n");
        let Err(err) = toml::from_str::<Config>(&text)
            .map_err(|err| err.to_string())
            .and_then(|config| config.validate().map_err(|err| err.to_string()))
        else {
            panic!("`{clients}` should have been refused");
        };

        err
    }

    #[test]
    fn a_half_configured_client_is_refused_before_the_server_starts() {
        // Both halves of the rule, because either one alone is a client that
        // looks like it works and cannot complete a sign-in.
        let without = refusal(
            r#"{ id = "a", redirect_uris = ["https://a.example.com/cb"], public = false }"#,
        );

        assert!(without.contains("public = false"), "{without}");

        let with = refusal(
            r#"{ id = "a", redirect_uris = ["https://a.example.com/cb"], secret = "s3cret" }"#,
        );

        assert!(with.contains("public = true"), "{with}");
    }

    #[test]
    fn a_fully_configured_confidential_client_loads() {
        let text = concat!(
            "[auth.oauth]\n",
            "clients = [{ id = \"a\", redirect_uris = [\"https://a.example.com/cb\"], ",
            "public = false, secret = \"s3cret\", ",
            "post_logout_redirect_uris = [\"https://a.example.com/\"] }]\n",
        );

        toml::from_str::<Config>(text)
            .expect("the section parses")
            .validate()
            .expect("and is usable as written");
    }
}
