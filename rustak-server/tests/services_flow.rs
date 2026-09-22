//! The whole sidecar contract, in one process and over real sockets.
//!
//! Enrol → connect → register → heartbeat → react to a server event → answer a
//! configuration validation. Every step
//! goes through the thing it is testing: a real certificate authority, the real
//! `POST /Marti/api/tls/signClient/v2`, a real mutually authenticated `:8089`
//! handshake, and `rustak_client::sidecar::drive` — the loop every
//! `rustak-plugin-*` binary's `main` ends in — driving a plugin written exactly
//! as `rustak-plugin-example` is.
//!
//! # Why the plugin is written out here rather than imported
//!
//! `rustak-plugin-example` is a binary crate with no library target, so there is
//! nothing to depend on. What this suite actually exercises is the *harness*,
//! which is `rustak-client`'s; the plugin below is the example's `on_event`
//! reduced to the one arm this is about.
//!
//! # `actix_web::test` rather than `tokio::test`
//!
//! `HttpServer::run` needs an actix `System` — a current-thread tokio runtime
//! with a `LocalSet` — and `enroll_flows` binds its listener the same way. The
//! stream harness and the sidecar are ordinary tokio tasks, which that runtime
//! runs as happily as any other.
//!
//! # Plain HTTP on the API listener
//!
//! The Marti and control APIs are bound over plain HTTP here, with
//! `allow_insecure_http`, because what is under test is the enrolment and
//! control contracts rather than the TLS one — `enroll_flows` binds a real mTLS
//! listener for that. The CoT stream is still TLS, because a sidecar's
//! certificate is the whole point of enrolling it.

#![cfg(feature = "testing")]

mod stream_support;

use std::sync::Arc;
use std::time::Duration;

use rustak_api::{CredentialKind, ServiceState, UserKind};
use rustak_client::sidecar::{
    NoSettings, Sidecar, SidecarConfig, SidecarContext, SidecarEvent, async_trait, drive,
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
const SERVICE: &str = "example";

/// Its account.
const ACCOUNT: &str = "svc.example";

/// What the plugin reports to the harness, so the test can assert on a sequence
/// rather than on a clock.
#[derive(Debug)]
enum Saw {
    Connected,
    ClientConnected(String),
}

/// The example plugin's `on_event`, reduced to the arms this suite is about.
struct Watcher {
    context: Option<SidecarContext<NoSettings>>,
    seen: mpsc::UnboundedSender<Saw>,
}

#[async_trait]
impl Sidecar for Watcher {
    const NAME: &'static str = "rustak-plugin-example";
    const VERSION: &'static str = "0.0.0-test";
    type Settings = NoSettings;

    async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        self.context = Some(ctx);

        Ok(())
    }

    /// What a schema cannot say: this plugin's upstream refuses one key.
    async fn validate_config(
        &mut self,
        config: &serde_json::Value,
    ) -> rustak_client::sidecar::ConfigValidation {
        match config["api_key"].as_str() {
            Some("refused") => {
                rustak_client::sidecar::ConfigIssue::at("/api_key", "The upstream refused it.")
                    .into()
            }
            _ => rustak_client::sidecar::ConfigValidation::accepted(),
        }
    }

    async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<Event>, Error> {
        match event {
            SidecarEvent::Connected { .. } => {
                let _ = self.seen.send(Saw::Connected);
            }
            SidecarEvent::Server(event) => {
                if let rustak_api::ServerEventPayload::ClientConnected(client) = &event.payload {
                    let _ = self
                        .seen
                        .send(Saw::ClientConnected(client.username.clone()));
                }
            }
            _ => {}
        }

        Ok(Vec::new())
    }
}

