//! `POST /api/v1/users/{username}/cloudtak-onboarding` and its one-shot
//! download, end to end against the real application.
//!
//! This is the one feature in rustak where the server generates a client's
//! private key, so the suite is written as a list of the properties that make
//! that defensible — and every one of them has a test here that fails if it is
//! relaxed:
//!
//! * **Only an administrator.** Both routes, separately. A hand-over that an
//!   ordinary account could ask for would be a way to mint an administrator
//!   certificate for anybody.
//! * **Only once.** The second fetch is a `410`, including when the two arrive
//!   together. A keystore that can be fetched twice is one a proxy log or a
//!   browser history can be replayed from.
//! * **Not for long.** The bundle expires whether or not it was collected, a
//!   later hand-over sweeps what is left, and a scheduled sweep removes it on
//!   an installation where no later hand-over ever comes.
//! * **Nothing stored in the clear, and nothing stored that opens it.** The
//!   key/value row is a sealed envelope around a PKCS#12 that is itself
//!   encrypted with a passphrase generated for this hand-over — and that
//!   passphrase is returned once and written nowhere, so a database dump plus
//!   the installation's sealing key yields an encrypted file rather than a
//!   usable credential. The certificate row holds no key at all.
//! * **Written down.** `cloudtak.onboarding.created` and
//!   `cloudtak.onboarding.downloaded` name the account, the administrator and
//!   the certificate; neither carries a secret.
//! * **Revocable.** The certificate is an ordinary client certificate: it is
//!   listed by `/api/v1/certificates` and ended by `POST .../revoke`.
//! * **Readable by CloudTAK.** The bundle uses the legacy algorithms
//!   `@tak-ps/node-p12` reads, its leaf comes first, and its key matches its
//!   certificate. `interop/node-tak` proves the same thing through CloudTAK's
//!   own parser; this proves it without a network.
//!
//! Run with
//! `cargo test -p rustak-server --features testing --test cloudtak_onboarding`.

#![cfg(feature = "testing")]

use std::sync::Arc;

use actix_web::http::StatusCode;
use actix_web::{App, test};
use base64::Engine as _;
use chrono::{Duration, Utc};
use p12_keystore::{KeyStore, Pkcs12ImportPolicy};
use rustak_api::cloudtak::ISSUED_VIA;
use rustak_api::{
    Certificate, CloudTakOnboarding, CloudTakOnboardingRequest, CloudTakPorts, CredentialId,
    CredentialKind, OnboardingCredential,
};
use rustak_core::config::ListenAddr;
use rustak_server::crypto::{Sealed, SecretContext};
use rustak_server::db::repos::UserRow;
use rustak_server::db::{AuditQuery, AuditStore as _, KeyValueStore as _};
use rustak_server::identity::cloudtak;
use rustak_server::jobs::{CloudTakSweepJob, JobRunnable};
use rustak_server::pki::{KeyType, Pki};
use rustak_server::prelude::*;
use rustak_server::testing::TestServer;
use rustak_server::testing::context::bearer;
use x509_parser::prelude::FromDer as _;

macro_rules! app {
    ($server:expr) => {
        test::init_service(App::new().configure($server.app())).await
    };
}

/// A server with an authority, the three listener ports written down, and an
/// administrator signed in.
async fn harness() -> (TestServer, String) {
    let server = TestServer::start_with(|config| {
        // RSA is what the hand-over generates whatever this says — CloudTAK's
        // parser reads nothing else — but the authority itself may be a curve,
        // and keeping it one saves seconds per test.
        config.pki.key_type = KeyType::EcdsaP256;
        config.stream.tls.listen = ListenAddr::new("", 8089);
        config.web.marti.listen = ListenAddr::new("", 8443);
        config.web.public.listen = vec![ListenAddr::new("", 8446)];
        config.marti.public_host = Some("tak.example.com".to_string());
    })
    .await;

    let config = server.config();
    let pki = Pki::load(
        server.db(),
        server.secrets(),
        &config.pki,
        &config.server.data_dir,
        &["tak.example.com".to_string()],
        &[],
    )
    .await
    .expect("an authority for the test server");

    server
        .context
        .install_pki(Arc::clone(&pki))
        .expect("the authority is installed once");

    let (_, session) = server.signed_in("ada", true).await;

    (server, bearer(&session))
}

