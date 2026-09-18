//! Enrolment end to end: `/Marti/api/tls/*` on the public listener, then the
//! certificate it produced against a **real** mutually authenticated socket.
//!
//! The unit tests inside `src/marti/tls.rs` check the pieces. This suite checks
//! the thing they add up to, because everything worth catching here is a wiring
//! failure that a handler tested in isolation cannot see: whether the issued
//! certificate actually completes a handshake against our own listener, whether
//! the row that makes it revocable was written, and whether a one-time token is
//! spent exactly once.
//!
//! # The two client shapes
//!
//! **CloudTAK** holds a bearer token from `/oauth/token`, asks for the config
//! with it, and then posts a **PEM** signing request with Basic and
//! `Accept: application/json`. It reassembles the PEM armour itself, so
//! `signedCert` must come back as bare base64.
//!
//! **ATAK** has a one-time token typed or scanned in, posts **bare base64** with
//! `Content-Type: application/octet-stream` and `Accept: application/xml`, and
//! reads every child of `<enrollment>` except `signedCert` as a CA.
//!
//! Run with `cargo test -p rustak-server --features testing`.

#![cfg(feature = "testing")]

use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use actix_web::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use actix_web::{App, test};
use base64::Engine as _;
use rustak_api::CredentialKind;
use rustak_core::config::ListenAddr;
use rustak_server::identity::credentials::{MintRequest, mint};
use rustak_server::pki::{KeyType, Pki, RevokeReason, generate_key, pem};
use rustak_server::prelude::*;
use rustak_server::testing::TestServer;

/// How long any single network exchange may take before the test fails rather
/// than hangs. A refused handshake must report the error rustls gave, not sit
/// waiting for a flight that will never be sent.
const TIMEOUT: Duration = Duration::from_secs(10);

/// A server with an authority, and a Marti listener bound on a real socket.
struct Harness {
    server: TestServer,
    pki: Arc<Pki>,
    port: u16,
    handle: actix_web::dev::ServerHandle,
}

impl Harness {
    /// The public listener's routes, in process.
    fn app(&self) -> impl FnOnce(&mut actix_web::web::ServiceConfig) + Clone {
        self.server.app()
    }

    /// The authority's certificate, as reqwest wants to be told about it.
    fn root(&self) -> reqwest::Certificate {
        reqwest::Certificate::from_pem(pem::pem_certificate(self.pki.ca().certificate()).as_bytes())
            .expect("our own authority is a certificate reqwest can read")
    }

    /// `https://localhost:<port><path>` on the mutually authenticated listener.
    fn url(&self, path: &str) -> String {
        format!("https://localhost:{}{path}", self.port)
    }

    async fn stop(self) {
        self.handle.stop(false).await;
    }
}

/// How many times [`harness`] will pick a different port before giving up.
///
/// Each attempt builds a whole `TestServer`, so this is deliberately small: the
/// window it covers is microseconds wide and a second collision would mean
/// something other than bad luck.
const BIND_ATTEMPTS: usize = 5;

/// A port nothing else is using, **still held**.
///
/// The configuration has to name a port before `build_marti` binds it, and a
/// listener on `:0` only reports its port once it is already bound — so the port
/// is found by binding `:0` here and the listener is handed back rather than
/// dropped. Holding it is the point: this used to return the number and close
/// the socket immediately, leaving the port unclaimed for the whole of
/// `TestServer::start_with` plus `Pki::load` — hundreds of milliseconds under
/// `cargo test --workspace`, during which any other test binding `:0` could take
/// it and this suite would fail on a port it had been promised. (M6-01 traced
/// the flake here.) The caller now releases it in the instruction before
/// `build_marti` binds it, and retries on the vanishingly small window that
/// remains.
fn reserve_port() -> (TcpListener, u16) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free port");
    let port = listener
        .local_addr()
        .expect("the port we just bound")
        .port();

    (listener, port)
}

