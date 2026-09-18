//! The server, started the way `main` starts it.
//!
//! Everything else in this crate tests a piece: a route through
//! `actix_web::test`, a repository against an in-memory database, a ceremony
//! against the software authenticator. None of that proves the pieces are
//! wired together, and the failures that matter most at start-up are exactly
//! the wiring ones — a certificate that is never issued, a handle installed too
//! late, a shutdown that hangs. So this suite drives [`rustak_server::run`] over
//! real sockets: a temporary data directory, a listener presenting a
//! certificate from an authority that did not exist a second ago, an HTTP
//! client that trusts it, and the whole first-run wizard through to a session
//! that `/api/v1/me` accepts.
//!
//! # Why the client is told how to resolve the host
//!
//! WebAuthn identifies a relying party by domain, so `https://127.0.0.1` cannot
//! register a passkey at all. The server is therefore reached at `localhost`,
//! and `reqwest`'s own resolver override points that at the loopback address
//! the listener bound — which also means these tests never depend on what the
//! machine's `/etc/hosts` says, or on whether `localhost` resolves to IPv6
//! first.
//!
//! Run with `cargo test -p rustak-server --features testing`.

#![cfg(feature = "testing")]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rustak_api::{AdminCreated, Me, PasskeyChallenge, SetupStatus, TokenResponse};
use rustak_core::config::ListenAddr;
use rustak_core::runtime::Shutdown;
use rustak_core::telemetry::Session;
use rustak_server::config::{Config, TlsMode};
use rustak_server::testing::SoftAuthenticator;
use tokio::task::JoinHandle;

/// How long a test waits for the listener to come up.
///
/// Generous, because a first start generates an RSA certificate authority and
/// a signing key, which on a loaded CI runner is seconds rather than
/// milliseconds.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a cancelled server is given to return, per design 01 §8 step 12.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// A server that has stopped, and the data directory it left behind.
struct Stopped {
    outcome: Result<(), human_errors::Error>,
    wal: std::path::PathBuf,
    data_dir: tempfile::TempDir,
}

impl Stopped {
    /// Asserts that the server returned `Ok(())` and folded its log back in.
    fn assert_clean(self) {
        self.outcome.expect("a cancelled server returns Ok");

        let wal = self
            .wal
            .metadata()
            .map(|meta| meta.len())
            .unwrap_or_else(|err| panic!("the -wal file should still be there: {err}"));

        assert_eq!(
            wal, 0,
            "the write-ahead log should have been truncated on the way out",
        );

        drop(self.data_dir);
    }
}

/// A server running in this process, and what is needed to talk to it.
struct Running {
    shutdown: Shutdown,
    handle: JoinHandle<Result<(), human_errors::Error>>,
    client: reqwest::Client,
    base: String,
    data_dir: tempfile::TempDir,
    config: Config,
}

impl Running {
    /// The full URL of a path on the server under test.
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Cancels the server and waits for `run` to return.
    ///
    /// The client goes first, because actix drains rather than cuts: an idle
    /// keep-alive connection — and over TLS that is an HTTP/2 one, because the
    /// listener advertises `h2` — holds the drain open for the full
    /// `shutdown_timeout`. A browser that is still connected really would cost
    /// those ten seconds, so this closes the connection the way a client
    /// leaving does, and what is measured afterwards is the shutdown rather
    /// than that timeout.
    async fn stop(self) -> Stopped {
        drop(self.client);
        self.shutdown.cancel();

        let outcome = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.handle)
            .await
            .expect("a cancelled server returns within ten seconds")
            .expect("the server task should not panic");

        Stopped {
            outcome,
            wal: wal_path(&self.config),
            // Returned rather than dropped, because dropping it deletes the
            // data directory — and a test that asserted on a file inside one
            // that had already gone would pass by reading nothing.
            data_dir: self.data_dir,
        }
    }
}

/// A port nothing is listening on.
///
/// The socket is closed before the server binds it, which is a race in
/// principle; in practice the operating system does not hand the same ephemeral
/// port out twice in the moment between, and the alternative — asking the
/// server which port it ended up on — would be API surface that exists only for
/// this file.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a free loopback port")
        .local_addr()
        .expect("the bound address")
        .port()
}