/// Asks for a hand-over and reads the response.
macro_rules! onboard {
    ($app:expr, $token:expr, $username:expr, $body:expr) => {{
        let response = test::call_service(
            &$app,
            test::TestRequest::post()
                .uri(&format!("/api/v1/users/{}/cloudtak-onboarding", $username))
                .insert_header(("authorization", $token.to_string()))
                .set_json($body)
                .to_request(),
        )
        .await;

        (response.status(), response)
    }};
}

/// A successful hand-over.
///
/// A macro rather than a function, like `app!` above: the application
/// `test::init_service` builds has an opaque type, and naming it in a signature
/// costs more than it saves.
macro_rules! prepared {
    ($app:expr, $token:expr, $username:expr) => {{
        let (status, response) = onboard!(
            $app,
            $token,
            $username,
            &CloudTakOnboardingRequest::default()
        );

        assert_eq!(status, StatusCode::OK, "a prepared hand-over is a 200");

        let onboarding: CloudTakOnboarding = test::read_body_json(response).await;
        onboarding
    }};
}

/// Fetches the prepared keystore, whatever the outcome.
macro_rules! fetch {
    ($app:expr, $token:expr, $url:expr) => {
        test::call_service(
            &$app,
            test::TestRequest::get()
                .uri($url)
                .insert_header(("authorization", $token.to_string()))
                .to_request(),
        )
        .await
    };
}

/// An ordinary account, already in the default channel.
async fn account(server: &TestServer, username: &str) -> UserRow {
    server.user(username, false).await
}

#[actix_web::test]
async fn one_action_produces_everything_cloudtaks_setup_page_asks_for() {
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let onboarding = prepared!(app, &token, "cloudtak");

    assert_eq!(onboarding.username.as_str(), "cloudtak");
    assert_eq!(onboarding.urls.stream, "ssl://tak.example.com:8089");
    assert_eq!(onboarding.urls.api, "https://tak.example.com:8443");
    assert_eq!(onboarding.urls.webtak, "https://tak.example.com:8446");
    assert!(
        onboarding.password.is_some(),
        "a minted password is shown once"
    );
    assert!(!onboarding.p12_password.is_empty());
    assert!(
        onboarding
            .p12_download_url
            .starts_with("/api/v1/cloudtak-onboarding/"),
        "{}",
        onboarding.p12_download_url,
    );
    assert!(onboarding.expires_at > Utc::now());
    assert!(
        onboarding.expires_at < Utc::now() + Duration::minutes(11),
        "the window is minutes, not hours",
    );
}

#[actix_web::test]
async fn the_keystore_is_one_cloudtaks_own_parser_can_read() {
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let onboarding = prepared!(app, &token, "cloudtak");
    let response = fetch!(app, &token, &onboarding.p12_download_url);

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/x-pkcs12"),
    );
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store"),
        "a proxy must not keep the copy this server has just deleted",
    );

    let bundle = test::read_body(response).await;

    // The algorithms, as OIDs in the file itself. `@tak-ps/node-p12` goes
    // through node-forge, which reads PBES1 with 3DES and a SHA-1 MAC and
    // nothing newer — a bundle written with PBES2 is one CloudTAK refuses.
    //  pbeWithSHAAnd3-KeyTripleDES-CBC  1.2.840.113549.1.12.1.3
    const PBE_3DES: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x0c, 0x01, 0x03];
    //  PBES2                            1.2.840.113549.1.5.13
    const PBES2: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x05, 0x0d];

    assert!(contains(&bundle, PBE_3DES), "the legacy key encryption");
    assert!(!contains(&bundle, PBES2), "never the modern one");

    let store = KeyStore::from_pkcs12(
        &bundle,
        &onboarding.p12_password,
        Pkcs12ImportPolicy::Relaxed,
    )
    .expect("the bundle must open with the passphrase it was handed with");

    let (_, chain) = store
        .private_key_chain()
        .expect("a keystore carries exactly one keychain");

    // node-p12 takes the *first* certificate bag and reads its common name, so
    // a chain that led with the authority would make CloudTAK think the
    // connection belongs to the CA.
    assert!(
        chain.certs()[0].subject().contains("cloudtak"),
        "the leaf comes first: {}",
        chain.certs()[0].subject(),
    );
    assert!(chain.certs().len() > 1, "the authority is included");

    // The key and the certificate have to go together, or the mTLS handshake
    // CloudTAK does next fails with nothing to look at.
    let key = rustak_server::pki::key_pair_from_pkcs8(chain.key().as_der(), KeyType::Rsa2048)
        .expect("the stored key is the RSA one we generated");
    let (_, certificate) =
        x509_parser::prelude::X509Certificate::from_der(chain.certs()[0].as_der())
            .expect("the leaf parses");

    assert_eq!(
        spki_of(&key),
        certificate.public_key().raw,
        "the key in the bundle is the key the certificate names",
    );
}

