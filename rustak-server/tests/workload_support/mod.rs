//! A deployment trusting a real orchestrator, and the calls a workload makes.
//!
//! Shared by the tests in `workload_identity`, which is the one suite that
//! uses it. What is expensive here is the server — a database, a certificate
//! authority, a `:8089` listener and an actix listener — so the suite boots one
//! per *group* of tests that cannot see each other, rather than one per test;
//! see `rustak_server::testing::cases` for how a group still reports each of
//! its members by name.
//!
//! The issuer's RSA keys are generated once per test binary, not per
//! deployment: [`TestWorkloadIssuer::warm`] forces the process-wide keys, so the
//! first deployment pays for them and every later one finds them made.

#![allow(dead_code)]

use std::sync::Arc;

use rustak_api::UserKind;
use rustak_client::stream::testing::Eud;
use rustak_client::stream::{Endpoint, StreamConfig, TlsIdentity};
use rustak_server::auth::RateLimiter;
use rustak_server::config::WorkloadConfig;
use rustak_server::db::repos::NewUser;
use rustak_server::prelude::*;
use rustak_server::testing::TestWorkloadIssuer;

use crate::stream_support::Harness;

/// The Nomad namespace the reference rule admits.
pub const NAMESPACE: &str = "default";

/// The job that runs the AIS sidecar, and the account it therefore becomes.
pub const JOB: &str = "rustak-plugin-ais";
pub const ACCOUNT: &str = "ais";

/// The `grant_type` RFC 7523 names for a JWT presented as an authorization grant.
pub const JWT_BEARER: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// What `[auth.workload]` an operator writes for the reference deployment.
fn reference(issuer: &TestWorkloadIssuer, revoke_previous: bool) -> WorkloadConfig {
    let mut workload: WorkloadConfig = toml::from_str(&format!(
        r#"
        revoke_previous = {revoke_previous}

        [[rules]]
        issuer = "nomad"
        namespace_claim = "nomad_namespace"
        namespace = "{NAMESPACE}"
        subject_claim = "nomad_job_id"
        subject_prefix = "rustak-plugin-"
        account = "strip-prefix"

        [[rules]]
        issuer = "kubernetes"
        namespace_claim = "kubernetes.io.namespace"
        namespace = "tak"
        subject_claim = "kubernetes.io.serviceaccount.name"
        subject_prefix = "rustak-plugin-"
        account = "strip-prefix"
        "#
    ))
    .expect("the reference section parses");

    workload.issuers = vec![issuer.config("nomad"), {
        let mut kubernetes = issuer.config("kubernetes");
        // The same signing authority under a second name, so one mock issuer
        // can stand in for both orchestrators; only the rules differ.
        kubernetes.discovery_url = Some(issuer.discovery_url());
        kubernetes.jwks_url = None;
        kubernetes
    }];

    workload
}

/// A running server with the reference configuration, and the issuer behind it.
pub struct Deployment {
    pub harness: Harness,
    pub issuer: TestWorkloadIssuer,
    pub base: String,
    api: actix_web::dev::ServerHandle,
    pub http: reqwest::Client,
}

impl Deployment {
    pub async fn start() -> Self {
        Self::start_with(true, |_| {}).await
    }

    /// As [`start`](Self::start), letting a test adjust the configuration.
    ///
    /// The login rate limit is raised out of reach, in both tiers. Every
    /// workload refusal from one address counts against one key, and every
    /// failure of any kind against the address, so a deployment shared by a
    /// dozen refusal cases would lock itself out at the pair's default of ten
    /// (the address tier's default is far above what the cases make, but is
    /// raised too so that its default can move freely) — and a lockout is a
    /// refusal, which would let a case pass for the wrong reason. Every case
    /// that expects a refusal also asserts it was not a `429`.
    pub async fn start_with(
        revoke_previous: bool,
        adjust: impl FnOnce(&mut rustak_server::config::Config),
    ) -> Self {
        rustak_core::identity::password::use_testing_params();

        let issuer = TestWorkloadIssuer::start().await;

        // Before anything opens a connection: see `TestWorkloadIssuer::warm`.
        issuer.warm();
        let workload = reference(&issuer, revoke_previous);
        let harness = Harness::start_with(move |config| {
            config.auth.workload = workload;
            config.auth.anon_group_default = true;
            config.auth.rate_limit.attempts = 10_000;
            config.auth.rate_limit.address_attempts = 10_000;
            config.auth.rate_limit.network_attempts = 10_000;
            adjust(config);
        })
        .await;

        let _ = harness.context.install_pki(Arc::clone(&harness.pki));
        let _ = harness.context.install_live(Arc::new(harness.live.clone()));

        let (base, api) = listener(&harness).await;

        Self {
            harness,
            issuer,
            base,
            api,
            // No connection pooling. `warm()` above closes the window this
            // suite actually hit, but any slow step between two requests
            // reopens it: the server drops an idle keep-alive connection, the
            // client writes its next request onto it, and because the write
            // succeeded hyper cannot safely retry and surfaces
            // `IncompleteMessage`. A fresh connection per request cannot race.
            // These tests make tens of requests against a loopback socket, so
            // the cost is nil.
            http: reqwest::Client::builder()
                .pool_max_idle_per_host(0)
                .build()
                .expect("a client for the test listener"),
        }
    }

