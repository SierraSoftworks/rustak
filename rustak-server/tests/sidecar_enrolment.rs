//! A sidecar that starts with no certificate and gets one for itself.
//!
//! `services_flow` enrols by *calling* `rustak_client::enroll` and handing the
//! files to the harness, which is what a test that is about the control API
//! should do. This suite is about the start-up sequence instead: a deployment
//! that has nothing but a configuration file, a service token and a one-time
//! enrolment token in the environment, started through
//! [`rustak_client::sidecar::serve`] — the same function `run` ends in, minus
//! the `report_and_exit` that would take the test process with it.
//!
//! What is asserted is the promise `docs/plugins.md` makes:
//!
//! 1. the first start enrols, writes the certificate, the key (mode `0600`) and
//!    the truststore, and connects and registers with them;
//! 2. the next start uses the files — and *cannot* have re-enrolled, because
//!    the token it was given is spent and the server would refuse it;
//! 3. a token the server refuses is a fatal, readable start-up error with no
//!    half-written identity left behind;
//! 4. `--enroll` writes the files and exits without starting the plugin.
//!
//! The listener notes from `services_flow` apply here too: `actix_web::test`
//! because `HttpServer::run` needs an actix `System`, and the Marti and control
//! APIs bound over plain HTTP because what is under test is enrolment rather
//! than the TLS contract. The CoT stream is real mutually authenticated TLS,
//! which is the point of having a certificate at all.

#![cfg(feature = "testing")]

mod stream_support;

use std::path::{Path, PathBuf};
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
const SERVICE: &str = "enrolling";

/// Its account, which is *not* its service name: enrolment presents the
/// account, and `[service] account` is what says so.
const ACCOUNT: &str = "svc.enrolling";

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

/// Everything a deployment has on disk before its first start.
struct Deployment {
    /// The directory the configuration file and the PKI live in.
    directory: tempfile::TempDir,
    config: PathBuf,
    env: PathBuf,
}

impl Deployment {
    /// Writes the configuration file, and an environment file beside it.
    ///
    /// `[service] certificate`, `key` and `truststore` are deliberately absent:
    /// this is the file an operator writes before anything has been issued, and
    /// what the harness does about that is the whole suite.
    fn new(stream: std::net::SocketAddr, control: &str, service_token: &str, token: &str) -> Self {
        let directory = tempfile::tempdir().expect("a directory for the deployment");
        let config = directory.path().join("plugin.toml");
        let env = directory.path().join(".env");

        std::fs::write(
            &config,
            format!(
                r#"
                [service]
                name = "{SERVICE}"
                account = "{ACCOUNT}"
                capabilities = ["cot.publish"]
                token = "{service_token}"

                [server]
                stream = "ssl://{stream}"
                control = "{control}"

                [sidecar]
                tick = "1s"
                "#
            ),
        )
        .expect("the configuration file lands");
        std::fs::write(&env, format!("RUSTAK_ENROLLMENT_TOKEN={token}\n"))
            .expect("the environment file lands");

        Self {
            directory,
            config,
            env,
        }
    }

    /// The command line the container's entrypoint would have.
    fn args(&self, enroll: bool) -> Args {
        Args {
            config: self.config.clone(),
            env: self.env.clone(),
            check: false,
            enroll,
        }
    }

    /// Where enrolment puts the files it was not told where to put: beside the
    /// configuration file.
    fn certificate(&self) -> PathBuf {
        self.directory.path().join(format!("{SERVICE}.pem"))
    }

    fn key(&self) -> PathBuf {
        self.directory.path().join(format!("{SERVICE}.key"))
    }

    fn truststore(&self) -> PathBuf {
        self.directory.path().join("truststore.pem")
    }
}

/// Binds the public route tree over the harness's context, on plain HTTP.
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

/// Creates the service account and mints its two credentials: a one-time
/// enrolment token to get a certificate with, and a service token to call the
/// control API with before there is one.
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
                MintRequest::new(kind, "A self-enrolling sidecar", &username),
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

/// The file's mode, for the assertion that the key is ours alone.
#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path)
        .expect("the file exists")
        .permissions()
        .mode()
        & 0o777
}