#[actix_web::test]
async fn the_keystore_is_handed_over_exactly_once() {
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let onboarding = prepared!(app, &token, "cloudtak");

    assert_eq!(
        fetch!(app, &token, &onboarding.p12_download_url).status(),
        StatusCode::OK,
    );

    let second = fetch!(app, &token, &onboarding.p12_download_url);

    assert_eq!(
        second.status(),
        StatusCode::GONE,
        "a second fetch is refused, not served",
    );

    let body = test::read_body(second).await;
    let refusal = String::from_utf8_lossy(&body);

    assert!(refusal.contains("already been downloaded"), "{refusal}");

    // And nothing is left behind for a sweep or a backup to find.
    let remaining: Vec<(String, serde_json::Value)> =
        server.db().list(cloudtak::BUNDLE_PARTITION).await.unwrap();

    assert!(
        remaining.is_empty(),
        "the bundle is deleted by the download"
    );
}

#[actix_web::test]
async fn an_unknown_download_is_refused_the_same_way_a_spent_one_is() {
    // Otherwise the endpoint is an oracle for which hand-overs are outstanding.
    let (server, token) = harness().await;
    let app = app!(server);

    for id in ["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "..%2f..%2fpki", "x"] {
        let response = fetch!(
            app,
            &token,
            &format!("/api/v1/cloudtak-onboarding/{id}.p12")
        );

        assert_eq!(response.status(), StatusCode::GONE, "{id}");
    }
}

#[actix_web::test]
async fn a_bundle_that_has_expired_is_swept_and_refused() {
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let onboarding = prepared!(app, &token, "cloudtak");

    // Well past the ten-minute window, which is what a sweep run tomorrow sees.
    let swept = cloudtak::sweep(server.db(), Utc::now() + Duration::hours(1))
        .await
        .unwrap();

    assert_eq!(swept, 1);
    assert_eq!(
        fetch!(app, &token, &onboarding.p12_download_url).status(),
        StatusCode::GONE,
    );
}

#[actix_web::test]
async fn preparing_a_hand_over_sweeps_the_one_before_it() {
    let (server, token) = harness().await;
    let app = app!(server);
    let user = account(&server, "cloudtak").await;

    // A bundle from a hand-over nobody collected, backdated past its window.
    //
    // Stashed through the same function a hand-over stashes through rather than
    // through a second `POST`, because the only thing a second `POST` would add
    // is the RSA-2048 key it generates for the client — and that one line is
    // very nearly the whole cost of this suite. Generating one is a random
    // prime search: measured here between 0.18 s and 1.38 s with the
    // optimisation the root `Cargo.toml` already gives the `rsa` crate, and
    // roughly twelve times that on the instrumented two-vCPU runner. Two draws
    // from that distribution in one test is why libtest reported *this* test as
    // "running for over 60 seconds" on CI run 35449569084, while the other
    // eighteen, which draw once each, did not. There is no sleep and no
    // real-time wait anywhere in this flow; the window is closed by rewriting
    // the stored expiry, below.
    let (stale, _) = cloudtak::stash(
        &server.context,
        &user.username,
        CertificateId::new(1),
        b"a keystore nobody ever collected".to_vec(),
    )
    .await
    .expect("a bundle waiting from an earlier hand-over");

    backdate(&server, &stale).await;

    let fresh = prepared!(app, &token, "cloudtak");

    // The row is *gone*, which the refusal below does not prove on its own:
    // `take` deletes before it checks the window, so an expired bundle the
    // sweep had missed would answer `410` just the same. This is the assertion
    // that says the preparation swept it.
    let stored: Vec<(String, serde_json::Value)> =
        server.db().list(cloudtak::BUNDLE_PARTITION).await.unwrap();

    assert_eq!(
        stored.len(),
        1,
        "the stale bundle went when the next one was prepared",
    );
    assert_eq!(
        fetch!(
            app,
            &token,
            &format!("/api/v1/cloudtak-onboarding/{stale}.p12")
        )
        .status(),
        StatusCode::GONE,
    );
    assert_eq!(
        fetch!(app, &token, &fresh.p12_download_url).status(),
        StatusCode::OK,
    );
}

