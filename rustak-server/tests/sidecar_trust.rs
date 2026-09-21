//! A deployment whose public listener does **not** hold the CA it enrolled
//! against.
//!
//! This is the shape the first live deployment had and every other suite
//! missed. `sidecar_enrolment` binds the public route tree over plain HTTP,
//! because what it is about is the start-up sequence; `services_flow` does the
//! same. So every suite passed while a sidecar that had enrolled could no
//! longer call the control API at all: enrolment writes rustak's own CA as
//! `[service] truststore`, the single HTTP client made that CA *replace* the
//! platform's roots, and the public listener was holding a Let's Encrypt
//! certificate.
//!
//! Here the public listener presents a certificate from a **second** authority
//! — the "public" CA, standing in for Let's Encrypt — while the CoT stream
//! keeps the internal one, which is exactly the deployment's shape. A sidecar
//! that gets both halves right registers, heartbeats and reads the event feed;
//! one built the old way cannot even open a connection.
//!
//! # What this can and cannot prove
//!
//! The *platform's* root store cannot be altered from a test — nothing may add
//! a root to it, and `std::env::set_var` is unsafe under the workspace's
//! `unsafe_code = "forbid"`, so `SSL_CERT_FILE` is not available either. So the
//! sidecar here pins the public listener with `[service] control_truststore`,
//! which is the *replacing* half of the rule, and the joining half — platform
//! roots **plus** `[service] truststore` — is proven by the pure-function unit
//! tests on `rustak_client::http::roots_for` and by the deployment itself.
//!
//! What this suite does prove end to end is the part that was actually broken:
//! the control endpoint is verified against a *different* set of roots from the
//! stream, and a client that uses the stream's set against it fails.

#![cfg(feature = "testing")]

mod stream_support;

use std::sync::Arc;
use std::time::Duration;

use rustak_api::{CredentialKind, UserKind};
use rustak_client::sidecar::{
    Args, NoSettings, Sidecar, SidecarContext, SidecarEvent, async_trait, serve,
};
use rustak_core::prelude::*;
use rustak_cot::Event;
use rustak_server::auth::RateLimiter;
use rustak_server::db::repos::NewUser;
use rustak_server::identity::credentials::{MintRequest, mint};
use rustak_server::prelude::*;
use tokio::sync::mpsc;

use stream_support::{EXPECT, Harness};

/// The service this suite deploys.
const SERVICE: &str = "pinned";

/// Its account; enrolment presents the account, not the service name.
const ACCOUNT: &str = "svc.pinned";

/// The plugin, reduced to "tell the test when the stream came up".
struct Watcher {
    seen: mpsc::UnboundedSender<()>,
}

#[async_trait]
impl Sidecar for Watcher {
    const NAME: &'static str = "rustak-plugin-example";
    const VERSION: &'static str = "0.0.0-test";
    type Settings = NoSettings;

    async fn start(&mut self, _ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        Ok(())
    }

    async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<Event>, Error> {
        if let SidecarEvent::Connected { .. } = event {
            let _ = self.seen.send(());
        }

        Ok(Vec::new())
    }
}

/// An authority that is not rustak's, and a server certificate it signed.
///
/// This is the Let's Encrypt of the test: a CA the deployment's own PKI has
/// never heard of, whose certificate the public listener presents.
struct PublicCa {
    certificate_pem: String,
    chain: Vec<rustls_pki_types::CertificateDer<'static>>,
    key: rustls_pki_types::PrivateKeyDer<'static>,
}

impl PublicCa {
    fn issue() -> Self {
        let ca_key = rcgen::KeyPair::generate().expect("an authority key");
        let mut ca_params = rcgen::CertificateParams::default();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.distinguished_name = rcgen::DistinguishedName::new();
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "rustak test public CA");
        let ca = ca_params.self_signed(&ca_key).expect("an authority");

        let key = rcgen::KeyPair::generate().expect("a server key");
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .expect("certificate parameters");
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "localhost");
        let issuer =
            rcgen::Issuer::from_ca_cert_der(ca.der(), ca_key).expect("the authority can sign");
        let issued = params
            .signed_by(&key, &issuer)
            .expect("the authority signs the listener's certificate");

        Self {
            certificate_pem: ca.pem(),
            chain: vec![issued.der().clone(), ca.der().clone()],
            key: rustls_pki_types::PrivateKeyDer::try_from(key.serialize_der())
                .expect("a usable key"),
        }
    }

    /// Writes the authority to a file, as an operator distributing it would.
    fn write_to(&self, path: &std::path::Path) {
        std::fs::write(path, &self.certificate_pem).expect("the public CA lands");
    }
}

