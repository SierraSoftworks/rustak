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
//! # One server per group, not per assertion
//!
//! A server is the expensive part, so the cases that cannot see one another
//! share one: `the_refusals` and `what_is_accepted` each boot a single
//! deployment and run their cases concurrently through
//! `rustak_server::testing::cases::run`, which reports every failing case by
//! name. They stay apart by **account**: each case that enrols does so as an
//! account of its own (`rustak-plugin-x` speaks for `x` under the reference
//! rule), because an enrolment supersedes that account's earlier certificates.
//! A test that changes the server's configuration or its issuer — a clock-skew
//! allowance, a token lifetime, no issuers, superseding off, a key rotation and
//! the refetch throttle — keeps a deployment of its own.
//!
//! # The refusals come first
//!
//! Deliberately, and in the file as well as in the head. A positive test alone
//! is satisfied by a server that accepts everything; every assertion below
//! about what *is* accepted is only worth reading because of the ones above it
//! about what is not.
//!
//! The Marti and control APIs are bound over plain HTTP, as `services_flow` and
//! `sidecar_enrolment` bind them: what is under test is the credential, not the
//! TLS contract.

#![cfg(feature = "testing")]

mod stream_support;
mod workload_support;

use rustak_api::{AuditCategory, UserKind};
use rustak_core::prelude::*;
use rustak_server::config::WorkloadConfig;
use rustak_server::prelude::*;
use rustak_server::testing::cases::{self, Case};

use workload_support::{ACCOUNT, Deployment, JOB, JWT_BEARER, NAMESPACE};

// ---------------------------------------------------------------------------
// The refusals.
// ---------------------------------------------------------------------------

/// The account the refusal deployment's access-control expression turns away.
const ACL_REFUSED: &str = "acl-refused";