#[actix_web::test]
async fn the_scheduled_sweep_removes_a_bundle_nobody_ever_came_back_for() {
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let onboarding = prepared!(app, &token, "cloudtak");
    let id = onboarding
        .p12_download_url
        .rsplit('/')
        .next()
        .and_then(|segment| segment.strip_suffix(".p12"))
        .expect("the download URL ends in the identifier")
        .to_string();

    backdate(&server, &id).await;

    // The point of the job: an installation that onboards CloudTAK once makes
    // no second request, so without a schedule this row would still be here.
    // Run through `JobRunnable` rather than `Job`, because that is the path the
    // host dispatches on and it proves the payload the queue carries is one
    // this handler can read.
    JobRunnable::handle(
        &CloudTakSweepJob,
        JobContext::new(server.context.clone(), Utc::now(), None, None),
        &serde_json::json!({}),
    )
    .await
    .expect("the sweep runs");

    let stored: Vec<(String, serde_json::Value)> =
        server.db().list(cloudtak::BUNDLE_PARTITION).await.unwrap();

    assert!(
        stored.is_empty(),
        "the expired bundle should be gone from the store, not merely refused",
    );
    assert_eq!(
        fetch!(app, &token, &onboarding.p12_download_url).status(),
        StatusCode::GONE,
    );
}

#[actix_web::test]
async fn nothing_readable_is_stored_between_preparing_and_downloading() {
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let onboarding = prepared!(app, &token, "cloudtak");
    let stored: Vec<(String, serde_json::Value)> =
        server.db().list(cloudtak::BUNDLE_PARTITION).await.unwrap();

    assert_eq!(stored.len(), 1);

    let rendered = serde_json::to_string(&stored[0].1).unwrap();

    assert!(
        rendered.contains("\"sealed\""),
        "the bundle is an envelope: {rendered}",
    );
    assert!(
        !rendered.contains(&onboarding.p12_password),
        "the passphrase is never written down",
    );
    assert!(
        !rendered.contains("BEGIN"),
        "nothing PEM-shaped is stored in the clear",
    );

    // And the certificate row keeps no key of its own: the hand-over is the
    // only copy, and it is about to be deleted.
    let row = server
        .db()
        .certificates()
        .get(onboarding.certificate_id)
        .await
        .unwrap()
        .expect("the certificate was recorded");

    assert!(row.key_sealed.is_none(), "the row holds no key");
}

#[actix_web::test]
async fn the_sealing_key_alone_does_not_open_the_bundle() {
    // The property the two layers exist for. Somebody who takes the database
    // *and* the installation's sealing key gets as far as the bytes of the
    // PKCS#12 — and no further, because the passphrase that opens it was
    // returned once and stored nowhere.
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let onboarding = prepared!(app, &token, "cloudtak");

    let stored: Vec<(String, serde_json::Value)> =
        server.db().list(cloudtak::BUNDLE_PARTITION).await.unwrap();
    let sealed: Sealed = serde_json::from_value(stored[0].1["sealed"].clone())
        .expect("the stash holds a sealed envelope");

    let bundle = server
        .secrets()
        .open(
            &sealed,
            SecretContext::ServiceCertKey {
                certificate: onboarding.certificate_id,
            },
        )
        .expect("the sealing key opens the envelope, which is all it does");

    for wrong in ["", "atakatak", "not-the-passphrase"] {
        assert!(
            KeyStore::from_pkcs12(&bundle, wrong, Pkcs12ImportPolicy::Relaxed).is_err(),
            "the unsealed bundle opened with '{wrong}'",
        );
    }

    assert!(
        KeyStore::from_pkcs12(
            &bundle,
            &onboarding.p12_password,
            Pkcs12ImportPolicy::Relaxed,
        )
        .is_ok(),
        "and the passphrase that was shown once does open it",
    );
}