/// A telemetry session that records into memory.
fn session() -> Arc<Session> {
    Arc::new(Session::new("rustak", "0.0.0-test").with_battery(tracing_batteries::Testing))
}

/// Starts a server on a port of its own and waits until it answers.
///
/// `tls` chooses between the internal authority and the plaintext listener the
/// insecure development mode allows, which are the two shapes `[web.public]`
/// can take in M0.
async fn start(tls: bool) -> Running {
    let data_dir = tempfile::tempdir().expect("a temporary data directory");
    let port = free_port();
    let address: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .expect("a loopback address");

    let mut config = Config::testing(data_dir.path());
    config.web.public.listen = vec![ListenAddr::new("127.0.0.1", port)];
    config.server.base_url = Some(format!(
        "{}://localhost:{port}",
        if tls { "https" } else { "http" }
    ));

    if tls {
        config.web.public.tls.mode = TlsMode::Internal;
        config.web.public.allow_insecure_http = false;
    }

    let shutdown = Shutdown::new();
    let handle = tokio::spawn(rustak_server::run(
        config.clone(),
        session(),
        shutdown.clone(),
    ));

    let base = config
        .server
        .base_url
        .clone()
        .expect("the base URL just configured");

    let client = client(tls, config.server.data_dir.as_path(), address).await;
    await_listener(&client, &base).await;

    Running {
        shutdown,
        handle,
        client,
        base,
        data_dir,
        config,
    }
}

/// An HTTP client that trusts this installation's own authority and resolves
/// `localhost` to the socket the listener bound.
async fn client(tls: bool, data_dir: &Path, address: SocketAddr) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .resolve("localhost", address)
        .timeout(Duration::from_secs(20));

    if tls {
        // The authority is created during start-up, so it is not on disk yet
        // when this is called — which is also the first thing this suite
        // proves, because a client that cannot find it cannot trust it.
        let ca = await_file(&rustak_server::pki::ca_certificate_path(data_dir)).await;

        builder = builder.add_root_certificate(
            reqwest::Certificate::from_pem(&ca).expect("the exported authority is valid PEM"),
        );
    }

    builder.build().expect("an HTTP client")
}