/// Starts a server with a real authority and a bound Marti listener.
///
/// # Panics
///
/// If the Marti listener cannot be bound on [`BIND_ATTEMPTS`] different ports,
/// which is no longer a race but a machine with no ports to give.
async fn harness() -> Harness {
    for attempt in 1..=BIND_ATTEMPTS {
        match try_harness().await {
            Ok(harness) => return harness,
            Err(err) if attempt < BIND_ATTEMPTS => {
                eprintln!("enroll_flows: the reserved port was taken ({err}); trying another");
            }
            Err(err) => panic!("the Marti listener would not bind on {BIND_ATTEMPTS} ports: {err}"),
        }
    }

    unreachable!("the loop either returns or panics on its last iteration")
}

/// One attempt at [`harness`], which fails only when the port was taken.
async fn try_harness() -> Result<Harness, human_errors::Error> {
    let (reservation, port) = reserve_port();
    let server = TestServer::start_with(move |config| {
        // Elliptic curve rather than the RSA default: this suite creates an
        // authority, a server certificate and a client key per test, and RSA
        // generation would dominate the run time without testing anything the
        // `pki::` unit tests do not already cover.
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

    // Released here and nowhere earlier: everything above this line runs while
    // the port is still ours, so the gap another test could slip into is the two
    // instructions between this drop and the bind below.
    drop(reservation);

    let listener = rustak_server::web::build_marti(server.context.clone())?
        .expect("the Marti listener is enabled");
    let handle = listener.handle();

    actix_web::rt::spawn(listener);

    Ok(Harness {
        server,
        pki,
        port,
        handle,
    })
}

/// An account and a credential of `kind`, as an administrator would mint one.
async fn credential(server: &TestServer, username: &str, kind: CredentialKind) -> String {
    let user = server
        .db()
        .users()
        .get_by_username(&Username::parse(username).unwrap())
        .await
        .unwrap()
        .unwrap_or(server.user(username, false).await);
    let actor = Username::parse("ada").unwrap();

    mint(
        server.db(),
        &server.config().auth,
        &user,
        MintRequest::new(kind, "Test device", &actor),
    )
    .await
    .expect("mint the credential under test")
    .secret
    .expose()
    .to_string()
}

/// `Authorization: Basic <base64(user:secret)>`.
fn basic(username: &str, secret: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{secret}"))
    )
}

/// A signing request for `cn`, and the key that goes with it.
fn signing_request(cn: &str) -> (String, rcgen::KeyPair) {
    let key = generate_key(KeyType::EcdsaP256).expect("a client key");
    let mut params = rcgen::CertificateParams::default();

    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, cn);
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "rustak");

    let csr = params.serialize_request(&key).expect("a signing request");

    (csr.pem().expect("the request as PEM"), key)
}

/// Rebuilds the PEM armour a client adds around the bare base64 we return.
fn armour(bare: &str) -> String {
    let body: String = bare.split_whitespace().collect::<Vec<_>>().join("\n");

    format!("-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n")
}

/// A mutually authenticated client, with its own connection pool.
///
/// A fresh client per call on purpose: reqwest keeps connections alive, and a
/// test that revokes a certificate has to open a new handshake rather than
/// reuse the one opened before.
fn mtls_client(harness: &Harness, certificate: &str, key: &rcgen::KeyPair) -> reqwest::Client {
    let identity =
        reqwest::Identity::from_pem(format!("{certificate}{}", key.serialize_pem()).as_bytes())
            .expect("the issued certificate and its key make an identity");

    reqwest::Client::builder()
        .add_root_certificate(harness.root())
        .identity(identity)
        .timeout(TIMEOUT)
        .build()
        .expect("a client for the Marti listener")
}

