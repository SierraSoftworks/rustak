//! Enrolling and authenticating with an orchestrator's workload identity.
//!
//! The deployment this is about holds **no rustak secret at all**: a Nomad job
//! or a Kubernetes pod presents the JWT its orchestrator already gave it, and
//! that buys the certificate and the access token a sidecar used to need two
//! minted credentials for.
//!
//! Everything here goes through the real thing. A real key set served over
//! HTTP, real RS256 assertions, the real `/Marti/api/tls/*` routes over a real
//! actix listener, and — for the certificate that comes out — a real mutually
//! authenticated handshake against the real `:8089` listener. There is no
//! test-only branch anywhere in `src/`, which is the whole argument for
//! [`rustak_server::testing::TestWorkloadIssuer`] existing at all.
//!
//! # The refusals come first
//!
//! Deliberately, and in the file as well as in the head. A positive test alone
//! is satisfied by a server that accepts everything; every assertion below
//! about what *is* accepted is only worth reading because of the fourteen above
//! it about what is not.
//!
//! The Marti and control APIs are bound over plain HTTP, as `services_flow` and
//! `sidecar_enrolment` bind them: what is under test is the credential, not the
//! TLS contract.

#![cfg(feature = "testing")]

mod stream_support;

use std::sync::Arc;

use rustak_api::{AuditCategory, UserKind};
use rustak_client::stream::testing::Eud;
use rustak_client::stream::{Endpoint, StreamConfig, TlsIdentity};
use rustak_core::prelude::*;
use rustak_server::auth::RateLimiter;
use rustak_server::config::WorkloadConfig;
use rustak_server::db::repos::NewUser;
use rustak_server::prelude::*;
use rustak_server::testing::TestWorkloadIssuer;

use stream_support::Harness;

/// The Nomad namespace the reference rule admits.
const NAMESPACE: &str = "default";

/// The job that runs the AIS sidecar, and the account it therefore becomes.
const JOB: &str = "rustak-plugin-ais";
const ACCOUNT: &str = "ais";

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
struct Deployment {
    harness: Harness,
    issuer: TestWorkloadIssuer,
    base: String,
    api: actix_web::dev::ServerHandle,
    http: reqwest::Client,
}

impl Deployment {
    async fn start() -> Self {
        Self::start_with(true, |_| {}).await
    }