/// Binds the public route tree over **TLS**, on a certificate the deployment's
/// own PKI did not issue.
async fn public_listener(harness: &Harness) -> (String, PublicCa, actix_web::dev::ServerHandle) {
    let _ = harness.context.install_pki(Arc::clone(&harness.pki));

    let authority = PublicCa::issue();
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(authority.chain.clone(), authority.key.clone_key())
        .expect("a server configuration");
    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    let context = harness.context.clone();
    let limiter = Arc::new(RateLimiter::new(&context.config().auth.rate_limit));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let address = listener.local_addr().expect("the bound address");

    let server = actix_web::HttpServer::new(move || {
        actix_web::App::new().configure(rustak_server::web::server::services(
            context.clone(),
            limiter.clone(),
        ))
    })
    .listen_rustls_0_23(listener, tls)
    .expect("the public listener binds")
    .run();
    let handle = server.handle();

    actix_web::rt::spawn(server);
    rustak_server::testing::await_serving(address).await;

    // `localhost`, not `127.0.0.1`: the certificate names the host, and a
    // sidecar configured with an address the certificate does not cover would
    // fail for a reason this suite is not about.
    (
        format!("https://localhost:{}", address.port()),
        authority,
        handle,
    )
}

/// Creates the service account and mints the two credentials it starts with.
async fn credentials(harness: &Harness) -> (String, String) {
    rustak_core::identity::password::use_testing_params();

    let context = &harness.context;
    let username = Username::parse(ACCOUNT).expect("a usable username");
    let user = context
        .db()
        .users()
        .create(NewUser {
            kind: UserKind::Service,
            ..NewUser::person(username.clone())
        })
        .await
        .expect("the service account");

    let config = context.config();
    let mut minted = Vec::new();

    for kind in [
        CredentialKind::EnrollmentToken,
        CredentialKind::ServiceToken,
    ] {
        minted.push(
            mint(
                context.db(),
                &config.auth,
                &user,
                MintRequest::new(kind, "A pinned sidecar", &username),
            )
            .await
            .expect("a credential")
            .secret
            .expose()
            .to_string(),
        );
    }

    (minted[0].clone(), minted[1].clone())
}

