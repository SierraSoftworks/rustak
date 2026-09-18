//! One whole ACME exchange, against a directory we serve ourselves.
//!
//! Pebble — the reference ACME test server — is what this would ideally run
//! against, and it is a Go binary this build cannot fetch. So the directory is
//! `wiremock` and the *client* is the real one: `instant-acme` signs every
//! request with a real account key, walks the real state machine, and parses
//! real JSON. What is faked is the authority's side of the conversation and
//! its decision to say "valid", which is the one part no test could
//! legitimately exercise anyway. The manual check against Let's Encrypt
//! staging is written down in `docs/deployment.md`.
//!
//! What this covers that the in-file tests cannot:
//!
//! - an account being registered, sealed, and **reused** on the next call;
//! - an `http-01` order end to end — including the fallback from the
//!   configured `tls-alpn-01`, which this authority does not offer — with the
//!   answer checked at the moment the authority would fetch it and checked
//!   again after it must be gone;
//! - the renewal decision: a second run against a fresh certificate places no
//!   order, a forced one does, and a refusal backs off instead of hammering.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rustak_api::TlsCertificateState;
use rustak_server::config::{AcmeChallenge, AcmeDirectory, KeyType, TlsMode};
use rustak_server::pki::acme;
use rustak_server::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// The name every order in this suite is placed for.
const DOMAIN: &str = "tak.example.com";

/// A faked ACME directory, and what it saw.
struct Authority {
    server: MockServer,
    /// This authority's challenge token. Unique per test, because the answer
    /// map is process-wide and these tests run concurrently.
    token: String,
    /// What `challenge::answer` returned at the moment `set_ready` arrived —
    /// which is when a real authority would fetch it.
    armed: Arc<Mutex<Option<String>>>,
    /// The chain it issued, once it has signed one.
    issued: Arc<Mutex<Option<String>>>,
}

impl Authority {
    /// Stands up a directory that validates anything and issues a chain.
    async fn start() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);

        let server = MockServer::start().await;
        let base = server.uri();
        let token = format!("http-01-token-{}", NEXT.fetch_add(1, Ordering::SeqCst));
        let armed = Arc::new(Mutex::new(None));
        // Set by `/finalize`, which signs the CSR it was sent, and read by the
        // order resource and the certificate resource: the order says "ready"
        // before finalisation and "valid" after it, for every order rather
        // than only the first.
        let issued = Arc::new(Mutex::new(None));

        directory(&server, &base).await;
        account(&server, &base).await;
        order(&server, &base, &token, Arc::clone(&issued)).await;
        challenge(&server, token.clone(), Arc::clone(&armed)).await;
        certificate(&server, Arc::clone(&issued)).await;

        Self {
            server,
            token,
            armed,
            issued,
        }
    }

    /// How many requests this directory saw for `path`.
    async fn hits(&self, path: &str) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| request.url.path() == path)
            .count()
    }

    /// A server configured to order from this authority.
    async fn client(&self) -> AppContext {
        let directory = format!("{}/directory", self.server.uri());

        AppContext::new_mock(move |config| {
            config.web.public.tls.mode = TlsMode::Acme;
            config.acme.enabled = true;
            config.acme.accept_tos = true;
            config.acme.contact = Some("ops@example.com".to_string());
            config.acme.domains = vec![DOMAIN.to_string()];
            // Deliberately the *other* challenge: this authority offers only
            // `http-01`, so the fallback has to find it.
            config.acme.challenge = AcmeChallenge::TlsAlpn01;
            // Built directly rather than parsed: `AcmeDirectory::from_str`
            // refuses anything that is not https, and wiremock speaks http.
            config.acme.directory = AcmeDirectory::Url(directory.clone());
            // An RSA key would put a second of bignum arithmetic in every run.
            config.pki.key_type = KeyType::EcdsaP256;
        })
        .await
        .unwrap()
    }
}