#[actix_web::test]
async fn a_cloudtak_shaped_enrolment_produces_a_certificate_the_marti_listener_accepts() {
    let harness = harness().await;
    let password = credential(&harness.server, "ada", CredentialKind::ClientPassword).await;
    let (_, session) = harness.server.signed_in("grace", false).await;
    let app = test::init_service(App::new().configure(harness.app())).await;

    // Step one: the name entries, under the bearer token the password grant
    // handed CloudTAK. CloudTAK genuinely does use the token here and Basic on
    // the call that follows.
    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/tls/config")
            .insert_header((
                AUTHORIZATION,
                rustak_server::testing::context::bearer(&session),
            ))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/xml",
    );

    let document = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

    assert!(
        document.contains("<ns2:certificateConfig"),
        "CloudTAK indexes `config['ns2:certificateConfig']` literally: {document}",
    );
    assert!(
        document.matches("<nameEntry ").count() >= 2,
        "xml-js collapses a one-element array and CloudTAK then iterates it: {document}",
    );

    // Step two: the signing request itself, with Basic and a PEM body.
    let (csr, key) = signing_request("ada");
    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/tls/signClient/v2?clientUid=ada%20(ETL)&version=3")
            .insert_header((AUTHORIZATION, basic("ada", &password)))
            .insert_header((ACCEPT, "application/json"))
            .set_payload(csr)
            .to_request(),
    )
    .await;

    assert_eq!(
        response.status().as_u16(),
        200,
        "ATAK reads a 201 as a failure, so success is always 200",
    );
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/json",
    );

    let body: serde_json::Value = test::read_body_json(response).await;
    let signed = body["signedCert"].as_str().expect("a signed certificate");

    assert!(
        !signed.contains("BEGIN CERTIFICATE"),
        "signedCert must be bare base64; the client adds the armour itself",
    );
    assert!(body["ca0"].is_string(), "the chain the client will trust");

    // Step three: the certificate against the real mutually authenticated
    // socket, which is the only thing that proves the whole flow worked.
    let certificate = armour(signed);
    let client = mtls_client(&harness, &certificate, &key);

    let response = client
        .get(harness.url("/Marti/api/version"))
        .send()
        .await
        .expect("the enrolled certificate completes the handshake");

    assert_eq!(response.status().as_u16(), 200);

    let text = response.text().await.unwrap();
    assert!(
        text.contains("TAK Server"),
        "ATAK matches this string to decide it is talking to a TAK server: {text}",
    );

    harness.stop().await;
}

#[actix_web::test]
async fn an_atak_shaped_enrolment_answers_xml_and_spends_its_token_exactly_once() {
    let harness = harness().await;
    let token = credential(&harness.server, "ada", CredentialKind::EnrollmentToken).await;
    let app = test::init_service(App::new().configure(harness.app())).await;

    // The config call comes first and must *not* spend the token — otherwise
    // every enrolment strands the person half-way through.
    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/tls/config")
            .insert_header((AUTHORIZATION, basic("ada", &token)))
            .insert_header((ACCEPT, "application/xml"))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);

    let (csr, _) = signing_request("ada");
    let bare = csr
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");

    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/tls/signClient/v2?clientUid=ANDROID-0123456789&version=5.1.0")
            .insert_header((AUTHORIZATION, basic("ada", &token)))
            .insert_header((ACCEPT, "application/xml"))
            .insert_header((CONTENT_TYPE, "application/octet-stream"))
            .set_payload(bare)
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/xml",
    );

    let document = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

    assert!(document.contains("<enrollment>"), "{document}");
    assert!(document.contains("<signedCert>"), "{document}");
    assert!(
        document.matches("<ca>").count() >= 1,
        "the truststore ATAK will keep: {document}",
    );
    assert!(
        !document.contains("BEGIN CERTIFICATE"),
        "every element is bare base64: {document}",
    );

    // The device the certificate belongs to is recorded, so it can be revoked
    // by name later.
    assert!(
        rustak_server::identity::devices::get(
            harness.server.db(),
            &DeviceUid::from_storage("ANDROID-0123456789"),
        )
        .await
        .unwrap()
        .is_some(),
    );

    // Second use of a one-time token: refused, with the challenge that tells a
    // client to present something else.
    let (second, _) = signing_request("ada");
    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/tls/signClient/v2?clientUid=ANDROID-0123456789")
            .insert_header((AUTHORIZATION, basic("ada", &token)))
            .insert_header((ACCEPT, "application/xml"))
            .set_payload(second)
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 401);
    assert_eq!(
        response.headers().get(WWW_AUTHENTICATE).unwrap(),
        "Basic realm=\"rustak\"",
    );

    harness.stop().await;
}