/// Waits for `check` to hold, or gives up with `what`.
async fn until(what: &str, mut check: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + EXPECT;

    while tokio::time::Instant::now() < deadline {
        if check().await {
            return;
        }

        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!("timed out waiting for {what}");
}

#[actix_web::test]
async fn a_sidecar_trusts_the_public_listener_and_the_stream_with_different_roots() {
    let harness = Harness::start_with(|config| {
        config.auth.anon_group_default = true;
    })
    .await;
    let _ = harness.context.install_live(Arc::new(harness.live.clone()));
    let (base, authority, api_handle) = public_listener(&harness).await;
    let (enrolment_token, service_token) = credentials(&harness).await;

    let directory = tempfile::tempdir().expect("a directory for the deployment");
    let public_ca = directory.path().join("public-ca.pem");
    authority.write_to(&public_ca);

    let config_path = directory.path().join("plugin.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
            [service]
            name = "{SERVICE}"
            account = "{ACCOUNT}"
            capabilities = ["cot.publish"]
            token = "{service_token}"
            enrollment_token = "{enrolment_token}"
            control_truststore = "{}"

            [server]
            stream = "ssl://{}"
            control = "{base}"

            [sidecar]
            tick = "1s"
            "#,
            public_ca.display(),
            harness.addr,
        ),
    )
    .expect("the configuration file lands");

    let args = Args {
        config: config_path.clone(),
        env: directory.path().join(".env"),
        check: false,
        enroll: false,
    };

    // 1. The first start enrols. It has no truststore yet, so the only thing
    //    that can verify the public listener is `control_truststore` — and it
    //    is the *only* thing, because the platform's roots have never heard of
    //    this authority either.
    let (seen_tx, mut seen) = mpsc::unbounded_channel();
    let shutdown = harness.context.shutdown().child();
    let running = shutdown.clone();
    let sidecar =
        tokio::spawn(async move { serve(Watcher { seen: seen_tx }, &args, running).await });

    // 2. The stream comes up, which means enrolment wrote the three files and
    //    the CoT stream is being verified against rustak's *internal* CA — a
    //    different set of roots from the one the control API just used.
    let connected = tokio::time::timeout(EXPECT, seen.recv())
        .await
        .expect("the sidecar connects to the stream with the certificate it enrolled for");

    if connected.is_none() {
        // The channel closes when the sidecar task ends, and a start-up failure
        // is the interesting thing this suite could find.
        panic!(
            "the sidecar stopped instead of connecting: {:?}",
            sidecar.await.expect("the sidecar task joins"),
        );
    }

    let truststore = directory.path().join("truststore.pem");
    assert!(truststore.exists(), "enrolment wrote the chain it was sent");
    assert_ne!(
        std::fs::read_to_string(&truststore).expect("the truststore reads"),
        std::fs::read_to_string(&public_ca).expect("the public CA reads"),
        "the two authorities have to be different or this suite proves nothing",
    );

    // 3. And the control API works: registration, the heartbeat and the event
    //    feed. Every one of these failed in the TLS handshake in the first live
    //    deployment, and rustak logged no request at all.
    let db = harness.context.db();
    let name = ServiceName::parse(SERVICE).expect("a usable service name");

    until("the sidecar to register", async || {
        db.services().get_by_name(&name).await.unwrap().is_some()
    })
    .await;

    let registered = db
        .services()
        .get_by_name(&name)
        .await
        .unwrap()
        .expect("the registration");
    assert_eq!(registered.version.as_deref(), Some(Watcher::VERSION));

    until("the sidecar to report a heartbeat", async || {
        db.services()
            .get_by_name(&name)
            .await
            .unwrap()
            .and_then(|service| service.last_heartbeat_at)
            .is_some()
    })
    .await;

    // The feed is the third call, and the one that is held open. A sidecar
    // whose feed never opened is one that would never hear a channel change —
    // and in the live deployment it was the call that failed silently forever.
    until("the event feed to be subscribed", async || {
        harness.context.events().subscribers() > 0
    })
    .await;

    shutdown.cancel();
    sidecar
        .await
        .expect("the sidecar task joins")
        .expect("the sidecar stops without an error");

    // 4. The counter-proof: the client this used to be — one client, verifying
    //    everything against the enrolled truststore — cannot reach the public
    //    listener at all. Without the fix the suite above would have failed
    //    here instead of passing.
    let stream_only =
        rustak_core::service::ServiceIdentity::new(name.clone()).with_truststore(&truststore);
    let old = rustak_client::http::client(
        &stream_only,
        rustak_client::http::Trust::Internal,
        rustak_client::http::DEFAULT_TIMEOUT,
    )
    .expect("a client");

    let err = old
        .get(format!("{base}/api/v1/services"))
        .send()
        .await
        .map(drop)
        .expect_err("the public listener holds a certificate this truststore does not cover");
    let rendered = rustak_client::http::transport(err, "read the services");

    assert!(
        rendered.description().contains("invalid peer certificate"),
        "and the operator is told why, rather than 'error sending request': {}",
        rendered.description(),
    );

    // And the policy the fix uses reaches it.
    let pinned = stream_only.with_control_truststore(&public_ca);
    let fixed = rustak_client::http::client(
        &pinned,
        rustak_client::http::Trust::Public,
        rustak_client::http::DEFAULT_TIMEOUT,
    )
    .expect("a client");

    assert!(
        fixed
            .get(format!("{base}/api/v1/services"))
            .send()
            .await
            .is_ok(),
        "the control policy verifies the public listener",
    );

    harness.stop().await;
    // `false`, not `true`: a graceful stop waits for the server's half of the
    // event feed to notice the sidecar has gone, which it does on its next
    // keepalive — thirty seconds of a suite that has already finished.
    api_handle.stop(false).await;
}

#[actix_web::test]
async fn a_sidecar_that_trusts_only_the_internal_ca_cannot_reach_the_public_listener() {
    // The same finding as a start-up failure rather than as a request failure:
    // a deployment that never pins the public listener and whose platform roots
    // do not cover it fails at enrolment, which is the first call it makes —
    // and the message it fails with names the cause.
    let harness = Harness::start().await;
    let (base, _authority, api_handle) = public_listener(&harness).await;
    let (enrolment_token, service_token) = credentials(&harness).await;

    let directory = tempfile::tempdir().expect("a directory for the deployment");
    let config_path = directory.path().join("plugin.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
            [service]
            name = "{SERVICE}"
            account = "{ACCOUNT}"
            token = "{service_token}"
            enrollment_token = "{enrolment_token}"

            [server]
            control = "{base}"
            "#
        ),
    )
    .expect("the configuration file lands");

    let (seen_tx, _seen) = mpsc::unbounded_channel();
    let err = serve(
        Watcher { seen: seen_tx },
        &Args {
            config: config_path,
            env: directory.path().join(".env"),
            check: false,
            enroll: false,
        },
        harness.context.shutdown().child(),
    )
    .await
    .expect_err("nothing here trusts the listener's authority");

    let described = err.to_string();

    assert!(described.contains("Could not enrol"), "{described}");
    assert!(
        described.contains("invalid peer certificate"),
        "the cause has to survive to the start-up line: {described}",
    );
    assert!(
        err.advice()
            .iter()
            .any(|line| line.contains("control_truststore")),
        "and the advice names the way out: {:?}",
        err.advice(),
    );

    assert!(
        !directory.path().join(format!("{SERVICE}.key")).exists(),
        "a failed enrolment leaves no half-written identity behind",
    );

    harness.stop().await;
    api_handle.stop(true).await;
}