#[actix_web::test]
async fn a_sidecar_enrols_on_its_first_start_and_uses_what_it_wrote_on_the_next_one() {
    let harness = Harness::start_with(|config| {
        config.auth.anon_group_default = true;
    })
    .await;
    let _ = harness.context.install_live(Arc::new(harness.live.clone()));
    let (base, api_handle) = api(&harness).await;
    let (enrolment_token, service_token) = credentials(&harness).await;
    let deployment = Deployment::new(harness.addr, &base, &service_token, &enrolment_token);

    // The start-up order `run_with` uses: the environment file first, so that
    // RUSTAK_ENROLLMENT_TOKEN is there to be read.
    rustak_core::config::load_env_file(&deployment.env).expect("the environment file loads");

    // 1. `--enroll`: the init container's one-off task. It writes the three
    //    files and exits without starting the plugin.
    let (seen_tx, mut seen) = mpsc::unbounded_channel();
    serve(
        Watcher {
            seen: seen_tx.clone(),
        },
        &deployment.args(true),
        harness.context.shutdown().child(),
    )
    .await
    .expect("the sidecar enrols");

    assert!(deployment.certificate().exists(), "the certificate landed");
    assert!(deployment.key().exists(), "the key landed");
    assert!(
        deployment.truststore().exists(),
        "the chain became the truststore"
    );
    assert!(
        seen.try_recv().is_err(),
        "--enroll does not start the sidecar",
    );

    let issued = std::fs::read_to_string(deployment.certificate()).expect("the certificate reads");
    let (_, pem) = x509_parser::pem::parse_x509_pem(issued.as_bytes()).expect("a PEM certificate");
    let certificate = pem.parse_x509().expect("an X.509 certificate");
    assert!(
        certificate.subject().to_string().contains(ACCOUNT),
        "the certificate names the account, not the service: {}",
        certificate.subject(),
    );
    assert!(
        std::fs::read_to_string(deployment.key())
            .expect("the key reads")
            .contains("PRIVATE KEY"),
        "the key was generated here and written here",
    );

    #[cfg(unix)]
    assert_eq!(mode(&deployment.key()), 0o600, "the key is ours alone");

    // 2. The next start uses the files. It cannot have enrolled again: the
    //    token is still in the environment but it was spent by step 1, so a
    //    second signing call would be refused and this would fail rather than
    //    connect.
    let shutdown = harness.context.shutdown().child();
    let running = shutdown.clone();
    let args = deployment.args(false);
    let sidecar =
        tokio::spawn(async move { serve(Watcher { seen: seen_tx }, &args, running).await });

    tokio::time::timeout(EXPECT, seen.recv())
        .await
        .expect("the sidecar connects to the stream with the certificate it enrolled for")
        .expect("the harness reports the connection");

    assert_eq!(
        std::fs::read_to_string(deployment.certificate()).expect("the certificate reads"),
        issued,
        "the second start used the certificate it had rather than issuing another",
    );

    // 3. And it is a service like any other on the other side: registered,
    //    reporting, and visible on the Services page.
    let db = harness.context.db();
    let name = ServiceName::parse(SERVICE).expect("a usable service name");

    until("the sidecar to register", async || {
        db.services().get_by_name(&name).await.unwrap().is_some()
    })
    .await;

    let registered = db.services().get_by_name(&name).await.unwrap().unwrap();
    assert_eq!(registered.version.as_deref(), Some(Watcher::VERSION));

    shutdown.cancel();
    sidecar
        .await
        .expect("the sidecar task joins")
        .expect("the sidecar stops without an error");

    harness.stop().await;
    api_handle.stop(true).await;
}

#[actix_web::test]
async fn a_token_the_server_refuses_stops_start_up_rather_than_running_half_identified() {
    let harness = Harness::start().await;
    let (base, api_handle) = api(&harness).await;
    let (_, service_token) = credentials(&harness).await;
    // The token is written into the file here rather than into the environment:
    // `[service] enrollment_token` wins over the variable, so this test says
    // what it is about whatever else is set in this process.
    let deployment = Deployment::new(harness.addr, &base, &service_token, "unused");
    std::fs::write(
        &deployment.config,
        format!(
            r#"
            [service]
            name = "{SERVICE}"
            account = "{ACCOUNT}"
            token = "{service_token}"
            enrollment_token = "rsk_a-token-this-server-never-minted"

            [server]
            control = "{base}"
            "#
        ),
    )
    .expect("the configuration file lands");

    let (seen_tx, _seen) = mpsc::unbounded_channel();
    let err = serve(
        Watcher { seen: seen_tx },
        &deployment.args(false),
        harness.context.shutdown().child(),
    )
    .await
    .expect_err("a refused enrolment is fatal");

    assert!(err.is(human_errors::Kind::User), "{err}");
    assert!(err.to_string().contains("Could not enrol"), "{err}");
    assert!(
        err.to_string().contains(ACCOUNT),
        "the message names the account it tried: {err}",
    );
    assert!(
        err.to_string().contains("one-time"),
        "and the advice from the refusal itself survives: {err}",
    );
    assert!(
        !deployment.key().exists(),
        "a failed enrolment leaves no half-written identity behind",
    );

    harness.stop().await;
    api_handle.stop(true).await;
}