#[actix_web::test]
async fn a_signing_request_naming_somebody_else_is_refused_without_issuing_anything() {
    let harness = harness().await;
    let password = credential(&harness.server, "ada", CredentialKind::ClientPassword).await;
    harness.server.user("grace", false).await;
    let app = test::init_service(App::new().configure(harness.app())).await;

    let (csr, _) = signing_request("grace");
    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/tls/signClient/v2?clientUid=ANDROID-1")
            .insert_header((AUTHORIZATION, basic("ada", &password)))
            .set_payload(csr)
            .to_request(),
    )
    .await;

    assert_eq!(
        response.status().as_u16(),
        403,
        "a request for somebody else will not start working by retrying",
    );

    let certificates = harness
        .server
        .db()
        .certificates()
        .list_of_kind(
            rustak_api::CertificateKind::Client,
            rustak_server::db::repos::Page::first(10),
        )
        .await
        .unwrap();

    assert!(
        certificates.is_empty(),
        "nothing may be issued for a refused enrolment",
    );

    harness.stop().await;
}

#[actix_web::test]
async fn an_accept_header_we_cannot_answer_is_refused_rather_than_guessed() {
    let harness = harness().await;
    let password = credential(&harness.server, "ada", CredentialKind::ClientPassword).await;
    let app = test::init_service(App::new().configure(harness.app())).await;

    let (csr, _) = signing_request("ada");
    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/tls/signClient/v2?clientUid=ANDROID-1")
            .insert_header((AUTHORIZATION, basic("ada", &password)))
            .insert_header((ACCEPT, "application/pkix-cert"))
            .set_payload(csr)
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 400);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/json",
    );

    harness.stop().await;
}

#[actix_web::test]
async fn the_legacy_endpoint_answers_a_bundle_a_client_can_open() {
    let harness = harness().await;
    let password = credential(&harness.server, "ada", CredentialKind::ClientPassword).await;
    let app = test::init_service(App::new().configure(harness.app())).await;

    let (csr, _) = signing_request("ada");
    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/tls/signClient?clientUid=ANDROID-1")
            .insert_header((AUTHORIZATION, basic("ada", &password)))
            .set_payload(csr)
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/octet-stream",
    );

    let bundle = test::read_body(response).await;
    let keystore = p12_keystore::KeyStore::from_pkcs12(
        &bundle,
        "atakatak",
        p12_keystore::Pkcs12ImportPolicy::Relaxed,
    )
    .expect("the bundle opens with the passphrase every TAK client tries first");

    assert!(
        keystore.entries().any(|(alias, _)| alias == "signedCert"),
        "the aliases the pre-v2 clients look for",
    );

    harness.stop().await;
}

#[actix_web::test]
async fn a_revoked_certificate_cannot_complete_the_handshake() {
    let harness = harness().await;
    let password = credential(&harness.server, "ada", CredentialKind::ClientPassword).await;
    let app = test::init_service(App::new().configure(harness.app())).await;

    let (csr, key) = signing_request("ada");
    let body: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/tls/signClient/v2?clientUid=ANDROID-1")
            .insert_header((AUTHORIZATION, basic("ada", &password)))
            .set_payload(csr)
            .to_request(),
    )
    .await;

    let certificate = armour(body["signedCert"].as_str().unwrap());

    // It works first.
    assert!(
        mtls_client(&harness, &certificate, &key)
            .get(harness.url("/Marti/api/version"))
            .send()
            .await
            .is_ok(),
    );

    let der = pem::parse_pem_chain(&certificate).unwrap();
    let fingerprint = pem::sha256_fingerprint(&der[0]);

    assert!(
        harness
            .pki
            .revoke(
                harness.server.db(),
                &fingerprint,
                RevokeReason::DeviceLost,
                None,
            )
            .await
            .unwrap(),
    );

    // A fresh client, so this is a new handshake rather than a pooled
    // connection opened before the revocation.
    let refused = mtls_client(&harness, &certificate, &key)
        .get(harness.url("/Marti/api/version"))
        .send()
        .await;

    assert!(
        refused.is_err(),
        "a revoked certificate must be refused at the handshake, not by a handler",
    );

    harness.stop().await;
}