#[actix_web::test]
async fn the_refusals() {
    // One deployment, every refusal that needs nothing but the reference
    // configuration. Its access-control expression refuses exactly one
    // account, `acl-refused`, so no other case can be refused by it.
    let deployment = Deployment::start_with(true, |config| {
        config.auth.user_acl =
            Some(filt_rs::Filter::new(format!(r#"username != "{ACL_REFUSED}""#)).unwrap());
    })
    .await;

    // The account every assertion below would otherwise be good for: the
    // refusals are about the assertion, not a missing account.
    deployment.account(ACCOUNT, UserKind::Service, false).await;
    deployment
        .account("somebody-else", UserKind::Service, false)
        .await;
    deployment.account("person", UserKind::Person, false).await;
    deployment.account("off", UserKind::Service, true).await;
    deployment
        .account(ACL_REFUSED, UserKind::Service, false)
        .await;

    let d = &deployment;
    let refusals: Vec<Case<'_>> = vec![
        (
            "an_assertion_for_another_audience_is_refused",
            Box::pin(another_audience(d)),
        ),
        (
            "an_assertion_that_leaves_its_audience_out_altogether_is_refused_too",
            Box::pin(no_audience(d)),
        ),
        (
            "an_expired_assertion_is_refused_wherever_it_is_presented",
            Box::pin(expired(d)),
        ),
        (
            "an_assertion_from_another_issuer_is_refused",
            Box::pin(another_issuer(d)),
        ),
        (
            "an_assertion_signed_by_a_key_the_issuer_does_not_publish_is_refused",
            Box::pin(unpublished_key(d)),
        ),
        (
            "an_assertion_from_another_namespace_is_refused",
            Box::pin(another_namespace(d)),
        ),
        (
            "a_job_outside_the_prefix_is_refused",
            Box::pin(outside_the_prefix(d)),
        ),
        (
            "an_assertion_that_resolves_to_a_persons_account_is_refused",
            Box::pin(a_persons_account(d)),
        ),
        (
            "an_assertion_for_an_account_that_is_switched_off_is_refused",
            Box::pin(switched_off(d)),
        ),
        (
            "an_assertion_for_an_account_that_does_not_exist_is_refused",
            Box::pin(no_such_account(d)),
        ),
        (
            "an_access_control_expression_that_refuses_the_account_stops_the_enrolment",
            Box::pin(refused_by_the_acl(d)),
        ),
        (
            "a_jwt_bearer_grant_with_an_assertion_we_would_not_accept_is_invalid_grant",
            Box::pin(invalid_grant(d)),
        ),
    ];

    cases::run(refusals).await;
    deployment.stop().await;
}

async fn another_audience(deployment: &Deployment) {
    // A Nomad cluster mints identities for Vault and Consul from the same keys.
    // The audience is the only thing that keeps one of those from enrolling
    // here, so it is the first refusal this suite asserts.
    let mut claims = deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais");
    claims["aud"] = serde_json::json!(["vault"]);

    let refused = deployment
        .refused(
            ACCOUNT,
            &deployment.issuer.issue(claims),
            "an assertion for somebody else must not enrol here",
        )
        .await;

    assert_eq!(refused, reqwest::StatusCode::UNAUTHORIZED);
}

async fn no_audience(deployment: &Deployment) {
    // `jsonwebtoken` compares `aud` only against a token that carries one, so
    // omitting it was accepted where naming the wrong one was refused — unless
    // it is required by name, which it is.
    let mut claims = deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais");
    claims.as_object_mut().unwrap().remove("aud");

    deployment
        .refused(
            ACCOUNT,
            &deployment.issuer.issue(claims),
            "an assertion with no audience",
        )
        .await;
}

async fn expired(deployment: &Deployment) {
    // The whole point of a workload identity is that it is short lived and
    // rotated; a server that ignored `exp` would turn every assertion it ever
    // saw into a permanent credential. Both routes that take one are asserted,
    // because "refused at enrolment, accepted at the token endpoint" is the
    // shape this would fail in.
    let assertion = deployment.expired(3_600);

    deployment
        .refused(ACCOUNT, &assertion, "the enrolment routes")
        .await;

    let response = deployment
        .token(&[("grant_type", JWT_BEARER), ("assertion", &assertion)])
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

    let body: serde_json::Value = response.json().await.expect("an OAuth error object");

    assert_eq!(body["error"], "invalid_grant");

    let description = body["error_description"]
        .as_str()
        .expect("a description of what was wrong");

    // `1h` rather than `1h00m`: the minutes tick over while the test runs, and
    // what is being asserted is that the holder is told which check failed and
    // roughly how far out it was — not how fast this host is.
    assert!(
        description.starts_with("exp: expired 1h") && description.ends_with(" ago"),
        "the holder of a token is told what is wrong with it: {description}",
    );
    assert!(
        !description.contains(&assertion) && !description.contains("eyJ"),
        "and never any of the token itself: {description}",
    );
}

async fn another_issuer(deployment: &Deployment) {
    // Genuinely signed, in date, correct audience — and `iss` says it came from
    // somewhere this installation never registered.
    let mut claims = deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais");
    claims["iss"] = serde_json::json!("https://nomad.somebody-elses-cluster.example.com");

    deployment
        .refused(
            ACCOUNT,
            &deployment.issuer.issue(claims),
            "an assertion from an issuer nobody registered",
        )
        .await;
}

async fn unpublished_key(deployment: &Deployment) {
    // The forgery that matters. The key set is public, so the `kid` naming a
    // legitimate key is not a secret and an attacker will reuse it; what they
    // cannot do is produce a signature that verifies against it.
    let forged = deployment
        .issuer
        .forge(deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais"));

    deployment
        .refused(ACCOUNT, &forged, "a forged signature")
        .await;
}

async fn another_namespace(deployment: &Deployment) {
    // The namespace is the boundary an operator controls with Nomad's own
    // `submit-job` capability — Nomad ACLs cannot filter by job name — so it
    // has to be the thing that decides.
    let elsewhere = deployment
        .issuer
        .issue(deployment.issuer.nomad_claims("staging", JOB, "ais"));

    deployment
        .refused(ACCOUNT, &elsewhere, "an assertion from another namespace")
        .await;
}

async fn outside_the_prefix(deployment: &Deployment) {
    let outside = deployment.issuer.issue(deployment.issuer.nomad_claims(
        NAMESPACE,
        "somebody-elses-job",
        "ais",
    ));

    deployment
        .refused("somebody-else", &outside, "a job outside the prefix")
        .await;
}

async fn a_persons_account(deployment: &Deployment) {
    // The one place a machine's mistake would reach a human being's rights.
    let refused = deployment
        .refused(
            "person",
            &deployment.nomad_for("rustak-plugin-person"),
            "a workload identity may only act as a service account",
        )
        .await;

    assert_eq!(refused, reqwest::StatusCode::FORBIDDEN);
}

async fn switched_off(deployment: &Deployment) {
    deployment
        .refused(
            "off",
            &deployment.nomad_for("rustak-plugin-off"),
            "an account that is switched off",
        )
        .await;
}

async fn no_such_account(deployment: &Deployment) {
    // The binding rule names an account; it does not create one. An
    // installation adds its service accounts deliberately.
    deployment
        .refused(
            "nobody",
            &deployment.nomad_for("rustak-plugin-nobody"),
            "an account nobody created",
        )
        .await;
}

async fn refused_by_the_acl(deployment: &Deployment) {
    let refused = deployment
        .refused(
            ACL_REFUSED,
            &deployment.nomad_for(&format!("rustak-plugin-{ACL_REFUSED}")),
            "`user_acl` gates this credential like every other",
        )
        .await;

    assert_eq!(refused, reqwest::StatusCode::FORBIDDEN);
}

async fn invalid_grant(deployment: &Deployment) {
    let forged = deployment
        .issuer
        .forge(deployment.issuer.nomad_claims(NAMESPACE, JOB, "ais"));

    for assertion in [forged.as_str(), "not-a-jwt"] {
        let response = deployment
            .token(&[("grant_type", JWT_BEARER), ("assertion", assertion)])
            .await;

        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

        let body: serde_json::Value = response.json().await.expect("an OAuth error object");
        assert_eq!(body["error"], "invalid_grant");
    }

    let missing = deployment.token(&[("grant_type", JWT_BEARER)]).await;
    assert_eq!(missing.status(), reqwest::StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// Refusals that need a deployment of their own.
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn the_clock_skew_allowance_is_a_boundary_and_not_a_door() {
    // `clock_skew` exists for two machines whose clocks differ, and it is the
    // one setting that could turn "expired" into "accepted". It is a window
    // measured from `exp`, and nothing outside it is admitted.
    //
    // Ten minutes rather than the default thirty seconds, so that what is being
    // asserted is the boundary rather than how long this test took to run: a
    // host ten times slower still puts both assertions on the same side of it.
    let deployment = Deployment::start_with(true, |config| {
        for issuer in &mut config.auth.workload.issuers {
            issuer.clock_skew = chrono::Duration::seconds(600);
        }
    })
    .await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let inside = deployment.expired(300);
    let outside = deployment.expired(900);

    assert!(
        deployment.enrol(ACCOUNT, &inside).await.is_ok(),
        "five minutes past `exp` with a ten-minute allowance is inside it",
    );
    deployment
        .refused(
            ACCOUNT,
            &outside,
            "fifteen minutes past it is not, and no allowance admits it",
        )
        .await;

    deployment.stop().await;
}

#[actix_web::test]
async fn an_installation_with_no_issuers_accepts_no_assertion_at_all() {
    let deployment = Deployment::start_with(true, |config| {
        config.auth.workload = WorkloadConfig::default();
    })
    .await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    deployment
        .refused(ACCOUNT, &deployment.nomad(), "no issuer is registered")
        .await;
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
    //
    // A deployment of its own: it rotates its issuer's keys and counts that
    // issuer's key-set fetches, both of which any other case would disturb.
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
        deployment
            .refused(ACCOUNT, &never, "a key that will never exist")
            .await;
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
async fn what_is_accepted() {
    // One deployment with the reference configuration and nothing else. Every
    // case that enrols does so as an account nobody else here uses, because
    // `revoke_previous` takes back the account's earlier certificates.
    let deployment = Deployment::start().await;

    for account in [ACCOUNT, "audited", "adsb", "moved", "granted"] {
        deployment.account(account, UserKind::Service, false).await;
    }
    deployment
        .account("cloudtak", UserKind::Person, false)
        .await;

    let d = &deployment;
    let accepted: Vec<Case<'_>> = vec![
        (
            "a_nomad_job_enrols_and_the_certificate_it_gets_completes_a_handshake",
            Box::pin(a_nomad_job_enrols(d)),
        ),
        (
            "the_enrolment_is_audited_with_the_run_that_made_it_and_never_the_token",
            Box::pin(the_enrolment_is_audited(d)),
        ),
        (
            "a_kubernetes_pod_enrols_through_the_nested_claims",
            Box::pin(a_kubernetes_pod_enrols(d)),
        ),
        (
            "a_second_enrolment_supersedes_the_first_and_the_old_certificate_stops_working",
            Box::pin(a_second_enrolment_supersedes(d)),
        ),
        (
            "the_compatibility_form_puts_the_assertion_where_a_password_would_go",
            Box::pin(the_compatibility_form(d)),
        ),
        (
            "a_jwt_bearer_grant_answers_a_token_that_reaches_the_control_api_and_nothing_more",
            Box::pin(a_jwt_bearer_grant(d)),
        ),
        (
            "the_password_grant_answers_exactly_what_it_always_did",
            Box::pin(the_password_grant(d)),
        ),
    ];

    cases::run(accepted).await;
    deployment.stop().await;
}

async fn a_nomad_job_enrols(deployment: &Deployment) {
    // The reference deployment, end to end: `tls/config`, `signClient/v2`, and
    // then the certificate against the real mutually authenticated listener.
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
}

async fn the_enrolment_is_audited(deployment: &Deployment) {
    // Its own job and account, so that the entry found is this case's and not
    // a neighbour's.
    let job = "rustak-plugin-audited";
    let token = deployment.nomad_for(job);
    deployment
        .enrol("audited", &token)
        .await
        .expect("the reference rule admits this job");

    let entries = deployment
        .harness
        .context
        .db()
        .audit(
            rustak_server::db::AuditQuery::about("audited", 20)
                .in_category(AuditCategory::Enrollment),
        )
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
    assert!(detail.contains(job), "{detail}");
    assert!(detail.contains(NAMESPACE), "{detail}");
    assert!(
        !detail.contains(&token),
        "the token must never reach the audit log",
    );
    assert!(!message.contains(&token), "nor the message: {message}");
}

async fn a_kubernetes_pod_enrols(deployment: &Deployment) {
    // The same machinery, the other orchestrator: everything Kubernetes says
    // about a pod is inside one claim whose own name contains a dot.
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
}

async fn a_second_enrolment_supersedes(deployment: &Deployment) {
    // The rescheduling story. A Nomad allocation that moves to another node
    // enrols again, and the certificate left on the old node's volume must
    // stop working — nothing was spent to get it, so nothing else would.
    let token = || deployment.nomad_for("rustak-plugin-moved");

    let (first, first_key) = deployment
        .enrol("moved", &token())
        .await
        .expect("the first allocation enrols");

    assert!(
        deployment.handshake(&first, &first_key, "FIRST").await,
        "the first certificate works before it is superseded",
    );

    let (second, second_key) = deployment
        .enrol("moved", &token())
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
        .audit(rustak_server::db::AuditQuery::about("moved", 20).in_category(AuditCategory::Pki))
        .await
        .unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry.action == "certificate.superseded"),
        "and the supersede is audited",
    );
}

async fn the_compatibility_form(deployment: &Deployment) {
    // `commoncommo`-shaped clients have a username box and a password box and
    // nothing else, so the same credential is accepted as HTTP Basic — and the
    // username it carries is ignored, because the rules decide the account.
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
}

async fn a_jwt_bearer_grant(deployment: &Deployment) {
    let response = deployment
        .token(&[
            ("grant_type", JWT_BEARER),
            ("assertion", &deployment.nomad_for("rustak-plugin-granted")),
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

    let control = deployment.register(token, "granted").await;
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
}

async fn the_password_grant(deployment: &Deployment) {
    // The grant CloudTAK's whole login hangs off. Adding a fourth grant type to
    // the same endpoint must not have moved a byte of it.
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
}

// ---------------------------------------------------------------------------
// What is accepted, with a configuration of its own.
// ---------------------------------------------------------------------------

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
async fn a_sidecar_whose_token_file_is_renewed_keeps_its_control_link() {
    // The production defect, end to end. A sidecar's access token is spent
    // (`access_token_ttl` is shorter than the client's renewal margin, so every
    // call exchanges again) and the assertion it would present has expired —
    // which is what a credential read once from the environment looks like an
    // hour later. The orchestrator then rewrites the file, and the *file* is
    // what a sidecar reads, every time.
    //
    // Nothing here waits: what moves is the content of a file and a lifetime in
    // the configuration.
    let deployment = Deployment::start_with(true, |config| {
        config.auth.access_token_ttl = chrono::Duration::seconds(30);
    })
    .await;
    deployment.account(ACCOUNT, UserKind::Service, false).await;

    let directory = tempfile::tempdir().expect("a directory for the task's secrets");
    let path = directory.path().join("nomad_rustak.jwt");
    std::fs::write(&path, deployment.nomad()).expect("Nomad writes the identity");

    let tokens = rustak_client::sidecar::AccessTokens::new(
        rustak_client::sidecar::Source::File(path.clone()),
        deployment.base.clone(),
        reqwest::Client::new(),
    );

    let first = tokens.current().await.expect("the first exchange");

    // An hour later, as a frozen copy of the identity would be.
    std::fs::write(&path, deployment.expired(3_600)).expect("the stale identity");

    let refused = tokens
        .current()
        .await
        .expect_err("an expired assertion buys nothing");

    assert!(
        refused.to_string().contains("expired 1h00m ago"),
        "and this end says so without being told twice: {refused}",
    );
    assert!(
        refused.to_string().contains("nomad_rustak.jwt"),
        "naming the source it read: {refused}",
    );

    // Nomad renews by rewriting the file. Nothing restarts, nothing is
    // reconfigured, and the next call is simply a call.
    std::fs::write(&path, deployment.nomad()).expect("Nomad renews the identity");

    let renewed = tokens.current().await.expect("the renewed exchange");

    assert_ne!(
        first.expose(),
        renewed.expose(),
        "a second access token, bought with the renewed assertion",
    );

    let control = deployment.register(renewed.expose(), ACCOUNT).await;

    assert!(
        control.status().is_success(),
        "and the control link is up on the far side of the renewal: {}",
        control.status(),
    );

    deployment.stop().await;
}
