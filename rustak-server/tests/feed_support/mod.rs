//! Running a feed sidecar against a real server, for the suites that assert on
//! what an operator's device actually receives.
//!
//! This is `services_flow`'s enrol → connect → register sequence with a feed
//! plugin on the end of it and a device watching the channel: a real
//! certificate authority, the real `POST /Marti/api/tls/signClient/v2`, a real
//! mutually authenticated `:8089` handshake, and `rustak_client::sidecar::drive`
//! — the loop every `rustak-plugin-*` binary's `main` ends in.
//!
//! It is generic over the plugin, so M9-01 and M9-02 point it at their own
//! sources by writing a different `[settings]` block:
//!
//! ```ignore
//! let mut feed = RunningFeed::start::<AisSidecar>("ais", SETTINGS).await;
//! let event = feed.eud.expect_uid("AIS-244660000", EXPECT).await.unwrap();
//! feed.stop().await;
//! ```
//!
//! A suite using this declares `mod stream_support;` alongside `mod
//! feed_support;`: the listener, the certificate authority and the fake EUD are
//! all that module's.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use rustak_api::{CredentialKind, UserKind};
use rustak_client::feed::Track;
use rustak_client::sidecar::{Sidecar, SidecarConfig, SidecarContext, drive};
use rustak_client::stream::testing::Eud;
use rustak_core::prelude::*;
use rustak_server::auth::RateLimiter;
use rustak_server::db::repos::NewUser;
use rustak_server::identity::credentials::{MintRequest, mint};
use rustak_server::prelude::*;

use crate::stream_support::Harness;

/// The account a feed sidecar runs as, named after its service.
fn account(service: &str) -> String {
    format!("svc.{service}")
}

/// A server, a feed sidecar connected to it, and a device watching the channel
/// the sidecar publishes on.
pub struct RunningFeed {
    /// The device the sidecar's CoT arrives at. An operator's phone, in effect.
    pub eud: Eud,
    /// The server the whole thing is running against.
    pub harness: Harness,
    /// The base URL of the Marti and control APIs.
    pub base: String,
    api: actix_web::dev::ServerHandle,
    shutdown: Shutdown,
    sidecar: tokio::task::JoinHandle<Result<(), Error>>,
    /// Held because the sidecar's certificate and replay fixture live under it.
    _files: tempfile::TempDir,
}

impl RunningFeed {
    /// Enrols `service`, connects a watching device, and starts the plugin.
    ///
    /// `settings` is the plugin's own `[settings]` block, written exactly as an
    /// operator would write it — which is what makes this reusable across
    /// sources: a suite for a live source passes its own.
    pub async fn start<S: Sidecar + Default>(service: &str, settings: &str) -> Self {
        // `anon_group_default` on, so that the service and the device are in the
        // same channel without this harness having to model channel membership
        // as well. What is under test is the feed, not the routing.
        let harness = Harness::start_with(|config| {
            config.auth.anon_group_default = true;
        })
        .await;
        let _ = harness.context.install_live(Arc::new(harness.live.clone()));

        let files = tempfile::tempdir().expect("a directory for the sidecar's files");
        let (base, api) = api(&harness).await;
        let (enrolment_token, service_token) = credentials(&harness, service).await;

        // 1. The sidecar gets its certificate the way a deployed one does: a
        //    one-time token, a signing request, and a private key that never
        //    leaves this process.
        let enrolled = rustak_client::enroll::enroll(&rustak_client::enroll::Enrolment {
            marti: &base,
            username: &account(service),
            secret: &Secret::new(enrolment_token),
            client_uid: &format!("SERVICE-{service}"),
            truststore: None,
            control_truststore: None,
            // A one-time token is a password, not a bearer credential.
            credential: rustak_client::enroll::Presentation::Basic,
            // `base` is the public route tree, which is the listener that may
            // hold a publicly issued certificate.
            trust: rustak_client::http::Trust::Public,
        })
        .await
        .expect("the sidecar enrols");
        let paths = enrolled
            .write_to(files.path().join("pki"), service)
            .expect("the certificate lands");

        // 2. The device connects *before* the plugin does, because the first
        //    tick happens as soon as the sidecar starts and a device that
        //    joined afterwards would miss it.
        let watcher = harness.enroll("watcher", "ANDROID-WATCHER", &[]).await;
        let eud = harness.eud(&watcher, "WATCHER").await;

        let config: SidecarConfig<S::Settings> = rustak_core::config::load_str(&format!(
            r#"
            [service]
            name = "{service}"
            capabilities = ["cot.publish"]
            token = "{service_token}"
            certificate = "{}"
            key = "{}"
            truststore = "{}"

            [server]
            stream = "ssl://{}"
            control = "{base}"

            [sidecar]
            tick = "1s"

            {settings}
            "#,
            paths.certificate.display(),
            paths.key.display(),
            paths.truststore.display(),
            harness.addr,
        ))
        .expect("the sidecar configuration loads");

        let context =
            SidecarContext::from_config(config, S::VERSION, harness.context.shutdown().child())
                .expect("the configuration describes a usable sidecar");
        let shutdown = context.shutdown().clone();

        let sidecar = tokio::spawn(async move {
            let mut plugin = S::default();

            drive(&mut plugin, context).await
        });

        Self {
            eud,
            harness,
            base,
            api,
            shutdown,
            sidecar,
            _files: files,
        }
    }