/// Signs the CSR out of a `finalize` request, as an authority would.
///
/// This is what makes the exchange real rather than a shape: the certificate
/// carries the public key from *our* signing request, so the key rustak sealed
/// and the chain it stored have to go together or nothing works. rcgen
/// verifies the request's signature on the way in, so a malformed CSR fails
/// here rather than silently.
fn sign(body: &[u8]) -> String {
    use base64::Engine as _;
    use chrono::Datelike as _;

    let envelope: serde_json::Value = serde_json::from_slice(body).expect("JOSE JSON");
    let payload = base64::prelude::BASE64_URL_SAFE_NO_PAD
        .decode(envelope["payload"].as_str().expect("a payload"))
        .expect("base64url");
    let payload: serde_json::Value = serde_json::from_slice(&payload).expect("a finalize request");
    let der = base64::prelude::BASE64_URL_SAFE_NO_PAD
        .decode(payload["csr"].as_str().expect("a csr"))
        .expect("base64url");

    let request = rcgen::CertificateSigningRequestParams::from_der(&der.into())
        .expect("rustak's signing request has to parse and verify");

    let authority_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let mut authority_params = rcgen::CertificateParams::default();
    authority_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    authority_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "wiremock ACME CA");
    let authority = rcgen::CertifiedIssuer::self_signed(authority_params, authority_key).unwrap();

    let mut leaf = request;
    let now = chrono::Utc::now();
    let end = now + chrono::Duration::days(90);
    leaf.params.not_before = rcgen::date_time_ymd(now.year(), now.month() as u8, now.day() as u8);
    leaf.params.not_after = rcgen::date_time_ymd(end.year(), end.month() as u8, end.day() as u8);

    // Leaf first, then the issuer, which is the order a chain is served in.
    format!(
        "{}{}",
        leaf.signed_by(&authority).unwrap().pem(),
        authority.pem()
    )
}

async fn directory(server: &MockServer, base: &str) {
    Mock::given(method("GET"))
        .and(path("/directory"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "newNonce": format!("{base}/new-nonce"),
            "newAccount": format!("{base}/new-account"),
            "newOrder": format!("{base}/new-order"),
            "revokeCert": format!("{base}/revoke-cert"),
            "keyChange": format!("{base}/key-change"),
        })))
        .mount(server)
        .await;

    Mock::given(method("HEAD"))
        .and(path("/new-nonce"))
        .respond_with(ResponseTemplate::new(200).insert_header("replay-nonce", "nonce-0"))
        .mount(server)
        .await;
}