    /// Creates an account of the given kind, as an administrator would have.
    pub async fn account(&self, name: &str, kind: UserKind, disabled: bool) {
        let username = Username::parse(name).expect("a usable username");
        let db = self.harness.context.db();
        let user = db
            .users()
            .create(NewUser {
                kind,
                ..NewUser::person(username)
            })
            .await
            .expect("the account under test");

        if disabled {
            db.users()
                .set_disabled(user.id, true)
                .await
                .expect("the account is switched off");
        }
    }

    /// A Nomad assertion for the job that runs the AIS sidecar.
    pub fn nomad(&self) -> String {
        self.nomad_for(JOB)
    }

    /// A Nomad assertion for another job in the admitted namespace.
    ///
    /// The reference rule strips `rustak-plugin-`, so `rustak-plugin-x` speaks
    /// for account `x`: this is how cases sharing a deployment keep to accounts
    /// of their own.
    pub fn nomad_for(&self, job: &str) -> String {
        self.issuer
            .issue(self.issuer.nomad_claims(NAMESPACE, job, "ais"))
    }

    /// The AIS assertion, which stopped being valid `ago` seconds ago.
    ///
    /// Issued an hour before it expired, as a one-hour identity is.
    pub fn expired(&self, ago: i64) -> String {
        let mut claims = self.issuer.nomad_claims(NAMESPACE, JOB, "ais");
        let expired = chrono::Utc::now().timestamp() - ago;

        claims["exp"] = serde_json::json!(expired);
        claims["iat"] = serde_json::json!(expired - 3_600);
        claims["nbf"] = serde_json::json!(expired - 3_600);

        self.issuer.issue(claims)
    }

    /// `GET /Marti/api/tls/config` with an assertion in the given header.
    pub async fn tls_config(&self, header: (&str, String)) -> reqwest::Response {
        self.http
            .get(format!("{}/Marti/api/tls/config", self.base))
            .header(header.0, header.1)
            .send()
            .await
            .expect("the enrolment listener answers")
    }

    /// The whole enrolment: the configuration call, then the signing call.
    ///
    /// Answers the certificate and the key, as a sidecar would hold them.
    pub async fn enrol(
        &self,
        account: &str,
        token: &str,
    ) -> Result<(String, String), reqwest::StatusCode> {
        let config = self
            .tls_config(("authorization", format!("Bearer {token}")))
            .await;

        if !config.status().is_success() {
            return Err(config.status());
        }

        let key = rcgen::KeyPair::generate().expect("a client key");
        let mut params = rcgen::CertificateParams::default();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, account);
        let csr = params
            .serialize_request(&key)
            .expect("a signing request")
            .pem()
            .expect("a PEM signing request");

        let response = self
            .http
            .post(format!("{}/Marti/api/tls/signClient/v2", self.base))
            .query(&[
                ("clientUid", format!("SERVICE-{account}")),
                ("version", "3".to_string()),
            ])
            .header("authorization", format!("Bearer {token}"))
            .header("accept", "application/json")
            .body(csr)
            .send()
            .await
            .expect("the signing endpoint answers");

        if !response.status().is_success() {
            return Err(response.status());
        }