#[actix_web::test]
async fn the_passphrase_is_in_the_response_and_in_no_row_anywhere() {
    // Named after the rule: it is not beside the bundle, not on the credential
    // or certificate rows, and not in the audit log. Anything that wrote it
    // down would make the sealed stash the only protection rather than the
    // second one.
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let onboarding = prepared!(app, &token, "cloudtak");
    let passphrase = onboarding.p12_password.clone();

    assert!(!passphrase.is_empty());

    let stash: Vec<(String, serde_json::Value)> =
        server.db().list(cloudtak::BUNDLE_PARTITION).await.unwrap();
    let credential = server
        .db()
        .credentials()
        .get(onboarding.credential_id)
        .await
        .unwrap()
        .unwrap();
    let certificate = server
        .db()
        .certificates()
        .get(onboarding.certificate_id)
        .await
        .unwrap()
        .unwrap();
    let audit = server
        .db()
        .audit(AuditQuery::about("cloudtak", 50))
        .await
        .unwrap();

    for (what, rendered) in [
        (
            "the key/value stash",
            serde_json::to_string(&stash).unwrap(),
        ),
        ("the credential row", format!("{credential:?}")),
        ("the certificate row", format!("{certificate:?}")),
        ("the audit log", serde_json::to_string(&audit).unwrap()),
    ] {
        assert!(
            !rendered.contains(&passphrase),
            "the passphrase reached {what}",
        );
    }
}

#[actix_web::test]
async fn only_an_administrator_may_prepare_or_collect_one() {
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let (_, ordinary) = server.signed_in("mallory", false).await;
    let ordinary = bearer(&ordinary);

    let (status, _) = onboard!(
        app,
        &ordinary,
        "cloudtak",
        &CloudTakOnboardingRequest::default()
    );

    assert_eq!(status, StatusCode::FORBIDDEN, "not for an ordinary account");

    // The download is administrative in its own right: a link handed to
    // somebody signed in is not a bearer token for a private key.
    let onboarding = prepared!(app, &token, "cloudtak");

    assert_eq!(
        fetch!(app, &ordinary, &onboarding.p12_download_url).status(),
        StatusCode::FORBIDDEN,
    );
    assert_eq!(
        fetch!(app, &token, &onboarding.p12_download_url).status(),
        StatusCode::OK,
        "and the administrator still gets it afterwards",
    );
}

#[actix_web::test]
async fn an_existing_client_password_may_be_reused_without_re_emitting_it() {
    let (server, token) = harness().await;
    let app = app!(server);
    let user = account(&server, "cloudtak").await;

    let minted = rustak_server::identity::credentials::mint(
        server.db(),
        &server.config().auth,
        &user,
        rustak_server::identity::MintRequest::new(
            CredentialKind::ClientPassword,
            "Already issued",
            &user.username,
        ),
    )
    .await
    .unwrap();

    let (status, response) = onboard!(
        app,
        &token,
        "cloudtak",
        &CloudTakOnboardingRequest {
            credential: OnboardingCredential::Existing(minted.credential.id),
            ..CloudTakOnboardingRequest::default()
        }
    );

    assert_eq!(status, StatusCode::OK);

    let onboarding: CloudTakOnboarding = test::read_body_json(response).await;

    assert_eq!(onboarding.credential_id, minted.credential.id);
    assert!(
        onboarding.password.is_none(),
        "a stored password is a hash; nothing can re-emit it",
    );
}

#[actix_web::test]
async fn a_credential_that_is_not_this_accounts_live_client_password_is_refused() {
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;
    let other = account(&server, "someone-else").await;

    // Somebody else's, an enrolment token, and one that does not exist.
    let theirs = rustak_server::identity::credentials::mint(
        server.db(),
        &server.config().auth,
        &other,
        rustak_server::identity::MintRequest::new(
            CredentialKind::ClientPassword,
            "Theirs",
            &other.username,
        ),
    )
    .await
    .unwrap();

    let token_credential = rustak_server::identity::credentials::mint(
        server.db(),
        &server.config().auth,
        &server
            .db()
            .users()
            .get_by_username(&Username::parse("cloudtak").unwrap())
            .await
            .unwrap()
            .unwrap(),
        rustak_server::identity::MintRequest::new(
            CredentialKind::EnrollmentToken,
            "Wrong kind",
            &Username::parse("ada").unwrap(),
        ),
    )
    .await
    .unwrap();

    for id in [
        theirs.credential.id,
        token_credential.credential.id,
        CredentialId::new(99_999),
    ] {
        let (status, _) = onboard!(
            app,
            &token,
            "cloudtak",
            &CloudTakOnboardingRequest {
                credential: OnboardingCredential::Existing(id),
                ..CloudTakOnboardingRequest::default()
            }
        );

        assert_eq!(status, StatusCode::BAD_REQUEST, "credential {id}");
    }
}