async fn account(server: &MockServer, base: &str) {
    Mock::given(method("POST"))
        .and(path("/new-account"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("location", format!("{base}/account/1").as_str())
                .insert_header("replay-nonce", "nonce-account")
                .set_body_json(serde_json::json!({ "status": "valid" })),
        )
        .mount(server)
        .await;
}

async fn order(server: &MockServer, base: &str, token: &str, issued: Arc<Mutex<Option<String>>>) {
    let pending = serde_json::json!({
        "status": "pending",
        "authorizations": [format!("{base}/authz/1")],
        "finalize": format!("{base}/finalize"),
    });
    let starting = Arc::clone(&issued);

    Mock::given(method("POST"))
        .and(path("/new-order"))
        .respond_with({
            let location = format!("{base}/order/1");

            move |_: &Request| {
                if let Ok(mut held) = starting.lock() {
                    *held = None;
                }

                ResponseTemplate::new(201)
                    .insert_header("location", location.as_str())
                    .insert_header("replay-nonce", "nonce-order")
                    .set_body_json(pending.clone())
            }
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/authz/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("replay-nonce", "nonce-authz")
                .set_body_json(serde_json::json!({
                    "identifier": { "type": "dns", "value": DOMAIN },
                    "status": "pending",
                    "challenges": [{
                        "type": "http-01",
                        "url": format!("{base}/challenge/1"),
                        "token": token,
                        "status": "pending",
                    }],
                })),
        )
        .mount(server)
        .await;

    let ready = serde_json::json!({
        "status": "ready",
        "authorizations": [format!("{base}/authz/1")],
        "finalize": format!("{base}/finalize"),
    });
    let valid = serde_json::json!({
        "status": "valid",
        "authorizations": [format!("{base}/authz/1")],
        "finalize": format!("{base}/finalize"),
        "certificate": format!("{base}/cert/1"),
    });
    let polling = Arc::clone(&issued);

    Mock::given(method("POST"))
        .and(path("/order/1"))
        .respond_with(move |_: &Request| {
            let done = polling.lock().is_ok_and(|held| held.is_some());

            ResponseTemplate::new(200)
                .insert_header("replay-nonce", "nonce-poll")
                .set_body_json(if done { valid.clone() } else { ready.clone() })
        })
        .mount(server)
        .await;

    let processing = serde_json::json!({
        "status": "processing",
        "authorizations": [format!("{base}/authz/1")],
        "finalize": format!("{base}/finalize"),
    });

    Mock::given(method("POST"))
        .and(path("/finalize"))
        .respond_with(move |request: &Request| {
            if let Ok(mut held) = issued.lock() {
                *held = Some(sign(&request.body));
            }

            ResponseTemplate::new(200)
                .insert_header("replay-nonce", "nonce-finalize")
                .set_body_json(processing.clone())
        })
        .mount(server)
        .await;
}

async fn challenge(server: &MockServer, token: String, armed: Arc<Mutex<Option<String>>>) {
    let answered = serde_json::json!({
        "type": "http-01",
        "url": "unused",
        "token": token,
        "status": "processing",
    });

    Mock::given(method("POST"))
        .and(path("/challenge/1"))
        .respond_with(move |_: &Request| {
            // This is the moment a real authority fetches
            // `/.well-known/acme-challenge/<token>`. Recording what we would
            // have answered *here* is what makes the publish/withdraw pair
            // testable at all: afterwards there is nothing left to see.
            if let Ok(mut held) = armed.lock() {
                *held = acme::challenge::answer(&token);
            }

            ResponseTemplate::new(200)
                .insert_header("replay-nonce", "nonce-challenge")
                .set_body_json(answered.clone())
        })
        .mount(server)
        .await;
}

async fn certificate(server: &MockServer, issued: Arc<Mutex<Option<String>>>) {
    Mock::given(method("POST"))
        .and(path("/cert/1"))
        .respond_with(move |_: &Request| {
            let chain = issued
                .lock()
                .ok()
                .and_then(|held| held.clone())
                .unwrap_or_default();

            ResponseTemplate::new(200)
                .insert_header("replay-nonce", "nonce-cert")
                .insert_header("content-type", "application/pem-certificate-chain")
                .set_body_string(chain)
        })
        .mount(server)
        .await;
}

#[tokio::test]
async fn an_account_is_registered_once_and_reused_afterwards() {
    let authority = Authority::start().await;
    let context = authority.client().await;
    let config = context.config();

    for _ in 0..2 {
        acme::account::ensure(
            context.db(),
            context.secrets(),
            &config.acme,
            context.http_client(),
        )
        .await
        .map_err(|err| err.to_string())
        .expect("the directory answers, so an account can be registered");
    }

    assert_eq!(
        authority.hits("/new-account").await,
        1,
        "an account key is the one thing that must survive a restart",
    );
}

#[tokio::test]
async fn one_http_01_order_produces_a_certificate_the_listener_can_serve() {
    let authority = Authority::start().await;
    let context = authority.client().await;
    let resolver = rustak_server::pki::HotSwapCertResolver::new(None);

    let state = acme::run(&context, Some(Arc::clone(&resolver)), false)
        .await
        .expect("the faked authority validates everything");

    assert!(matches!(state, acme::CertState::Valid { .. }));

    // The answer was armed when the authority came to look …
    let armed = authority.armed.lock().unwrap().clone();
    assert_eq!(
        armed
            .as_deref()
            .and_then(|answer| answer.split_once('.'))
            .map(|(token, _)| token),
        Some(authority.token.as_str()),
        "the key authorization is `<token>.<account key thumbprint>`",
    );

    // … and gone afterwards, because a path that keeps serving a secret is a
    // path somebody will eventually find.
    assert_eq!(acme::challenge::answer(&authority.token), None);

    let stored = acme::store::load(context.db(), &[DOMAIN.to_string()])
        .await
        .unwrap()
        .expect("a finished order stores its chain");

    assert!(stored.is_issued());
    assert_eq!(
        stored.chain_pem,
        authority.issued.lock().unwrap().clone().unwrap(),
        "what was stored is what the authority signed, chain and all",
    );
    assert_eq!(
        stored.chain_pem.matches("BEGIN CERTIFICATE").count(),
        2,
        "the issuer travels with the leaf, or no client can build a path",
    );
    assert_eq!(
        stored.challenge_type,
        Some(AcmeChallenge::Http01),
        "the configured tls-alpn-01 was not offered, so http-01 was used",
    );
    assert_eq!(stored.attempts, 0);
    assert!(
        stored.certified(context.secrets()).is_ok(),
        "the stored key must open under the row it was sealed for, and match the chain",
    );

    assert!(
        resolver.is_ready(),
        "the listener is swapped over without a restart",
    );
}

#[tokio::test]
async fn a_second_run_against_a_fresh_certificate_places_no_order() {
    // The property the authority's rate limits depend on: the hourly check is
    // a row read, not a request.
    let authority = Authority::start().await;
    let context = authority.client().await;

    acme::run(&context, None, false).await.unwrap();
    let after_first = authority.hits("/new-order").await;

    let state = acme::run(&context, None, false).await.unwrap();

    assert_eq!(authority.hits("/new-order").await, after_first);
    assert!(matches!(state, acme::CertState::Valid { .. }));

    let reported = acme::status(&context).await.unwrap();
    assert_eq!(reported.state, TlsCertificateState::Valid);
    assert_eq!(reported.domains, vec![DOMAIN.to_string()]);
    assert_eq!(reported.challenge.as_deref(), Some("http-01"));
    assert!(reported.renews_at.is_some());
    assert!(!reported.needs_attention());
}

#[tokio::test]
async fn a_forced_run_orders_again_even_though_nothing_is_due() {
    // What `POST /api/v1/settings/tls/renew` is for, and why it is not the
    // default: it spends real rate limit.
    let authority = Authority::start().await;
    let context = authority.client().await;

    acme::run(&context, None, false).await.unwrap();
    let after_first = authority.hits("/new-order").await;

    acme::run(&context, None, true).await.unwrap();

    assert_eq!(authority.hits("/new-order").await, after_first + 1);
}

#[tokio::test]
async fn an_authority_that_refuses_the_order_is_recorded_rather_than_retried_at_once() {
    let authority = Authority::start().await;
    let context = authority.client().await;

    // A refusal in front of the working order resource: wiremock takes the
    // first matching mock in priority order, and 1 beats the default 5.
    Mock::given(method("POST"))
        .and(path("/new-order"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("replay-nonce", "nonce-refused")
                .set_body_json(serde_json::json!({
                    "type": "urn:ietf:params:acme:error:rateLimited",
                    "detail": "too many certificates already issued for this name",
                })),
        )
        .with_priority(1)
        .mount(&authority.server)
        .await;

    let refused = acme::run(&context, None, false)
        .await
        .expect_err("a refused order is not a certificate");

    assert!(
        refused.to_string().contains("too many certificates"),
        "the authority's own words are what an operator has to act on: {refused}",
    );

    let stored = acme::store::load(context.db(), &[DOMAIN.to_string()])
        .await
        .unwrap()
        .expect("a failed order still reserves its row");

    assert_eq!(stored.attempts, 1);
    assert!(stored.last_error.is_some());

    // And the back-off is in force, so the next scheduled check does nothing.
    assert!(matches!(
        acme::decide(
            Some(&stored),
            chrono::Duration::days(30),
            chrono::Utc::now()
        ),
        acme::Decision::BackOff { .. },
    ));

    let reported = acme::status(&context).await.unwrap();
    assert_eq!(reported.state, TlsCertificateState::Failed);
    assert!(reported.needs_attention());
}