        let body: serde_json::Value = response.json().await.expect("a JSON enrolment answer");
        let signed = body["signedCert"].as_str().expect("a signed certificate");

        Ok((armour(signed), key.serialize_pem()))
    }

    /// An enrolment that must be refused, and the status it was refused with.
    ///
    /// Never a `429`: see [`start_with`](Self::start_with). A refusal by the
    /// limiter says nothing about the assertion under test.
    pub async fn refused(&self, account: &str, token: &str, why: &str) -> reqwest::StatusCode {
        let status = self.enrol(account, token).await.expect_err(why);

        assert_ne!(
            status,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "refused by the rate limiter rather than for its own reason: {why}",
        );

        status
    }

    /// Whether the certificate is one the `:8089` listener really accepts.
    ///
    /// `Eud::connect` answering `Ok` proves nothing. Under TLS 1.3 the client
    /// finishes its handshake before the server has judged the *client*
    /// certificate, so a refusal arrives afterwards as a connection that goes
    /// quiet — which is exactly what `stream_session`'s foreign-authority test
    /// documents. What is asserted here is that the session reached the hub:
    /// the client announced itself and the live registry says so.
    ///
    /// Callsigns must be unique within a deployment, since the registry is
    /// what decides.
    pub async fn handshake(&self, certificate: &str, key: &str, callsign: &str) -> bool {
        let directory = tempfile::tempdir().expect("a directory for the client's material");
        let truststore = directory.path().join("ca.pem");
        let cert_file = directory.path().join("client.pem");
        let key_file = directory.path().join("client.key");

        std::fs::write(&truststore, self.harness.pki.ca().certificate_pem()).unwrap();
        std::fs::write(&cert_file, certificate).unwrap();
        std::fs::write(&key_file, key).unwrap();

        let identity = TlsIdentity::from_pem_files(&truststore, &cert_file, &key_file)
            .expect("the client's own material loads");
        let config = StreamConfig::new(
            Endpoint::tls(self.harness.addr.ip().to_string(), self.harness.addr.port()),
            format!("SERVICE-{callsign}"),
        )
        .with_tls(identity)
        .with_callsign(callsign);

        let Ok(mut eud) = Eud::connect(&config, callsign).await else {
            return false;
        };

        // A write is what drives the socket far enough for the server's
        // decision to matter; whether it reports an error here is timing, so
        // the registry is what decides.
        let _ = eud.send_sa(51.5074, -0.1278).await;

        // Five seconds, up from two when every test had a server to itself:
        // the cases of a group now share one process's CPU, and a refusal pays
        // the whole wait anyway, so a longer one only costs the negative case.
        for _ in 0..500 {
            if self
                .harness
                .live
                .snapshot()
                .iter()
                .any(|endpoint| endpoint.callsign == callsign)
            {
                return true;
            }

            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        false
    }

    /// `POST /oauth/token` with a form body.
    pub async fn token(&self, form: &[(&str, &str)]) -> reqwest::Response {
        self.http
            .post(format!("{}/oauth/token", self.base))
            .form(form)
            .send()
            .await
            .expect("the token endpoint answers")
    }

    /// `POST /api/v1/services/register` as `name`, with an access token.
    pub async fn register(&self, token: &str, name: &str) -> reqwest::Response {
        self.http
            .post(format!("{}/api/v1/services/register", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({ "name": name, "version": "0.0.0-test" }))
            .send()
            .await
            .expect("the control API answers")
    }

    pub async fn stop(self) {
        self.harness.stop().await;
        self.api.stop(true).await;
    }
}

/// Binds the public route tree over the harness's context, on plain HTTP.
async fn listener(harness: &Harness) -> (String, actix_web::dev::ServerHandle) {
    let context = harness.context.clone();
    let limiter = Arc::new(RateLimiter::new(&context.config().auth.rate_limit));
    let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let address = socket.local_addr().expect("the bound address");

    let server = actix_web::HttpServer::new(move || {
        actix_web::App::new().configure(rustak_server::web::server::services(
            context.clone(),
            limiter.clone(),
        ))
    })
    .listen(socket)
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

/// Rebuilds the PEM armour around the bare base64 the server answers with.
fn armour(bare: &str) -> String {
    let body: String = bare.split_whitespace().collect::<Vec<_>>().join("\n");

    format!("-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n")
}
