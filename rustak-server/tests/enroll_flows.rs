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

/// How long to wait for the Marti listener to start answering.
const MARTI_READY_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a probe holds a connection open before calling the listener ready.
const MARTI_PROBE: Duration = Duration::from_millis(250);

/// Waits until the Marti listener stops closing connections as they arrive.
///
/// `build_marti_on` is handed a socket that is already **bound**, and only the
/// spawned `Server` future starts the workers that serve it — so a connection
/// arriving in between is completed by the kernel from the backlog and then
/// dropped by actix, which has no worker to hand it to. `reqwest` reports that
/// as `connection closed` part-way through the handshake. On a quiet machine
/// the gap is too small to hit and this suite passes; under a loaded
/// `cargo test --workspace` the spawned task is starved and M5-02 measured the
/// two real-mTLS tests failing four runs in five. CI has not shown it, which is
/// consistent — the runner is slow but not contended.
///
/// The probe sits deliberately *below* TLS, because that is the layer the
/// failure is at. A listener with a worker holds the connection open waiting
/// for a ClientHello, so a read that **times out** is the ready signal; an
/// immediate EOF or reset is "not yet". Nothing is written, no certificate is
/// issued and no row is created, so this is invisible to every assertion.
///
/// This also closes a quieter hazard. Two tests here assert that a handshake is
/// *refused* — the revoked certificate and the caller with none — and a
/// listener that drops connections because it has no worker yet would satisfy
/// both for entirely the wrong reason. Waiting until it genuinely serves means
/// those two can only pass on a real refusal.
async fn await_marti(port: u16) {
    use tokio::io::AsyncReadExt as _;

    let deadline = std::time::Instant::now() + MARTI_READY_TIMEOUT;

    loop {
        if let Ok(mut stream) = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await {
            let mut byte = [0u8; 1];

            match tokio::time::timeout(MARTI_PROBE, stream.read(&mut byte)).await {
                // Held open, waiting for us to speak: a worker has it.
                Err(_elapsed) => return,
                // It spoke first, which it can only do if it is serving.
                Ok(Ok(1..)) => return,
                // EOF or a reset: bound, but nothing is serving it yet.
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

/// Starts a server with a real authority and a Marti listener on a real socket.
///
/// The socket is bound **here**, on `:0`, and handed to the server rather than
/// described to it. Binding `:0` is the only way to be given a free port, and
/// serving on the socket that claimed it is the only way to serve on that port
/// with no gap in which anything else can take it. This used to bind `:0`,
/// read the port, release it and have `build_marti` bind the number again,
/// retrying if the bind failed — which covered a bind that *failed*, and
/// nothing else that a loaded `cargo test --workspace` could fit into the gap.
/// (M6-01 traced one flake to the gap; M5-02 saw the two real-mTLS tests fail
/// four runs in five under load.)
///
/// # Panics
///
/// If the socket cannot be bound or the server cannot be built on it. Neither
/// is a race: the first is a machine with no loopback ports to give, and the
/// second is the authority or actix refusing a socket that is already ours.
async fn harness() -> Harness {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free port on loopback");
    let port = listener
        .local_addr()
        .expect("the port we just bound")
        .port();

    let server = TestServer::start_with(move |config| {
        // Elliptic curve rather than the RSA default: this suite creates an
        // authority, a server certificate and a client key per test, and RSA
        // generation would dominate the run time without testing anything the
        // `pki::` unit tests do not already cover.
        config.pki.key_type = KeyType::EcdsaP256;
        config.pki.server_ips = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
        // Not what binds the listener — the socket above is — but the
        // configuration should still say where it is.
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

#[actix_web::test]
async fn every_advertised_name_entry_appears_in_the_issued_subject() {
    // R-02 M6. `GET /Marti/api/tls/config` has to advertise at least two
    // `<nameEntry>` elements with non-empty values — CloudTAK's `xml-js`
    // collapses a one-element array and commoncommo refuses a zero-length
    // subject component — and that padding used to be done in `marti::tls` over
    // the advertised list alone. With stock configuration the server therefore
    // told a device to build `CN + O + OU` and then issued `CN + O`, so
    // `warn_on_subject_mismatch` fired on every ATAK enrolment and
    // `compat/enrollment.md` §1/§4's "the advertised and issued subjects agree"
    // was not true of the default path.
    //
    // Run against the **default** configuration on purpose: the default is what
    // was wrong, and a fixture with explicit `name_entries` hides it.
    let harness = harness().await;
    let token = credential(&harness.server, "ada", CredentialKind::EnrollmentToken).await;
    let app = test::init_service(App::new().configure(harness.app())).await;

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

    let document = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();
    let advertised: Vec<(String, String)> = document
        .match_indices("<nameEntry ")
        .map(|(at, _)| {
            let element = &document[at..document[at..].find("/>").unwrap() + at];
            let field = |key: &str| {
                let start = element.find(&format!("{key}=\"")).unwrap() + key.len() + 2;
                let rest = &element[start..];

                rest[..rest.find('"').unwrap()].to_string()
            };

            (field("name"), field("value"))
        })
        .collect();

    assert!(
        advertised.len() >= 2,
        "xml-js collapses a one-element array: {document}",
    );
    assert!(
        advertised.iter().all(|(_, value)| !value.is_empty()),
        "OpenSSL refuses a zero-length subject component: {document}",
    );

    let (csr, _) = signing_request("ada");
    let issued: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::post()
            .uri("/Marti/api/tls/signClient/v2?clientUid=ANDROID-1&version=3")
            .insert_header((AUTHORIZATION, basic("ada", &token)))
            .insert_header((ACCEPT, "application/json"))
            .set_payload(csr)
            .to_request(),
    )
    .await;

    // The base64 is bare but line-wrapped, which the strict engine refuses.
    let bare: String = issued["signedCert"]
        .as_str()
        .expect("a signed certificate")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(bare)
        .expect("bare base64");
    let (_, certificate) = x509_parser::parse_x509_certificate(&der).expect("a certificate");
    let subject = certificate.subject().to_string();

    for (name, value) in &advertised {
        assert!(
            subject.contains(&format!("{name}={value}")),
            "the enrolment document advertised {name}={value} and the issued subject \
             is '{subject}' — a device builds its signing request from exactly these \
             entries, so one the issuer drops is a subject that disagrees with itself",
        );
    }

    harness.stop().await;
}

#[actix_web::test]
async fn one_enrolment_token_produces_exactly_one_certificate() {
    // R-01 M3. Usability was checked in one read and the spend was a later,
    // unconditional write, so two concurrent posts of the same token both
    // passed and both received a certificate — the window being one argon2
    // verification plus a signature. The consumption is now the gate.
    let harness = harness().await;
    let token = credential(&harness.server, "ada", CredentialKind::EnrollmentToken).await;
    let app = test::init_service(App::new().configure(harness.app())).await;

    let post = |body: String| {
        test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/Marti/api/tls/signClient/v2?clientUid=ANDROID-RACE&version=5.1.0")
                .insert_header((AUTHORIZATION, basic("ada", &token)))
                .insert_header((ACCEPT, "application/json"))
                .insert_header((CONTENT_TYPE, "application/octet-stream"))
                .set_payload(body)
                .to_request(),
        )
    };

    let (first, second) = tokio::join!(
        post(signing_request("ada").0),
        post(signing_request("ada").0)
    );
    let statuses = [first.status().as_u16(), second.status().as_u16()];

    assert_eq!(
        statuses.iter().filter(|status| **status == 200).count(),
        1,
        "exactly one of two racing enrolments may win: {statuses:?}",
    );

    let user = harness
        .server
        .db()
        .users()
        .get_by_username(&Username::parse("ada").unwrap())
        .await
        .unwrap()
        .unwrap();
    let held = harness
        .server
        .db()
        .credentials()
        .list_for_user(user.id, true)
        .await
        .unwrap();

    assert_eq!(held.len(), 1);
    assert_eq!(held[0].uses, 1, "and the token is spent exactly once");
    assert!(held[0].revoked_at.is_some(), "a one-time token stays spent");

    harness.stop().await;
}

#[actix_web::test]
async fn a_spent_enrolment_token_cannot_be_presented_again() {
    let harness = harness().await;
    let token = credential(&harness.server, "ada", CredentialKind::EnrollmentToken).await;
    let app = test::init_service(App::new().configure(harness.app())).await;

    for expected in [200, 401] {
        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/Marti/api/tls/signClient/v2?clientUid=ANDROID-ONCE&version=5.1.0")
                .insert_header((AUTHORIZATION, basic("ada", &token)))
                .insert_header((ACCEPT, "application/json"))
                .insert_header((CONTENT_TYPE, "application/octet-stream"))
                .set_payload(signing_request("ada").0)
                .to_request(),
        )
        .await;

        assert_eq!(response.status().as_u16(), expected);
    }

    harness.stop().await;
}

#[actix_web::test]
async fn a_device_uid_cannot_be_taken_from_the_account_that_enrolled_it() {
    // R-01 M4. A device uid is in every CoT event that device sends and in
    // `GET /Marti/api/clientEndPoints`, so it is not a secret — and taking the
    // row over takes the `device_group_state` the owner's channels are
    // intersected against with it.
    let harness = harness().await;
    let ada = credential(&harness.server, "ada", CredentialKind::ClientPassword).await;
    let grace = credential(&harness.server, "grace", CredentialKind::ClientPassword).await;
    let app = test::init_service(App::new().configure(harness.app())).await;

    let enrol = |username: &'static str, password: String| {
        test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/Marti/api/tls/signClient/v2?clientUid=ANDROID-ADA&version=5.1.0")
                .insert_header((AUTHORIZATION, basic(username, &password)))
                .insert_header((ACCEPT, "application/json"))
                .insert_header((CONTENT_TYPE, "application/octet-stream"))
                .set_payload(signing_request(username).0)
                .to_request(),
        )
    };

    assert_eq!(enrol("ada", ada).await.status().as_u16(), 200);

    let stolen = enrol("grace", grace).await;

    assert_eq!(
        stolen.status().as_u16(),
        403,
        "another account's device uid is not one to enrol with",
    );

    let device = harness
        .server
        .db()
        .devices()
        .get_by_uid(&DeviceUid::parse("ANDROID-ADA").unwrap())
        .await
        .unwrap()
        .expect("the device row");
    let ada_row = harness
        .server
        .db()
        .users()
        .get_by_username(&Username::parse("ada").unwrap())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(device.user_id, ada_row.id);

    harness.stop().await;
}