    /// Stops the sidecar, the listener and the API, and insists the sidecar
    /// stopped because it was asked to rather than because it failed.
    pub async fn stop(self) {
        self.shutdown.cancel();
        self.sidecar
            .await
            .expect("the sidecar task joins")
            .expect("the sidecar stops without an error");

        // The server before the API: cancelling the server's shutdown is what
        // ends the open server-event response the sidecar was holding.
        self.harness.stop().await;
        self.api.stop(true).await;
    }
}

/// Writes tracks as a replay fixture, and answers the path and the `[settings]`
/// block that names it.
///
/// The directory is the caller's to hold: a fixture whose `TempDir` was dropped
/// is a plugin that fails to start, naming a file that was there a moment ago.
pub fn replay_settings(directory: &tempfile::TempDir, tracks: &[Track]) -> (PathBuf, String) {
    let path = directory.path().join("tracks.ndjson");
    let body: String = tracks
        .iter()
        .map(|track| {
            let line = serde_json::to_string(track).expect("a track serialises");
            format!("{line}\n")
        })
        .collect();

    std::fs::write(&path, body).expect("the fixture lands");

    let settings = format!(
        "[settings.source]\nkind = \"replay\"\npath = \"{}\"\n",
        path.display(),
    );

    (path, settings)
}

/// Binds the public route tree over the harness's context, on plain HTTP.
///
/// Plain because what is under test here is the feed rather than the TLS
/// contract; the CoT stream the tracks arrive on is still mutually
/// authenticated TLS.
async fn api(harness: &Harness) -> (String, actix_web::dev::ServerHandle) {
    let _ = harness.context.install_pki(Arc::clone(&harness.pki));

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
    .listen(listener)
    .expect("the API listener binds")
    .run();
    let handle = server.handle();

    actix_web::rt::spawn(server);

    (format!("http://{address}"), handle)
}

/// Creates the service account and mints the two credentials it needs: an
/// enrolment token to get a certificate with, and a service token to call the
/// control API with.
async fn credentials(harness: &Harness, service: &str) -> (String, String) {
    // Argon2 at its real cost is ~100ms a hash, and this mints two credentials
    // and verifies one on every heartbeat.
    rustak_core::identity::password::use_testing_params();

    let context = &harness.context;
    let username = Username::parse(&account(service)).expect("a usable username");
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
                MintRequest::new(kind, "A feed sidecar", &username),
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
