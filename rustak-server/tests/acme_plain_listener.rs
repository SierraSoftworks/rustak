//! `[web.public] plain_bind` on a real socket: the challenge, and the redirect.
//!
//! The unit tests in `src/web/plain.rs` check what a `Location` is built from.
//! This suite checks the thing they add up to — that an authority validating
//! `http-01` on the plaintext port gets the answer the renewal armed on the
//! *other* listener's context, and that everything else on that port is a
//! `301` to the HTTPS origin rather than an insecure copy of the API.
//!
//! The foreign-`Host` case is the one worth a network test rather than a unit
//! one: a `Location` built from a header the caller sent is an open redirect,
//! and a cached one is everybody else's redirect too.
//!
//! Run with `cargo test -p rustak-server --features testing`.

#![cfg(feature = "testing")]

use std::net::{Ipv4Addr, TcpListener};
use std::time::Duration;

use rustak_core::config::ListenAddr;
use rustak_server::prelude::*;
use rustak_server::testing::TestServer;

/// The name this installation answers to, and the only one it redirects to.
const DOMAIN: &str = "tak.example.com";

/// The port `[web.public] listen` is configured with, which is the port a
/// redirect has to carry: a browser sent to 443 would find nothing.
const HTTPS_PORT: u16 = 8446;

/// How long the listener is given to start answering before the test fails.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// A server with a plaintext listener on a socket bound here.
///
/// The socket is bound first and handed over, as `enroll_flows` does for the
/// Marti listener: binding `:0` is the only way to be given a free port, and
/// serving on the socket that claimed it leaves no gap for anything else to
/// take it.
struct Harness {
    server: TestServer,
    port: u16,
    handle: actix_web::dev::ServerHandle,
}

impl Harness {
    async fn start() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free loopback port");
        let port = listener.local_addr().expect("the port just bound").port();

        let server = TestServer::start_with(move |config| {
            config.server.domains = vec![DOMAIN.to_string()];
            config.web.public.listen = vec![ListenAddr::new("127.0.0.1", HTTPS_PORT)];
            config.web.public.plain_bind = Some(ListenAddr::new("127.0.0.1", port));
        })
        .await;

        let plain = rustak_server::web::build_plain_on(server.context.clone(), listener)
            .expect("the plaintext listener serves on the socket it was handed");
        let handle = plain.handle();

        actix_web::rt::spawn(plain);

        let harness = Self {
            server,
            port,
            handle,
        };
        harness.await_ready().await;

        harness
    }

    /// `http://127.0.0.1:<port><path>`, which is what the authority fetches.
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// A client that reports a redirect instead of following it.
    fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .expect("a client for the test")
    }

    /// Waits until the spawned server actually has a worker on the socket.
    ///
    /// The socket is bound before the server task runs, so a request that
    /// arrives in between is completed from the backlog and then dropped.
    async fn await_ready(&self) {
        let client = self.client();
        let deadline = std::time::Instant::now() + READY_TIMEOUT;

        loop {
            if client.get(self.url("/")).send().await.is_ok() {
                return;
            }

            assert!(
                std::time::Instant::now() < deadline,
                "the plaintext listener on {} never started answering",
                self.port,
            );

            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn stop(self) {
        self.handle.stop(false).await;
    }
}

#[actix_web::test]
async fn the_authority_fetches_the_answer_the_renewal_armed() {
    let harness = Harness::start().await;
    let client = harness.client();

    // A token nobody armed is a `404`, which is what the path answers on every
    // installation that has never ordered anything.
    let missing = client
        .get(harness.url("/.well-known/acme-challenge/never-armed"))
        .send()
        .await
        .expect("the listener answers");
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    // What `order::place` does through the responder, on this server's handle.
    harness
        .server
        .context
        .acme()
        .publish("a-token", "a-token.thumbprint");

    let answered = client
        .get(harness.url("/.well-known/acme-challenge/a-token"))
        .send()
        .await
        .expect("the listener answers");

    assert_eq!(answered.status(), reqwest::StatusCode::OK);
    assert_eq!(
        answered.text().await.unwrap(),
        "a-token.thumbprint",
        "the authority compares the body byte for byte",
    );

    harness.server.context.acme().withdraw("a-token");

    let withdrawn = client
        .get(harness.url("/.well-known/acme-challenge/a-token"))
        .send()
        .await
        .expect("the listener answers");
    assert_eq!(
        withdrawn.status(),
        reqwest::StatusCode::NOT_FOUND,
        "a path that keeps serving a secret is a path somebody will find",
    );

    harness.stop().await;
}

#[actix_web::test]
async fn everything_else_is_redirected_to_the_tls_origin() {
    let harness = Harness::start().await;

    let response = harness
        .client()
        .get(harness.url("/api/v1/health"))
        .header(reqwest::header::HOST, DOMAIN)
        .send()
        .await
        .expect("the listener answers");

    assert_eq!(response.status(), reqwest::StatusCode::MOVED_PERMANENTLY);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some(format!("https://{DOMAIN}:{HTTPS_PORT}/api/v1/health").as_str()),
        "the redirect has to name the port the HTTPS listener is actually on",
    );
    assert!(
        response.text().await.unwrap_or_default().is_empty(),
        "nothing on this port serves a body somebody could act on",
    );

    harness.stop().await;
}

#[actix_web::test]
async fn a_foreign_host_header_is_sent_to_the_canonical_name_rather_than_back() {
    // An open redirect if it were echoed, and — cached by anything in front —
    // everybody else's redirect too.
    let harness = Harness::start().await;

    let response = harness
        .client()
        .get(harness.url("/login"))
        .header(reqwest::header::HOST, "evil.example.net")
        .send()
        .await
        .expect("the listener answers");

    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();

    assert_eq!(response.status(), reqwest::StatusCode::MOVED_PERMANENTLY);
    assert_eq!(location, format!("https://{DOMAIN}:{HTTPS_PORT}/login"));
    assert!(
        !location.contains("evil.example.net"),
        "the host a caller asked for is never what it is sent to: {location}",
    );

    harness.stop().await;
}

#[actix_web::test]
async fn the_api_is_not_served_on_the_plaintext_port() {
    // The whole reason this listener is not gated by `allow_insecure_http`:
    // there is nothing on it to be insecure with.
    let harness = Harness::start().await;

    let posted = harness
        .client()
        .post(harness.url("/api/v1/auth/token"))
        .header(reqwest::header::HOST, DOMAIN)
        .body("{}")
        .send()
        .await
        .expect("the listener answers");

    assert_eq!(
        posted.status(),
        reqwest::StatusCode::MOVED_PERMANENTLY,
        "a credential endpoint must not answer over plaintext, whatever the method",
    );
    assert_eq!(
        posted
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some(format!("https://{DOMAIN}:{HTTPS_PORT}/api/v1/auth/token").as_str()),
    );

    harness.stop().await;
}