/// Waits for a file start-up writes, and reads it.
async fn await_file(path: &Path) -> Vec<u8> {
    let deadline = std::time::Instant::now() + STARTUP_TIMEOUT;

    loop {
        if let Ok(contents) = tokio::fs::read(path).await {
            return contents;
        }

        assert!(
            std::time::Instant::now() < deadline,
            "{} was never written",
            path.display(),
        );

        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Waits until the listener answers, rather than guessing how long it takes.
async fn await_listener(client: &reqwest::Client, base: &str) {
    let deadline = std::time::Instant::now() + STARTUP_TIMEOUT;

    loop {
        let last = match client.get(format!("{base}/api/v1/health")).send().await {
            Ok(response) if response.status().is_success() => return,
            Ok(response) => response.status().to_string(),
            Err(err) => err.to_string(),
        };

        assert!(
            std::time::Instant::now() < deadline,
            "the listener never came up: {last}",
        );

        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The body of a successful request, as JSON.
async fn get_json<T: serde::de::DeserializeOwned>(server: &Running, path: &str) -> T {
    let response = server
        .client
        .get(server.url(path))
        .send()
        .await
        .unwrap_or_else(|err| panic!("GET {path}: {err}"));

    assert!(response.status().is_success(), "GET {path}: {:?}", response);

    response.json().await.expect("a JSON body")
}

/// Posts JSON and asserts the request succeeded.
async fn post_json<T: serde::de::DeserializeOwned>(
    server: &Running,
    path: &str,
    body: serde_json::Value,
    bearer: Option<&str>,
) -> T {
    let mut request = server.client.post(server.url(path)).json(&body);

    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }

    let response = request
        .send()
        .await
        .unwrap_or_else(|err| panic!("POST {path}: {err}"));
    let status = response.status();
    let text = response.text().await.expect("a response body");

    assert!(status.is_success(), "POST {path} answered {status}: {text}");

    serde_json::from_str(&text).unwrap_or_else(|err| panic!("POST {path} returned {text}: {err}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_first_start_serves_its_own_tls_and_walks_an_operator_all_the_way_in() {
    let server = start(true).await;

    // The embedded UI, over a certificate issued minutes ago by an authority
    // that did not exist before this test ran.
    let robots = server
        .client
        .get(server.url("/robots.txt"))
        .send()
        .await
        .expect("GET /robots.txt");
    assert!(robots.status().is_success());
    assert!(robots.text().await.unwrap().contains("Disallow"));

    let health: serde_json::Value = get_json(&server, "/api/v1/health").await;
    assert_eq!(health["status"], "ok", "{health}");

    // The wizard, from the token on disk to a session.
    let status: SetupStatus = get_json(&server, "/api/v1/setup/status").await;
    assert!(status.needs_setup && !status.has_admin);

    let token = String::from_utf8(await_file(&server.config.setup_token_file()).await)
        .expect("the setup token is text");

    let created: AdminCreated = post_json(
        &server,
        "/api/v1/setup/admin",
        serde_json::json!({
            "setup_token": token.trim(),
            "username": "ada",
            "display_name": "Ada Lovelace",
        }),
        None,
    )
    .await;
    assert_eq!(created.username.as_str(), "ada");

    let challenge: PasskeyChallenge = post_json(
        &server,
        "/api/v1/auth/passkey/register/start",
        serde_json::json!({
            "label": "Laptop",
            "registration_token": created.registration_token,
        }),
        None,
    )
    .await;

    let authenticator = SoftAuthenticator::new(&server.base);
    let credential = authenticator.create(&challenge.options);

    // The wizard's registration answers with a session, because the account it
    // just created has no other way to authenticate.
    let signed_in: TokenResponse = post_json(
        &server,
        "/api/v1/auth/passkey/register/finish",
        serde_json::json!({
            "challenge_id": challenge.challenge_id,
            "credential": credential,
        }),
        None,
    )
    .await;

    let me: Me = server
        .client
        .get(server.url("/api/v1/me"))
        .bearer_auth(&signed_in.token)
        .send()
        .await
        .expect("GET /api/v1/me")
        .json()
        .await
        .expect("a JSON identity");

    assert_eq!(me.username.as_str(), "ada");
    assert!(me.is_admin, "the wizard creates an administrator");

    // The whole point of closing the database rather than dropping it: what is
    // left behind is a database and an empty log, not megabytes of pending
    // writes somebody's backup would miss.
    server.stop().await.assert_clean();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_insecure_development_listener_serves_plaintext_and_still_stops_cleanly() {
    // The other shape `[web.public]` can take, and the one every other test in
    // this crate runs under — worth proving it binds a real socket too.
    let server = start(false).await;

    let robots = server
        .client
        .get(server.url("/robots.txt"))
        .send()
        .await
        .expect("GET /robots.txt");
    assert!(robots.status().is_success());

    let started = std::time::Instant::now();
    server.stop().await.assert_clean();

    assert!(
        started.elapsed() < SHUTDOWN_TIMEOUT,
        "a server with nothing in flight should stop promptly",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_listener_that_cannot_bind_reports_it_rather_than_running_without_one() {
    // A server that came up with no socket would look healthy in an
    // orchestrator and answer nothing, so the failure has to reach the caller.
    let data_dir = tempfile::tempdir().expect("a temporary data directory");
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to occupy");
    let port = occupied.local_addr().unwrap().port();

    let mut config = Config::testing(data_dir.path());
    config.web.public.listen = vec![ListenAddr::new("127.0.0.1", port)];

    let Err(err) = rustak_server::run(config, session(), Shutdown::new()).await else {
        panic!("binding a port somebody else holds should not start a server");
    };

    assert!(err.is(human_errors::Kind::User), "{err}");
    assert!(err.to_string().contains(&port.to_string()), "{err}");
}

/// Where the write-ahead log lives for a configured server.
fn wal_path(config: &Config) -> std::path::PathBuf {
    let database = config.database_path();
    let mut name = database.file_name().unwrap_or_default().to_os_string();
    name.push("-wal");

    database.with_file_name(name)
}