#[actix_web::test]
async fn the_deployments_own_host_and_ports_may_be_echoed_back() {
    // The deployment this was asked for: rustak in a container published on
    // 28089/28443/28446.
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let (status, response) = onboard!(
        app,
        &token,
        "cloudtak",
        &CloudTakOnboardingRequest {
            host: Some("tak.internal".to_string()),
            ports: Some(CloudTakPorts {
                stream: Some(28089),
                marti: Some(28443),
                public: Some(28446),
            }),
            ..CloudTakOnboardingRequest::default()
        }
    );

    assert_eq!(status, StatusCode::OK);

    let onboarding: CloudTakOnboarding = test::read_body_json(response).await;

    assert_eq!(onboarding.urls.stream, "ssl://tak.internal:28089");
    assert_eq!(onboarding.urls.api, "https://tak.internal:28443");
    assert_eq!(onboarding.urls.webtak, "https://tak.internal:28446");
}

#[actix_web::test]
async fn a_host_that_cannot_go_in_a_url_is_refused_before_anything_is_issued() {
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let (status, _) = onboard!(
        app,
        &token,
        "cloudtak",
        &CloudTakOnboardingRequest {
            host: Some("https://tak.example.com/marti".to_string()),
            ..CloudTakOnboardingRequest::default()
        }
    );

    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Nothing was minted and nothing was signed: a mistyped name must not leave
    // a live credential and a certificate behind.
    let certificates: Vec<Certificate> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/certificates?username=cloudtak")
            .insert_header(("authorization", token.as_str()))
            .to_request(),
    )
    .await;

    assert!(certificates.is_empty(), "{certificates:?}");
}

#[actix_web::test]
async fn an_unknown_or_disabled_account_is_refused() {
    let (server, token) = harness().await;
    let app = app!(server);
    let user = account(&server, "cloudtak").await;

    let (status, _) = onboard!(app, &token, "nobody", &CloudTakOnboardingRequest::default());

    assert_eq!(status, StatusCode::BAD_REQUEST, "no such account");

    server
        .db()
        .users()
        .set_disabled(user.id, true)
        .await
        .unwrap();

    let (status, _) = onboard!(
        app,
        &token,
        "cloudtak",
        &CloudTakOnboardingRequest::default()
    );

    assert_eq!(status, StatusCode::BAD_REQUEST, "a disabled account");
}

#[actix_web::test]
async fn the_certificate_is_an_ordinary_one_that_can_be_listed_and_revoked() {
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let onboarding = prepared!(app, &token, "cloudtak");

    let listed: Vec<Certificate> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/certificates?username=cloudtak")
            .insert_header(("authorization", token.as_str()))
            .to_request(),
    )
    .await;

    let certificate = listed
        .iter()
        .find(|entry| entry.id == onboarding.certificate_id)
        .expect("the hand-over's certificate is listed like any other");

    assert_eq!(certificate.subject_cn, "cloudtak");
    assert_eq!(
        certificate.username.as_ref().map(Username::as_str),
        Some("cloudtak"),
    );

    let row = server
        .db()
        .certificates()
        .get(onboarding.certificate_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        row.issued_via.as_deref(),
        Some(ISSUED_VIA),
        "the exception is countable in the register",
    );
    assert_eq!(row.credential_id, Some(onboarding.credential_id));

    let revoked = test::call_service(
        &app,
        test::TestRequest::post()
            .uri(&format!(
                "/api/v1/certificates/{}/revoke",
                onboarding.certificate_id
            ))
            .insert_header(("authorization", token.as_str()))
            .set_json(serde_json::json!({ "reason": "superseded" }))
            .to_request(),
    )
    .await;

    assert_eq!(revoked.status(), StatusCode::OK);

    let after: Certificate = test::read_body_json(revoked).await;

    assert!(after.is_revoked());
}

#[actix_web::test]
async fn both_halves_are_written_down_and_neither_carries_a_secret() {
    let (server, token) = harness().await;
    let app = app!(server);
    account(&server, "cloudtak").await;

    let onboarding = prepared!(app, &token, "cloudtak");

    assert_eq!(
        fetch!(app, &token, &onboarding.p12_download_url).status(),
        StatusCode::OK,
    );

    let records = server
        .db()
        .audit(AuditQuery::about("cloudtak", 50))
        .await
        .unwrap();

    let created = records
        .iter()
        .find(|record| record.action == "cloudtak.onboarding.created")
        .expect("preparing a hand-over is recorded");
    let downloaded = records
        .iter()
        .find(|record| record.action == "cloudtak.onboarding.downloaded")
        .expect("collecting one is recorded");

    for record in [created, downloaded] {
        assert_eq!(record.actor.as_deref(), Some("ada"), "the administrator");
        assert_eq!(record.subject.as_deref(), Some("cloudtak"));

        let rendered = serde_json::to_string(record).unwrap();

        assert!(
            !rendered.contains(&onboarding.p12_password),
            "the passphrase must never reach the audit log: {rendered}",
        );

        if let Some(password) = &onboarding.password {
            assert!(
                !rendered.contains(password),
                "nor the account's password: {rendered}",
            );
        }
    }

    assert_eq!(
        created
            .detail
            .as_ref()
            .and_then(|detail| detail.get("certificate_id"))
            .and_then(serde_json::Value::as_i64),
        Some(onboarding.certificate_id.get()),
    );
}