#[actix_web::test]
async fn the_marti_listener_refuses_a_caller_with_no_certificate_at_all() {
    let harness = harness().await;

    let anonymous = reqwest::Client::builder()
        .add_root_certificate(harness.root())
        .timeout(TIMEOUT)
        .build()
        .unwrap();

    let refused = anonymous
        .get(harness.url("/Marti/api/version"))
        .send()
        .await;

    assert!(
        refused.is_err(),
        "`[web.marti] client_cert = \"required\"` means the handshake fails, not the request",
    );

    harness.stop().await;
}

#[actix_web::test]
async fn the_enrolment_endpoints_refuse_without_a_credential_and_say_how_to_retry() {
    let harness = harness().await;
    let app = test::init_service(App::new().configure(harness.app())).await;

    for (method, uri) in [
        ("GET", "/Marti/api/tls/config"),
        ("POST", "/Marti/api/tls/signClient/v2"),
        ("POST", "/Marti/api/tls/signClient"),
    ] {
        let response = test::call_service(
            &app,
            test::TestRequest::default()
                .method(method.parse().unwrap())
                .uri(uri)
                .to_request(),
        )
        .await;

        assert_eq!(response.status().as_u16(), 401, "{method} {uri}");
        assert_eq!(
            response.headers().get(WWW_AUTHENTICATE).unwrap(),
            "Basic realm=\"rustak\"",
            "{method} {uri}",
        );
        assert!(
            !response.status().is_redirection(),
            "{method} {uri} answered {}",
            response.status(),
        );
    }

    harness.stop().await;
}

#[actix_web::test]
async fn the_profile_endpoints_answer_no_content_until_they_have_something_to_send() {
    // M3-02 took these routes over from the 204 stubs. The connection fetch
    // still answers 204 when an installation has configured no profiles, which
    // is what this test has always been about; what changed is that both routes
    // now identify the caller, require the `clientUid` ATAK always sends, and
    // that enrolment always has one generated file to send.
    let harness = harness().await;
    let (_, session) = harness.server.signed_in("ada", false).await;
    let token = rustak_server::testing::context::bearer(&session);
    let app = test::init_service(App::new().configure(harness.app())).await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/device/profile/connection?clientUid=ANDROID-1&syncSecago=-1")
            .insert_header((AUTHORIZATION, token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(
        response.status().as_u16(),
        204,
        "ATAK reads 'nothing for you' and carries on to the stream",
    );

    // The enrolment profile is the exception: it always carries the generated
    // `rustak-enrollment.pref`, because that is the only thing that turns on
    // the client preference every later connection profile depends on.
    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/tls/profile/enrollment?clientUid=ANDROID-1")
            .insert_header((AUTHORIZATION, token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);

    // And a request with no credential is refused rather than answered with an
    // empty profile, because these carry an account's own settings.
    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/tls/profile/enrollment?clientUid=ANDROID-1")
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 401);

    harness.stop().await;
}

#[actix_web::test]
async fn re_enrolling_is_a_fresh_certificate_rather_than_a_conflict() {
    // CloudTAK re-enrols automatically within seven days of expiry, reusing the
    // password it already has cached.
    let harness = harness().await;
    let password = credential(&harness.server, "ada", CredentialKind::ClientPassword).await;
    let app = test::init_service(App::new().configure(harness.app())).await;

    let mut fingerprints = Vec::new();

    for _ in 0..2 {
        let (csr, _) = signing_request("ada");
        let body: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/Marti/api/tls/signClient/v2?clientUid=ada%20(ETL)&version=3")
                .insert_header((AUTHORIZATION, basic("ada", &password)))
                .set_payload(csr)
                .to_request(),
        )
        .await;

        let der = pem::parse_pem_chain(&armour(body["signedCert"].as_str().unwrap())).unwrap();
        fingerprints.push(pem::sha256_fingerprint(&der[0]));
    }

    assert_ne!(
        fingerprints[0], fingerprints[1],
        "each enrolment is a fresh certificate, not the previous one handed back",
    );

    harness.stop().await;
}