/// Binds the public route tree over the harness's context, on plain HTTP.
///
/// Answers the base URL a sidecar is pointed at, and the handle that stops it.
async fn api(harness: &Harness) -> (String, actix_web::dev::ServerHandle) {
    // The authority the enrolment endpoints issue from. The stream harness loads
    // it for the listener's own TLS and does not publish it, because most of its
    // suites never call an HTTP route; this one calls the one route that is
    // nothing but the authority.
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

    // Binding is not serving: the socket above is listening the moment it is
    // bound, so a client's connection completes from the backlog, but until the
    // spawned future's workers are up actix has nobody to hand it to and drops
    // it. See `rustak_server::testing::serving`.
    rustak_server::testing::await_serving(address).await;

    (format!("http://{address}"), handle)
}

/// Creates the service account and mints the two credentials it needs.
///
/// An enrolment token to get a certificate with, and a service token to call the
/// control API with — the two credentials `docs/plugins.md` describes, and the
/// reason a sidecar can register before it has enrolled.
async fn credentials(harness: &Harness) -> (String, String) {
    // Argon2 at its real cost is ~100ms a hash, and this suite mints two
    // credentials and verifies one on every heartbeat. The cheaper parameters
    // exist for exactly this and cannot reach a deployment — see
    // `rustak_core::identity::password`.
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
                MintRequest::new(kind, "The example sidecar", &username),
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

/// The configuration file a sidecar would be deployed with.
fn sidecar_config(
    stream: std::net::SocketAddr,
    control: &str,
    token: &str,
    paths: &rustak_client::enroll::Paths,
) -> SidecarConfig<NoSettings> {
    rustak_core::config::load_str(&format!(
        r#"
        [service]
        name = "{SERVICE}"
        capabilities = ["cot.publish"]
        token = "{token}"
        certificate = "{}"
        key = "{}"
        truststore = "{}"

        [server]
        stream = "ssl://{stream}"
        control = "{control}"

        [sidecar]
        tick = "1s"
        "#,
        paths.certificate.display(),
        paths.key.display(),
        paths.truststore.display(),
    ))
    .expect("the sidecar configuration loads")
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
async fn a_sidecar_enrols_connects_registers_reports_and_hears_what_the_server_saw() {
    // `anon_group_default` on, which is the shipped default and is what the
    // step-5 assertion below depends on. The feed is filtered by channel
    // reachability — the rule `Hub::snapshot_for` applies to the client
    // listing, so that a service account with no memberships is not handed
    // every device in the installation (R-01 H3) — and this harness otherwise
    // turns the default channel off, which would leave two accounts holding
    // nothing at all unable to see each other here exactly as they would be
    // unable to see each other on the stream.
    let harness = Harness::start_with(|config| {
        config.auth.anon_group_default = true;
    })
    .await;
    // The registry the server-event feed watches: the runtime installs this when
    // the listener binds, and this harness only does so for its mission variant.
    let _ = harness.context.install_live(Arc::new(harness.live.clone()));
    let (base, api_handle) = api(&harness).await;
    let (enrolment_token, service_token) = credentials(&harness).await;
    let pki_dir = harness.data_dir.path().join("sidecar");

    // 1. Enrol. Over the real `signClient/v2`, with the one-time token, and the
    //    private key never leaves this process.
    let enrolled = rustak_client::enroll::enroll(&rustak_client::enroll::Enrolment {
        marti: &base,
        username: ACCOUNT,
        secret: &Secret::new(enrolment_token),
        client_uid: &format!("SERVICE-{SERVICE}"),
        truststore: None,
        control_truststore: None,
        // A one-time token is a password, not a bearer credential.
        credential: rustak_client::enroll::Presentation::Basic,
        // `base` is the public route tree, which is the listener that may hold
        // a publicly issued certificate.
        trust: rustak_client::http::Trust::Public,
    })
    .await
    .expect("the sidecar enrols");
    let paths = enrolled
        .write_to(&pki_dir, SERVICE)
        .expect("the files land");

    // 2. Start the harness against the real listeners.
    let (seen_tx, mut seen) = mpsc::unbounded_channel();
    let context = SidecarContext::from_config(
        sidecar_config(harness.addr, &base, &service_token, &paths),
        Watcher::VERSION,
        harness.context.shutdown().child(),
    )
    .expect("the configuration describes a usable sidecar");
    let shutdown = context.shutdown().clone();

    let sidecar = tokio::spawn(async move {
        let mut watcher = Watcher {
            context: None,
            seen: seen_tx,
        };

        drive(&mut watcher, context).await
    });

    // 3. It connects to `:8089` with the certificate it was just issued.
    let connected = tokio::time::timeout(EXPECT, seen.recv())
        .await
        .expect("the sidecar connects within the timeout");
    assert!(matches!(connected, Some(Saw::Connected)));

    // 4. It registers itself, and reports a heartbeat on its first tick.
    let db = harness.context.db();
    let name = ServiceName::parse(SERVICE).expect("a usable service name");

    until("the sidecar to register", async || {
        db.services().get_by_name(&name).await.unwrap().is_some()
    })
    .await;
    until("the sidecar to report its health", async || {
        db.services()
            .get_by_name(&name)
            .await
            .unwrap()
            .is_some_and(|row| row.status == ServiceState::Healthy)
    })
    .await;

    let registered = db.services().get_by_name(&name).await.unwrap().unwrap();
    assert_eq!(registered.version.as_deref(), Some(Watcher::VERSION));
    assert_eq!(
        registered
            .endpoints
            .as_ref()
            .and_then(|e| e.control.clone()),
        Some(base.clone()),
        "the descriptor reports the address it reached us on",
    );

    // 5. Somebody else joins the stream, and the sidecar hears about it on the
    //    server-event feed rather than by polling for it.
    let device = harness.enroll("ada", "ANDROID-1", &[]).await;
    let _eud = harness.eud(&device, "ADA").await;

    // Its own connection is on the feed too — a service is a client like any
    // other, which is the whole model — so what is asserted is that the device's
    // arrives, not that it is the first thing the plugin hears.
    let heard = tokio::time::timeout(EXPECT, async {
        while let Some(event) = seen.recv().await {
            if let Saw::ClientConnected(username) = event
                && username == "ada"
            {
                return username;
            }
        }

        panic!("the sidecar stopped before it heard about the device");
    })
    .await
    .expect("the feed delivers within the timeout");

    assert_eq!(heard, "ada");

    // 6. An administrator's candidate configuration is put to the running
    //    sidecar. The server cannot dial it, so the question goes out on the
    //    feed it holds open and the answer comes back over the control API —
    //    both of which are the harness's doing rather than the plugin's.
    let registered = db.services().get_by_name(&name).await.unwrap().unwrap();

    for (api_key, valid) in [("accepted", true), ("refused", false)] {
        let report = rustak_server::plugins::validation::validate(
            &harness.context,
            &registered,
            &serde_json::json!({ "api_key": api_key }),
        )
        .await;

        assert_eq!(
            report.service,
            rustak_api::ServiceCheck::Checked,
            "{api_key}"
        );
        assert_eq!(report.valid, valid, "{api_key}");
    }

    // 7. Stopping is clean: the loop ends, `stop` runs, and `drive` returns.
    shutdown.cancel();
    sidecar
        .await
        .expect("the sidecar task joins")
        .expect("the sidecar stops without an error");

    // The harness first: cancelling the *server's* shutdown is what ends the
    // open server-event response, and a graceful stop with one still open waits
    // for its next keepalive write to fail.
    harness.stop().await;
    api_handle.stop(true).await;
}

#[actix_web::test]
async fn a_sidecar_whose_registration_is_removed_registers_again() {
    // What an administrator does when they are tidying up: the registration goes
    // while the sidecar is running, and the sidecar's next heartbeat puts it
    // back rather than ending the process.
    let harness = Harness::start().await;
    let (base, api_handle) = api(&harness).await;
    let (_, service_token) = credentials(&harness).await;

    let context = SidecarContext::from_config(
        rustak_core::config::load_str(&format!(
            r#"
            [service]
            name = "{SERVICE}"
            token = "{service_token}"

            [server]
            control = "{base}"

            [sidecar]
            tick = "1s"
            "#
        ))
        .expect("the configuration loads"),
        "0.0.0-test",
        harness.context.shutdown().child(),
    )
    .expect("a sidecar with no stream is a usable sidecar");
    let shutdown = context.shutdown().clone();

    let (seen_tx, _seen) = mpsc::unbounded_channel();
    let sidecar = tokio::spawn(async move {
        let mut watcher = Watcher {
            context: None,
            seen: seen_tx,
        };

        drive(&mut watcher, context).await
    });

    let db = harness.context.db();
    let name = ServiceName::parse(SERVICE).expect("a usable service name");

    until("the first registration", async || {
        db.services().get_by_name(&name).await.unwrap().is_some()
    })
    .await;

    let first = db.services().get_by_name(&name).await.unwrap().unwrap();
    db.services().delete(first.id).await.unwrap();

    until("the sidecar to register again", async || {
        db.services().get_by_name(&name).await.unwrap().is_some()
    })
    .await;

    shutdown.cancel();
    sidecar.await.unwrap().unwrap();
    harness.stop().await;
    api_handle.stop(true).await;
}

/// A plugin that reports its own health through the hook, rather than letting
/// the harness report the floor.
///
/// What a feed plugin does: the state, the sentence and the counters are all
/// worked out during the tick, and `health` hands them over afterwards. Written
/// out here for the same reason `Watcher` is — `rustak-plugin-example` has no
/// library target, and what is under test is the harness.
#[derive(Default)]
struct Reporter {
    ticks: usize,
}

#[async_trait]
impl Sidecar for Reporter {
    const NAME: &'static str = "rustak-plugin-test";
    const VERSION: &'static str = "0.0.0-test";
    type Settings = NoSettings;

    async fn start(&mut self, _ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        Ok(())
    }

    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        self.ticks += 1;

        Ok(Vec::new())
    }

    async fn health(&mut self) -> Option<rustak_api::Heartbeat> {
        Some(rustak_api::Heartbeat {
            state: ServiceState::Degraded,
            message: Some("The upstream has not answered for 4 minutes.".into()),
            metrics: serde_json::json!({ "tracked": 612, "ticks": self.ticks }),
        })
    }
}

/// An administrator, and the `Authorization` header they read the listing with.
///
/// `GET /api/v1/services` is administrative — a listing of every sidecar, its
/// endpoints and its metrics is not something a service account may read (R-01
/// M16) — so the assertion below goes through a real session rather than the
/// database.
async fn administrator(harness: &Harness) -> String {
    let user = harness
        .context
        .db()
        .users()
        .create(NewUser {
            is_admin: true,
            ..NewUser::person(Username::parse("grace").expect("a usable username"))
        })
        .await
        .expect("the administrator's account");
    let session = rustak_server::testing::context::session_for(&harness.context, &user, true).await;

    rustak_server::testing::context::bearer(&session)
}

/// The service, as `GET /api/v1/services` lists it.
async fn listed(base: &str, bearer: &str, name: &str) -> Option<rustak_api::ServiceSummary> {
    let listing: Vec<rustak_api::ServiceSummary> = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .expect("a client for the test listener")
        .get(format!("{base}/api/v1/services"))
        .header("authorization", bearer)
        .send()
        .await
        .expect("the services listing answers")
        .json()
        .await
        .expect("a list of services");

    listing
        .into_iter()
        .find(|service| service.descriptor.name.as_str() == name)
}

/// Starts a control-only sidecar: no stream, no enrolment, one tick a second.
///
/// Everything these two cases are about happens between `tick` and the control
/// API, so the CoT listener would only be a certificate authority they do not
/// use.
fn spawn<S: Sidecar + Default>(
    harness: &Harness,
    base: &str,
    token: &str,
) -> (Shutdown, tokio::task::JoinHandle<Result<(), Error>>) {
    let context = SidecarContext::from_config(
        rustak_core::config::load_str(&format!(
            r#"
            [service]
            name = "{SERVICE}"
            token = "{token}"

            [server]
            control = "{base}"

            [sidecar]
            tick = "1s"
            "#
        ))
        .expect("the configuration loads"),
        S::VERSION,
        harness.context.shutdown().child(),
    )
    .expect("a sidecar with no stream is a usable sidecar");
    let shutdown = context.shutdown().clone();

    let sidecar = tokio::spawn(async move {
        let mut plugin = S::default();

        drive(&mut plugin, context).await
    });

    (shutdown, sidecar)
}

#[actix_web::test]
async fn a_plugins_own_health_report_is_what_the_services_listing_shows() {
    // The bug M9-01 and M9-02 both hit: the harness used to post
    // `Heartbeat::healthy()` unconditionally after every tick, so a plugin that
    // reported `degraded` with metrics was overwritten a few milliseconds later
    // and the Services page showed a plain healthy row with nothing in it.
    let harness = Harness::start().await;
    let (base, api_handle) = api(&harness).await;
    let (_, service_token) = credentials(&harness).await;
    let bearer = administrator(&harness).await;
    let (shutdown, sidecar) = spawn::<Reporter>(&harness, &base, &service_token);

    until("the plugin's own health to be listed", async || {
        listed(&base, &bearer, SERVICE)
            .await
            .is_some_and(|service| service.status.state == ServiceState::Degraded)
    })
    .await;

    let after_one = listed(&base, &bearer, SERVICE)
        .await
        .expect("the service is listed");

    assert_eq!(
        after_one.status.message.as_deref(),
        Some("The upstream has not answered for 4 minutes."),
        "the sentence the page shows is the plugin's, in full",
    );
    assert_eq!(after_one.metrics["tracked"], 612);

    // The second tick, and every one after it, must still be the plugin's. The
    // failure this is here to catch is a harness heartbeat *after* the hook's,
    // which would show up as a healthy row with no metrics at all.
    let first = after_one.metrics["ticks"].as_u64().expect("a tick count");

    until("a later tick", async || {
        listed(&base, &bearer, SERVICE)
            .await
            .and_then(|service| service.metrics["ticks"].as_u64())
            .is_some_and(|ticks| ticks > first)
    })
    .await;

    let after_more = listed(&base, &bearer, SERVICE)
        .await
        .expect("the service is still listed");

    assert_eq!(after_more.status.state, ServiceState::Degraded);
    assert_eq!(
        after_more.status.message, after_one.status.message,
        "nothing overwrote it",
    );
    assert_eq!(after_more.metrics["tracked"], 612);

    shutdown.cancel();
    sidecar.await.unwrap().unwrap();
    harness.stop().await;
    api_handle.stop(true).await;
}

#[actix_web::test]
async fn a_sidecar_with_the_default_health_hook_is_listed_healthy() {
    // The other half of the contract: a plugin that says nothing about its own
    // health still gets the floor, which is what makes `[server] control` worth
    // setting for a plugin that has nothing to report.
    let harness = Harness::start().await;
    let (base, api_handle) = api(&harness).await;
    let (_, service_token) = credentials(&harness).await;
    let bearer = administrator(&harness).await;
    let (shutdown, sidecar) = spawn::<Quiet>(&harness, &base, &service_token);

    until("the harness to report the floor", async || {
        listed(&base, &bearer, SERVICE)
            .await
            .is_some_and(|service| service.status.state == ServiceState::Healthy)
    })
    .await;

    let listed = listed(&base, &bearer, SERVICE)
        .await
        .expect("the service is listed");

    assert_eq!(listed.status.message, None, "healthy needs no sentence");
    assert!(
        listed
            .metrics
            .as_object()
            .is_none_or(serde_json::Map::is_empty),
        "and carries no metrics: {}",
        listed.metrics,
    );

    shutdown.cancel();
    sidecar.await.unwrap().unwrap();
    harness.stop().await;
    api_handle.stop(true).await;
}

/// A plugin with no `health` of its own, which is every plugin that has nothing
/// to say — including `rustak-plugin-example`.
#[derive(Default)]
struct Quiet;

#[async_trait]
impl Sidecar for Quiet {
    const NAME: &'static str = "rustak-plugin-test";
    const VERSION: &'static str = "0.0.0-test";
    type Settings = NoSettings;

    async fn start(&mut self, _ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        Ok(())
    }
}