/// Rewrites a stashed bundle so that its window has already closed.
///
/// The window is ten minutes of wall clock, and no test waits it out: what
/// makes a bundle stale here is the stored timestamp, so every expiry case runs
/// in the time one database write takes and none of them depends on a clock.
async fn backdate(server: &TestServer, id: &str) {
    let mut stored: serde_json::Value = server
        .db()
        .get(cloudtak::BUNDLE_PARTITION, id.to_string())
        .await
        .unwrap()
        .expect("the bundle is waiting");

    stored["expires_at"] = serde_json::json!(Utc::now() - Duration::minutes(1));

    server
        .db()
        .set(cloudtak::BUNDLE_PARTITION, id.to_string(), stored)
        .await
        .unwrap();
}

/// The subject public key info of a key pair, as a certificate carries it.
fn spki_of(key: &rcgen::KeyPair) -> Vec<u8> {
    let pem = key.public_key_pem();
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();

    base64::engine::general_purpose::STANDARD
        .decode(body)
        .expect("rcgen writes valid PEM")
}

/// Whether `haystack` contains `needle`, for the OID checks above.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// The migration that made room for `cloudtak_onboarding` rebuilds the whole
/// `certificates` table, because SQLite cannot alter a `CHECK` in place.
///
/// A rebuild is a drop, and `foreign_keys` is on for every connection, so the
/// two `ON DELETE SET NULL` references to this table would quietly empty
/// themselves as it went: a certificate's issuer, and the certificate a device
/// last presented. `an_upgraded_database_has_the_same_schema_as_a_fresh_one`
/// in `db::migrations` already proves the *shape* comes out right; this proves
/// the rows do.
#[actix_web::test]
async fn the_rebuild_that_made_room_keeps_every_row_and_both_links() {
    use rustak_server::db::Database;

    // The schema as it stood just before this brief's own migration, found by
    // name rather than by number: migrations are numbered in the order they are
    // written, and another brief landing one first moves this one along.
    let mine = rustak_server::db::migrations::load()
        .expect("the compiled-in migrations load")
        .into_iter()
        .find(|migration| migration.name.ends_with("_cloudtak_onboarding.sql"))
        .expect("this brief's migration is compiled in")
        .id;

    let db = Database::open_in_memory_at_migration(mine - 1)
        .await
        .unwrap();

    db.write(|tx| {
        tx.execute_batch(
            "INSERT INTO users \
               (id, username, kind, is_admin, disabled, source, created_at, updated_at) \
               VALUES (1, 'ada', 'person', 1, 0, 'local', '2026-01-01T00:00:00.000Z', \
                       '2026-01-01T00:00:00.000Z');
             INSERT INTO devices (id, uid, user_id, first_seen_at, last_seen_at) \
               VALUES (1, 'ANDROID-1', 1, '2026-01-01T00:00:00.000Z', \
                       '2026-01-01T00:00:00.000Z');
             INSERT INTO certificates \
               (id, kind, source, issued_via, serial_hex, fingerprint, subject_cn, der, \
                not_before, not_after, created_at) \
               VALUES (1, 'ca', 'internal', NULL, 'aa', 'ca-print', 'rustak CA', x'00', \
                       '2026-01-01T00:00:00.000Z', '2036-01-01T00:00:00.000Z', \
                       '2026-01-01T00:00:00.000Z');
             INSERT INTO certificates \
               (id, kind, source, issued_via, serial_hex, fingerprint, subject_cn, der, \
                user_id, device_id, issuer_id, not_before, not_after, created_at) \
               VALUES (2, 'client', 'enrollment', 'enroll_v2_xml', 'bb', 'leaf-print', 'ada', \
                       x'00', 1, 1, 1, '2026-01-01T00:00:00.000Z', '2027-01-01T00:00:00.000Z', \
                       '2026-01-01T00:00:00.000Z');
             UPDATE devices SET last_certificate_id = 2 WHERE id = 1;",
        )
    })
    .await
    .unwrap();

    db.upgrade().await.unwrap();

    let (count, issuer, last_seen) = db
        .read(|c| {
            Ok((
                c.query_one("SELECT COUNT(*) FROM certificates", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                c.query_one(
                    "SELECT issuer_id FROM certificates WHERE id = 2",
                    [],
                    |row| row.get::<_, Option<i64>>(0),
                )?,
                c.query_one(
                    "SELECT last_certificate_id FROM devices WHERE id = 1",
                    [],
                    |row| row.get::<_, Option<i64>>(0),
                )?,
            ))
        })
        .await
        .unwrap();

    assert_eq!(count, 2, "both certificates survived the rebuild");
    assert_eq!(issuer, Some(1), "the leaf still names its authority");
    assert_eq!(last_seen, Some(2), "the device still names its certificate");

    // And the value the rebuild was for is now accepted, while a typo is not.
    db.write(|tx| {
        tx.execute(
            "INSERT INTO certificates \
               (id, kind, source, issued_via, serial_hex, fingerprint, subject_cn, der, \
                not_before, not_after, created_at) \
             VALUES (3, 'client', 'admin_package', 'cloudtak_onboarding', 'cc', 'cloudtak-print', \
                     'cloudtak', x'00', '2026-01-01T00:00:00.000Z', '2027-01-01T00:00:00.000Z', \
                     '2026-01-01T00:00:00.000Z')",
            [],
        )
    })
    .await
    .expect("the widened set accepts the hand-over");

    let refused = db
        .write(|tx| {
            tx.execute(
                "UPDATE certificates SET issued_via = 'cloudtak' WHERE id = 3",
                [],
            )
        })
        .await;

    assert!(refused.is_err(), "the set is still closed");

    let violations: i64 = db
        .read(|c| {
            let mut statement = c.prepare("PRAGMA foreign_key_check")?;
            let count = statement.query_map([], |_| Ok(()))?.count();

            Ok(count as i64)
        })
        .await
        .unwrap();

    assert_eq!(violations, 0, "the rebuilt table satisfies SQLite itself");
}

#[actix_web::test]
async fn the_surface_the_interop_suites_probe_for_answers_something_they_can_read() {
    // `interop/shared/src/probe.ts` decides a surface is absent on a `404` or
    // on HTML with a success status.
    //
    // That is why the probe path is the *download* and not the POST that
    // creates a hand-over: there is no `405` anywhere in this API — a route's
    // method guard is hoisted onto its resource, so a `GET` on a POST-only path
    // matches nothing and is answered by the scope's default service, exactly
    // as a path that does not exist is. It reads as "not served yet", and
    // probing it would skip every interop scenario for this brief in silence.
    let (server, token) = harness().await;
    let app = app!(server);

    let response = fetch!(app, &token, "/api/v1/cloudtak-onboarding/probe.p12");

    assert_eq!(response.status(), StatusCode::GONE);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json"),
        "the probe reads a success with HTML as absence",
    );

    // And the preparation really does read as absent, which is the trap this
    // documents. It used to read as absent by falling through to the admin UI's
    // shell, and on CI run 35449569084 that shell was a `500`: the `Test` job
    // never runs `trunk`, `rustak-ui/dist` is empty there, and the placeholder
    // `web::ui` served in its place carried an `Internal Server Error` status.
    // The same request on a developer's machine was a `200`, so this assertion
    // could not be true in both places (CI-01). `/api/v1` no longer falls
    // through to the shell at all, which is an answer that does not depend on
    // whether the UI was built — or on anything else about the run.
    let post_path = fetch!(app, &token, "/api/v1/users/probe/cloudtak-onboarding");

    let status = post_path.status();
    let content_type = post_path
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    // The body is read before the assertion rather than after, because a status
    // the contract does not allow is the interesting case and the body is the
    // only thing that says why — libtest captures the server's own output and
    // prints nothing but the panic.
    let body = String::from_utf8_lossy(&test::read_body(post_path).await).into_owned();

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a GET on the POST-only route is answered by the API's own default \
         service; it answered {status} with content-type {content_type:?} and \
         body: {}",
        body.chars().take(400).collect::<String>(),
    );
    assert_eq!(
        content_type.as_deref(),
        Some("application/json"),
        "the refusal is the API's own shape, not the single-page shell",
    );
}