    /// As [`start`](Self::start), letting a test adjust the configuration.
    async fn start_with(
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
    async fn account(&self, name: &str, kind: UserKind, disabled: bool) {
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
    fn nomad(&self) -> String {
        self.issuer
            .issue(self.issuer.nomad_claims(NAMESPACE, JOB, "ais"))
    }

    /// `GET /Marti/api/tls/config` with an assertion in the given header.
    async fn tls_config(&self, header: (&str, String)) -> reqwest::Response {
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
    async fn enrol(
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

    /// Whether the certificate is one the `:8089` listener really accepts.
    ///
    /// `Eud::connect` answering `Ok` proves nothing. Under TLS 1.3 the client
    /// finishes its handshake before the server has judged the *client*
    /// certificate, so a refusal arrives afterwards as a connection that goes
    /// quiet — which is exactly what `stream_session`'s foreign-authority test
    /// documents. What is asserted here is that the session reached the hub:
    /// the client announced itself and the live registry says so.
    async fn handshake(&self, certificate: &str, key: &str, callsign: &str) -> bool {
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

        for _ in 0..200 {
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
    async fn token(&self, form: &[(&str, &str)]) -> reqwest::Response {
        self.http
            .post(format!("{}/oauth/token", self.base))
            .form(form)
            .send()
            .await
            .expect("the token endpoint answers")
    }

    async fn stop(self) {
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

// ---------------------------------------------------------------------------
// The refusals.
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn an_assertion_for_another_audience_is_refused() {
    // A Nomad cluster mints identities for Vault and Consul from the same keys.
    // The audience is the only thing that keeps one of those from enrolling
    // here, so it is the first refusal this suite asserts.
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let mut claims = deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais");
    claims["aud"] = serde_json::json!(["vault"]);

    let refused = deployment
        .enrol(ACCOUNT, &deployment.issuer.issue(claims))
        .await
        .expect_err("an assertion for somebody else must not enrol here");

    assert_eq!(refused, reqwest::StatusCode::UNAUTHORIZED);
    deployment.stop().await;
}

#[actix_web::test]
async fn an_assertion_that_leaves_its_audience_out_altogether_is_refused_too() {
    // `jsonwebtoken` compares `aud` only against a token that carries one, so
    // omitting it was accepted where naming the wrong one was refused — unless
    // it is required by name, which it is.
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let mut claims = deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais");
    claims.as_object_mut().unwrap().remove("aud");

    assert!(
        deployment
            .enrol(ACCOUNT, &deployment.issuer.issue(claims))
            .await
            .is_err()
    );
    deployment.stop().await;
}

#[actix_web::test]
async fn an_expired_assertion_is_refused() {
    // The whole point of a workload identity is that it is short lived and
    // rotated; a server that ignored `exp` would turn every assertion it ever
    // saw into a permanent credential.
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let mut claims = deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais");
    let expired = chrono::Utc::now().timestamp() - 3600;
    claims["exp"] = serde_json::json!(expired);
    claims["iat"] = serde_json::json!(expired - 60);
    claims["nbf"] = serde_json::json!(expired - 60);

    assert!(
        deployment
            .enrol(ACCOUNT, &deployment.issuer.issue(claims))
            .await
            .is_err()
    );
    deployment.stop().await;
}

#[actix_web::test]
async fn an_assertion_from_another_issuer_is_refused() {
    // Genuinely signed, in date, correct audience — and `iss` says it came from
    // somewhere this installation never registered.
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let mut claims = deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais");
    claims["iss"] = serde_json::json!("https://nomad.somebody-elses-cluster.example.com");

    assert!(
        deployment
            .enrol(ACCOUNT, &deployment.issuer.issue(claims))
            .await
            .is_err()
    );
    deployment.stop().await;
}

#[actix_web::test]
async fn an_assertion_signed_by_a_key_the_issuer_does_not_publish_is_refused() {
    // The forgery that matters. The key set is public, so the `kid` naming a
    // legitimate key is not a secret and an attacker will reuse it; what they
    // cannot do is produce a signature that verifies against it.
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let forged = deployment
        .issuer
        .forge(deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais"));

    assert!(deployment.enrol(ACCOUNT, &forged).await.is_err());
    deployment.stop().await;
}

#[actix_web::test]
async fn an_assertion_from_another_namespace_is_refused() {
    // The namespace is the boundary an operator controls with Nomad's own
    // `submit-job` capability — Nomad ACLs cannot filter by job name — so it
    // has to be the thing that decides.
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let elsewhere = deployment
        .issuer
        .issue(deployment.issuer.nomad_claims("staging", JOB, "ais"));

    assert!(deployment.enrol(ACCOUNT, &elsewhere).await.is_err());
    deployment.stop().await;
}

#[actix_web::test]
async fn a_job_outside_the_prefix_is_refused() {
    let deployment = Deployment::start().await;
    deployment
        .account("somebody-else", UserKind::Service, false)
        .await;

    let outside = deployment.issuer.issue(deployment.issuer.nomad_claims(
        NAMESPACE,
        "somebody-elses-job",
        "ais",
    ));

    assert!(deployment.enrol("somebody-else", &outside).await.is_err());
    deployment.stop().await;
}

#[actix_web::test]
async fn an_assertion_that_resolves_to_a_persons_account_is_refused() {
    // The one place a machine's mistake would reach a human being's rights.
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Person, false).await;

    let refused = deployment
        .enrol(ACCOUNT, &deployment.nomad())
        .await
        .expect_err("a workload identity may only act as a service account");

    assert_eq!(refused, reqwest::StatusCode::FORBIDDEN);
    deployment.stop().await;
}

#[actix_web::test]
async fn an_assertion_for_an_account_that_is_switched_off_is_refused() {
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, true).await;

    assert!(
        deployment
            .enrol(ACCOUNT, &deployment.nomad())
            .await
            .is_err()
    );
    deployment.stop().await;
}

#[actix_web::test]
async fn an_assertion_for_an_account_that_does_not_exist_is_refused() {
    // The binding rule names an account; it does not create one. An
    // installation adds its service accounts deliberately.
    let deployment = Deployment::start().await;

    assert!(
        deployment
            .enrol(ACCOUNT, &deployment.nomad())
            .await
            .is_err()
    );
    deployment.stop().await;
}

#[actix_web::test]
async fn an_installation_with_no_issuers_accepts_no_assertion_at_all() {
    let deployment = Deployment::start_with(true, |config| {
        config.auth.workload = WorkloadConfig::default();
    })
    .await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    assert!(
        deployment
            .enrol(ACCOUNT, &deployment.nomad())
            .await
            .is_err()
    );
    deployment.stop().await;
}

#[actix_web::test]
async fn an_access_control_expression_that_refuses_the_account_stops_the_enrolment() {
    let deployment = Deployment::start_with(true, |config| {
        config.auth.user_acl =
            Some(filt_rs::Filter::new(r#"username == "somebody-else""#).unwrap());
    })
    .await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let refused = deployment
        .enrol(ACCOUNT, &deployment.nomad())
        .await
        .expect_err("`user_acl` gates this credential like every other");

    assert_eq!(refused, reqwest::StatusCode::FORBIDDEN);
    deployment.stop().await;
}

#[actix_web::test]
async fn a_rotated_key_is_picked_up_by_the_first_assertion_that_needs_it() {
    // The rotation story, both halves. The maintainer's Nomad serves six keys
    // and rotates them, so an installation that only refetched on a timer would
    // lock every workload out until the timer came round. A `kid` we do not
    // hold sends us back to the orchestrator *on the spot* — and at most once a
    // minute, so a token naming a key that will never exist cannot be replayed
    // into a request per presentation.
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    // Caches the key set as it is now: one key.
    deployment
        .enrol(ACCOUNT, &deployment.nomad())
        .await
        .expect("the reference rule admits this job");
    let cached = deployment.issuer.jwks_fetches().await;

    let rotated = deployment
        .issuer
        .issue_rotated(deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais"));

    deployment.issuer.rotate();

    assert!(
        deployment.enrol(ACCOUNT, &rotated).await.is_ok(),
        "a key published after we cached the set must be picked up without a restart",
    );
    assert_eq!(
        deployment.issuer.jwks_fetches().await,
        cached + 1,
        "and exactly one refetch, not one per request from here on",
    );

    // A key that will never exist: refused, and the refetch is throttled, so
    // two presentations do not become two calls to somebody else's API.
    let after = deployment.issuer.jwks_fetches().await;
    let never = deployment.issuer.issue_with_kid(
        Some("a-key-nobody-has-heard-of"),
        deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais"),
    );

    for _ in 0..3 {
        assert!(deployment.enrol(ACCOUNT, &never).await.is_err());
    }

    // The window is per *issuer*, and this fixture registers two of them over
    // one mock server (a Nomad rule set and a Kubernetes one), so three
    // presentations may cost at most two refetches — never three.
    assert!(
        deployment.issuer.jwks_fetches().await <= after + 2,
        "the refetch is throttled to once a minute per issuer: {} from {after}",
        deployment.issuer.jwks_fetches().await,
    );

    deployment.stop().await;
}

// ---------------------------------------------------------------------------
// What is accepted.
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn a_nomad_job_enrols_and_the_certificate_it_gets_completes_a_handshake() {
    // The reference deployment, end to end: `tls/config`, `signClient/v2`, and
    // then the certificate against the real mutually authenticated listener.
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let (certificate, key) = deployment
        .enrol(ACCOUNT, &deployment.nomad())
        .await
        .expect("the reference rule admits this job");

    let (_, pem) = x509_parser::pem::parse_x509_pem(certificate.as_bytes()).expect("a PEM");
    let parsed = pem.parse_x509().expect("an X.509 certificate");
    assert!(
        parsed
            .subject()
            .to_string()
            .contains(&format!("CN={ACCOUNT}")),
        "the prefix rule maps {JOB} to {ACCOUNT}: {}",
        parsed.subject(),
    );

    assert!(
        deployment.handshake(&certificate, &key, "AIS-NOMAD").await,
        "the certificate a workload identity bought is a certificate like any other",
    );

    // The register says where it came from, which is what `revoke_previous`
    // matches on and what an operator counts.
    let row = deployment
        .harness
        .context
        .db()
        .certificates()
        .get_by_fingerprint(&rustak_server::pki::sha256_fingerprint(
            &rustls_pki_types::CertificateDer::from(pem.contents.clone()),
        ))
        .await
        .unwrap()
        .expect("the certificate is recorded");
    assert_eq!(
        row.issued_via.as_deref(),
        Some(rustak_server::pki::WORKLOAD_IDENTITY),
    );

    deployment.stop().await;
}

#[actix_web::test]
async fn the_enrolment_is_audited_with_the_run_that_made_it_and_never_the_token() {
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let token = deployment.nomad();
    deployment
        .enrol(ACCOUNT, &token)
        .await
        .expect("the reference rule admits this job");

    let entries = deployment
        .harness
        .context
        .db()
        .audit(rustak_server::db::AuditQuery::recent(20).in_category(AuditCategory::Enrollment))
        .await
        .unwrap();

    let workload = entries
        .iter()
        .find(|entry| entry.action == "enrollment.workload")
        .expect("a workload enrolment is audited as one");

    let detail = workload
        .detail
        .as_ref()
        .expect("a workload enrolment records what it was")
        .to_string();
    let message = workload.message.clone().unwrap_or_default();
    assert!(detail.contains("nomad"), "{detail}");
    assert!(detail.contains(JOB), "{detail}");
    assert!(detail.contains(NAMESPACE), "{detail}");
    assert!(
        !detail.contains(&token),
        "the token must never reach the audit log",
    );
    assert!(!message.contains(&token), "nor the message: {message}");

    deployment.stop().await;
}

#[actix_web::test]
async fn a_kubernetes_pod_enrols_through_the_nested_claims() {
    // The same machinery, the other orchestrator: everything Kubernetes says
    // about a pod is inside one claim whose own name contains a dot.
    let deployment = Deployment::start().await;
    deployment.account("adsb", UserKind::Service, false).await;

    let token = deployment.issuer.issue(
        deployment
            .issuer
            .kubernetes_claims("tak", "rustak-plugin-adsb"),
    );

    let (certificate, key) = deployment
        .enrol("adsb", &token)
        .await
        .expect("the Kubernetes rule admits this pod");

    assert!(deployment.handshake(&certificate, &key, "ADSB-K8S").await);

    deployment.stop().await;
}

#[actix_web::test]
async fn a_second_enrolment_supersedes_the_first_and_the_old_certificate_stops_working() {
    // The rescheduling story. A Nomad allocation that moves to another node
    // enrols again, and the certificate left on the old node's volume must
    // stop working — nothing was spent to get it, so nothing else would.
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let (first, first_key) = deployment
        .enrol(ACCOUNT, &deployment.nomad())
        .await
        .expect("the first allocation enrols");

    assert!(
        deployment.handshake(&first, &first_key, "FIRST").await,
        "the first certificate works before it is superseded",
    );

    let (second, second_key) = deployment
        .enrol(ACCOUNT, &deployment.nomad())
        .await
        .expect("the rescheduled allocation enrols again");

    assert!(
        deployment.handshake(&second, &second_key, "SECOND").await,
        "the new certificate is the one that works",
    );
    assert!(
        !deployment
            .handshake(&first, &first_key, "FIRST-AGAIN")
            .await,
        "and the one on the old node's volume does not",
    );

    let entries = deployment
        .harness
        .context
        .db()
        .audit(rustak_server::db::AuditQuery::recent(20).in_category(AuditCategory::Pki))
        .await
        .unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry.action == "certificate.superseded"),
        "and the supersede is audited",
    );

    deployment.stop().await;
}

#[actix_web::test]
async fn an_installation_that_turns_superseding_off_keeps_both_certificates() {
    // For the deployment that genuinely runs two copies of one account and has
    // thought about it.
    let deployment = Deployment::start_with(false, |_| {}).await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let (first, first_key) = deployment
        .enrol(ACCOUNT, &deployment.nomad())
        .await
        .expect("the first enrolment");
    let (second, second_key) = deployment
        .enrol(ACCOUNT, &deployment.nomad())
        .await
        .expect("the second enrolment");

    assert!(deployment.handshake(&first, &first_key, "BOTH-A").await);
    assert!(deployment.handshake(&second, &second_key, "BOTH-B").await);

    deployment.stop().await;
}

#[actix_web::test]
async fn the_compatibility_form_puts_the_assertion_where_a_password_would_go() {
    // `commoncommo`-shaped clients have a username box and a password box and
    // nothing else, so the same credential is accepted as HTTP Basic — and the
    // username it carries is ignored, because the rules decide the account.
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let response = deployment
        .http
        .get(format!("{}/Marti/api/tls/config", deployment.base))
        .basic_auth("somebody-who-is-not-the-account", Some(deployment.nomad()))
        .send()
        .await
        .expect("the enrolment listener answers");

    assert!(
        response.status().is_success(),
        "a Basic header carrying an assertion is the same credential: {}",
        response.status(),
    );

    deployment.stop().await;
}

// ---------------------------------------------------------------------------
// The token endpoint.
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn a_jwt_bearer_grant_answers_a_token_that_reaches_the_control_api_and_nothing_more() {
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let response = deployment
        .token(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &deployment.nomad()),
        ])
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json"),
        "node-tak compares this header with string equality",
    );

    let body: serde_json::Value = response.json().await.expect("a JSON token response");
    assert_eq!(body["token_type"], "Bearer");
    assert!(body["expires_in"].as_u64().unwrap_or(0) > 0);
    assert!(
        body.get("refresh_token").is_none(),
        "this grant issues no refresh token, exactly as the password grant does not",
    );

    let token = body["access_token"].as_str().expect("an access token");

    let control = deployment
        .http
        .post(format!("{}/api/v1/services/register", deployment.base))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": ACCOUNT, "version": "0.0.0-test" }))
        .send()
        .await
        .expect("the control API answers");
    assert!(
        control.status().is_success(),
        "the token a workload identity bought is what replaces the service token: {}",
        control.status(),
    );

    let admin = deployment
        .http
        .get(format!("{}/api/v1/users", deployment.base))
        .bearer_auth(token)
        .send()
        .await
        .expect("the admin API answers");
    assert!(
        !admin.status().is_success(),
        "and it reaches nothing more: {}",
        admin.status(),
    );

    deployment.stop().await;
}

#[actix_web::test]
async fn a_jwt_bearer_grant_with_an_assertion_we_would_not_accept_is_invalid_grant() {
    let deployment = Deployment::start().await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let forged = deployment
        .issuer
        .forge(deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais"));

    for assertion in [forged.as_str(), "not-a-jwt"] {
        let response = deployment
            .token(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion),
            ])
            .await;

        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

        let body: serde_json::Value = response.json().await.expect("an OAuth error object");
        assert_eq!(body["error"], "invalid_grant");
    }

    let missing = deployment
        .token(&[("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer")])
        .await;
    assert_eq!(missing.status(), reqwest::StatusCode::BAD_REQUEST);

    deployment.stop().await;
}

#[actix_web::test]
async fn the_password_grant_answers_exactly_what_it_always_did() {
    // The grant CloudTAK's whole login hangs off. Adding a fourth grant type to
    // the same endpoint must not have moved a byte of it.
    let deployment = Deployment::start().await;
    deployment
        .account("cloudtak", UserKind::Person, false)
        .await;

    let db = deployment.harness.context.db();
    let actor = Username::parse("cloudtak").expect("a usable username");
    let user = db
        .users()
        .get_by_username(&actor)
        .await
        .unwrap()
        .expect("the account");
    let password = rustak_server::identity::credentials::mint(
        db,
        &deployment.harness.context.config().auth,
        &user,
        rustak_server::identity::credentials::MintRequest::new(
            rustak_api::CredentialKind::ClientPassword,
            "CloudTAK",
            &actor,
        ),
    )
    .await
    .expect("a client password")
    .secret
    .expose()
    .to_string();

    let response = deployment
        .token(&[
            ("grant_type", "password"),
            ("username", "cloudtak"),
            ("password", &password),
        ])
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json"),
    );

    let body: serde_json::Value = response.json().await.expect("a JSON token response");
    // `serde_json` parses an object into a sorted map, so what a test can hold
    // this to is the *set* of fields rather than their order on the wire.
    let fields: std::collections::BTreeSet<&str> = body
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();

    assert_eq!(
        fields,
        ["access_token", "expires_in", "token_type"]
            .into_iter()
            .collect(),
        "the password grant's three fields, and no others",
    );
    assert_eq!(body["token_type"], "Bearer");

    deployment.stop().await;
}
