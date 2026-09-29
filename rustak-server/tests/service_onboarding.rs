//! `POST /api/v1/service-onboarding`, against the real application.
//!
//! The contract first — status, the JSON shape, and every refusal — then the
//! property that makes one action out of three: a failure part-way through
//! leaves nothing behind. The last test is the one the action exists for: a
//! sidecar started with exactly what the response said, and nothing else,
//! enrols, connects and registers.
//!
//! Run with
//! `cargo test -p rustak-server --features testing --test service_onboarding`.

#![cfg(feature = "testing")]

mod stream_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use actix_web::http::StatusCode;
use actix_web::{App, test};
use rustak_api::{
    CredentialKind, ServiceOnboarding, ServiceOnboardingRequest, ServiceTokenOutcome, UserKind,
};
use rustak_client::sidecar::{
    Args, NoSettings, Sidecar, SidecarContext, SidecarEvent, async_trait, serve,
};
use rustak_cot::Event;
use rustak_server::auth::RateLimiter;
use rustak_server::db::AuditQuery;
use rustak_server::db::repos::{NewUser, UserRow};
use rustak_server::identity::verify::verify;
use rustak_server::identity::{Purpose, VerifiedSecretCache};
use rustak_server::prelude::*;
use rustak_server::testing::TestServer;
use rustak_server::testing::context::{bearer, session_for};
use tokio::sync::mpsc;

use stream_support::{EXPECT, Harness};

macro_rules! app {
    ($server:expr) => {
        test::init_service(App::new().configure($server.app())).await
    };
}

/// Asks for a service and answers the status and the raw response.
macro_rules! onboard {
    ($app:expr, $token:expr, $body:expr) => {{
        let response = test::call_service(
            &$app,
            test::TestRequest::post()
                .uri("/api/v1/service-onboarding")
                .insert_header(("authorization", $token.to_string()))
                .set_json($body)
                .to_request(),
        )
        .await;

        (response.status(), response)
    }};
}

/// A request for `name`, with every default.
fn named(name: &str) -> ServiceOnboardingRequest {
    ServiceOnboardingRequest {
        name: name.to_string(),
        ..ServiceOnboardingRequest::default()
    }
}

/// A server with an administrator signed in.
async fn harness() -> (TestServer, String) {
    let server = TestServer::start().await;
    let (_, session) = server.signed_in("ada", true).await;

    (server, bearer(&session))
}

async fn account(server: &TestServer, username: &str) -> Option<UserRow> {
    server
        .db()
        .users()
        .get_by_username(&Username::parse(username).unwrap())
        .await
        .unwrap()
}

/// Every credential the account holds, revoked ones included.
async fn credentials(
    server: &TestServer,
    user: &UserRow,
) -> Vec<rustak_server::db::repos::CredentialRow> {
    server
        .db()
        .credentials()
        .list_for_user(user.id, true)
        .await
        .unwrap()
}

/// Makes every credential insert of `kind` (or of any kind) fail, which is the
/// second step failing for a reason of storage's own.
async fn refuse_inserts(server: &TestServer, kind: Option<&str>) {
    let when = kind.map_or_else(String::new, |kind| format!("WHEN NEW.kind = '{kind}'"));

    server
        .db()
        .write(move |tx| {
            tx.execute_batch(&format!(
                "CREATE TRIGGER refuse_credentials BEFORE INSERT ON credentials {when} \
                 BEGIN SELECT RAISE(ABORT, 'refused by the test'); END;"
            ))
        })
        .await
        .unwrap();
}

#[actix_web::test]
async fn an_administrator_adds_a_service_in_one_step() {
    let (server, token) = harness().await;
    let app = app!(server);

    let (status, response) = onboard!(
        app,
        token,
        &ServiceOnboardingRequest {
            account: Some("svc.adsb".to_string()),
            ..named("adsb")
        }
    );
    assert_eq!(status, StatusCode::OK);

    let body: serde_json::Value = test::read_body_json(response).await;
    let keys: BTreeSet<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "name",
            "account",
            "account_created",
            "enrollment_token",
            "enrollment_token_id",
            "enrollment_expires_at",
            "service_token_outcome",
            "service_token",
            "service_token_id",
            "config_fragment",
            "environment",
            "notes",
        ]),
        "{body}"
    );

    let onboarding: ServiceOnboarding = serde_json::from_value(body).unwrap();
    assert_eq!(onboarding.name.as_str(), "adsb");
    assert_eq!(onboarding.account.as_str(), "svc.adsb");
    assert!(onboarding.account_created);
    assert_eq!(
        onboarding.service_token_outcome,
        ServiceTokenOutcome::Minted
    );

    let service_token = onboarding.service_token.clone().unwrap();
    assert!(service_token.starts_with("rsk_"));

    // What an operator pastes: the fragment names the token by its variable,
    // the environment carries both values and the expiry.
    assert!(onboarding.config_fragment.contains("name = \"adsb\""));
    assert!(
        onboarding
            .config_fragment
            .contains("account = \"svc.adsb\"")
    );
    assert!(!onboarding.config_fragment.contains(&service_token));
    assert!(onboarding.environment.contains(&format!(
        "RUSTAK_ENROLLMENT_TOKEN={}\n",
        onboarding.enrollment_token
    )));
    assert!(
        onboarding
            .environment
            .contains(&format!("RUSTAK_SERVICE_TOKEN={service_token}\n"))
    );
    assert!(
        onboarding.environment.contains(
            &onboarding
                .enrollment_expires_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        )
    );
    let ttl = server.config().auth.enrollment_token_ttl;
    assert!(onboarding.enrollment_expires_at <= chrono::Utc::now() + ttl);

    // The account is a service's, and it holds exactly the two credentials.
    let user = account(&server, "svc.adsb").await.unwrap();
    assert_eq!(user.kind, UserKind::Service);
    let held = credentials(&server, &user).await;
    let kinds: BTreeSet<&str> = held.iter().map(|row| row.kind.as_str()).collect();
    assert_eq!(held.len(), 2);
    assert_eq!(
        kinds,
        BTreeSet::from([
            CredentialKind::EnrollmentToken.as_str(),
            CredentialKind::ServiceToken.as_str()
        ])
    );

    // Stored as hashes; the secrets verify for what they are for.
    for row in &held {
        assert!(!row.secret_hash.as_str().contains(&service_token));
        assert!(
            !row.secret_hash
                .as_str()
                .contains(&onboarding.enrollment_token)
        );
    }
    let cache = VerifiedSecretCache::new(Duration::from_secs(1), 8);
    verify(
        server.db(),
        &user.username,
        &service_token,
        Purpose::ServiceApi,
        &cache,
    )
    .await
    .expect("the service token reaches the control API");
    verify(
        server.db(),
        &user.username,
        &onboarding.enrollment_token,
        Purpose::Enrollment,
        &cache,
    )
    .await
    .expect("the enrolment token enrols");

    // Audited, by whom and for whom — and never with a secret in it.
    let records = server
        .db()
        .audit(AuditQuery::about("svc.adsb", 50))
        .await
        .unwrap();
    let created = records
        .iter()
        .find(|record| record.action == "service.onboarding.created")
        .expect("an audit entry for the action");
    assert_eq!(created.actor.as_deref(), Some("ada"));
    let rendered = serde_json::to_string(&records).unwrap();
    assert!(!rendered.contains(&service_token), "{rendered}");
    assert!(
        !rendered.contains(&onboarding.enrollment_token),
        "{rendered}"
    );
}

#[actix_web::test]
async fn only_an_administrator_may_add_one() {
    let server = TestServer::start().await;
    let (_, session) = server.signed_in("bob", false).await;
    let app = app!(server);

    let (status, _) = onboard!(app, bearer(&session), &named("adsb"));

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        account(&server, "adsb").await.is_none(),
        "nothing was created"
    );
}

#[actix_web::test]
async fn a_persons_account_is_refused_and_given_nothing() {
    let (server, token) = harness().await;
    let person = server.user("weather", false).await;
    let app = app!(server);

    let (status, response) = onboard!(app, token, &named("weather"));
    assert_eq!(status, StatusCode::CONFLICT);
    let body: serde_json::Value = test::read_body_json(response).await;
    assert!(body.to_string().contains("person"), "{body}");
    assert!(
        credentials(&server, &person).await.is_empty(),
        "no credential was minted for them"
    );

    // The same when the person's name is given as the account.
    let (status, _) = onboard!(
        app,
        token,
        &ServiceOnboardingRequest {
            account: Some("ada".to_string()),
            ..named("feed")
        }
    );
    assert_eq!(status, StatusCode::CONFLICT);
}

#[actix_web::test]
async fn a_name_that_cannot_be_a_service_is_a_bad_request() {
    let (server, token) = harness().await;
    let app = app!(server);

    for name in ["", "-leading", "has space", "under_score", "x"] {
        let (status, _) = onboard!(app, token, &named(name));
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name:?}");
    }

    let (status, _) = onboard!(
        app,
        token,
        &ServiceOnboardingRequest {
            account: Some("not a username!".to_string()),
            ..named("adsb")
        }
    );
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(account(&server, "adsb").await.is_none());
}

#[actix_web::test]
async fn a_switched_off_service_account_is_refused() {
    let (server, token) = harness().await;
    let user = server
        .db()
        .users()
        .create(NewUser::service(Username::parse("ais").unwrap()))
        .await
        .unwrap();
    server
        .db()
        .users()
        .set_disabled(user.id, true)
        .await
        .unwrap();
    let app = app!(server);

    let (status, _) = onboard!(app, token, &named("ais"));

    assert_eq!(status, StatusCode::CONFLICT);
    assert!(credentials(&server, &user).await.is_empty());
}

#[actix_web::test]
async fn a_name_registered_to_another_account_is_refused() {
    let (server, token) = harness().await;
    let app = app!(server);

    let (status, response) = onboard!(app, token, &named("ais"));
    assert_eq!(status, StatusCode::OK);
    let first: ServiceOnboarding = test::read_body_json(response).await;
    let owner = account(&server, "ais").await.unwrap();
    server
        .db()
        .services()
        .register(rustak_server::db::repos::NewService::new(
            first.name.clone(),
            owner.id,
        ))
        .await
        .unwrap();

    let (status, _) = onboard!(
        app,
        token,
        &ServiceOnboardingRequest {
            account: Some("svc.ais".to_string()),
            ..named("ais")
        }
    );

    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        account(&server, "svc.ais").await.is_none(),
        "nothing was created"
    );
}

#[actix_web::test]
async fn asking_again_mints_a_fresh_enrolment_token_and_revokes_nothing() {
    let (server, token) = harness().await;
    let app = app!(server);

    let (_, response) = onboard!(app, token, &named("adsb"));
    let first: ServiceOnboarding = test::read_body_json(response).await;

    let (status, response) = onboard!(app, token, &named("adsb"));
    assert_eq!(status, StatusCode::OK);
    let body: serde_json::Value = test::read_body_json(response).await;
    assert!(body.get("service_token").is_none(), "{body}");
    let second: ServiceOnboarding = serde_json::from_value(body).unwrap();

    assert!(!second.account_created);
    assert_ne!(second.enrollment_token, first.enrollment_token);
    assert_eq!(second.service_token_outcome, ServiceTokenOutcome::Kept);
    assert!(
        second
            .notes
            .iter()
            .any(|note| note.contains("already existed")),
        "{:?}",
        second.notes
    );
    assert!(
        second.notes.iter().any(|note| note.contains("Kept")),
        "{:?}",
        second.notes
    );
    assert!(!second.environment.contains("RUSTAK_SERVICE_TOKEN="));

    // Nothing that was issued the first time stopped working.
    let user = account(&server, "adsb").await.unwrap();
    let held = credentials(&server, &user).await;
    assert_eq!(held.len(), 3, "two enrolment tokens and one service token");
    assert!(held.iter().all(|row| row.revoked_at.is_none()));
}

#[actix_web::test]
async fn replacing_the_service_token_is_explicit_and_revokes_the_old_one() {
    let (server, token) = harness().await;
    let app = app!(server);

    let (_, response) = onboard!(app, token, &named("adsb"));
    let first: ServiceOnboarding = test::read_body_json(response).await;

    let (status, response) = onboard!(
        app,
        token,
        &ServiceOnboardingRequest {
            rotate_service_token: true,
            ..named("adsb")
        }
    );
    assert_eq!(status, StatusCode::OK);
    let rotated: ServiceOnboarding = test::read_body_json(response).await;

    assert_eq!(rotated.service_token_outcome, ServiceTokenOutcome::Rotated);
    assert_eq!(
        rotated.revoked_service_tokens,
        vec![first.service_token_id.unwrap()]
    );
    assert!(
        rotated.notes.iter().any(|note| note.contains("revoked")),
        "{:?}",
        rotated.notes
    );

    let user = account(&server, "adsb").await.unwrap();
    let cache = VerifiedSecretCache::new(Duration::from_secs(1), 8);
    assert!(
        verify(
            server.db(),
            &user.username,
            first.service_token.as_deref().unwrap(),
            Purpose::ServiceApi,
            &cache
        )
        .await
        .is_err(),
        "the old token no longer works",
    );
    verify(
        server.db(),
        &user.username,
        rotated.service_token.as_deref().unwrap(),
        Purpose::ServiceApi,
        &cache,
    )
    .await
    .expect("the new one does");
}

#[actix_web::test]
async fn a_failure_minting_leaves_no_account_behind() {
    for (what, kind) in [
        ("any credential", None),
        ("the service token", Some("service_token")),
    ] {
        let (server, token) = harness().await;
        refuse_inserts(&server, kind).await;
        let app = app!(server);

        let (status, _) = onboard!(app, token, &named("adsb"));

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{what}");
        assert!(
            account(&server, "adsb").await.is_none(),
            "refusing {what} left the account behind"
        );
    }
}

#[actix_web::test]
async fn a_failure_on_an_existing_account_takes_back_what_this_call_minted() {
    let (server, token) = harness().await;
    let user = server
        .db()
        .users()
        .create(NewUser::service(Username::parse("adsb").unwrap()))
        .await
        .unwrap();
    refuse_inserts(&server, Some("service_token")).await;
    let app = app!(server);

    let (status, _) = onboard!(app, token, &named("adsb"));
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    // The account was not this call's to delete; the enrolment token it minted
    // before the failure was, and it no longer works.
    assert!(account(&server, "adsb").await.is_some());
    let held = credentials(&server, &user).await;
    assert_eq!(held.len(), 1);
    assert!(
        held[0].revoked_at.is_some(),
        "the orphaned enrolment token was revoked"
    );
}

// ---------------------------------------------------------------------------
// A sidecar deployed with exactly what the action returned.
// ---------------------------------------------------------------------------

/// The plugin, reduced to "tell the test when the stream came up".
struct Watcher {
    seen: mpsc::UnboundedSender<()>,
}

#[async_trait]
impl Sidecar for Watcher {
    const NAME: &'static str = "rustak-plugin-example";
    const VERSION: &'static str = "0.0.0-onboarded";
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

/// Binds the public route tree over the harness's context, on plain HTTP, as
/// `sidecar_enrolment` does and for the same reasons.
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
    rustak_server::testing::await_serving(address).await;

    (format!("http://{address}"), handle)
}

#[actix_web::test]
async fn a_sidecar_enrols_and_registers_with_exactly_what_the_action_returned() {
    rustak_core::identity::password::use_testing_params();

    let harness = Harness::start_with(|config| {
        config.auth.anon_group_default = true;
    })
    .await;
    let _ = harness.context.install_live(Arc::new(harness.live.clone()));
    let (base, api_handle) = api(&harness).await;

    // An administrator, and the one action — over HTTP, as the console makes it.
    let admin = harness
        .context
        .db()
        .users()
        .create(NewUser {
            is_admin: true,
            ..NewUser::person(Username::parse("ada").unwrap())
        })
        .await
        .unwrap();
    let session = session_for(&harness.context, &admin, true).await;
    let onboarding: ServiceOnboarding = reqwest::Client::new()
        .post(format!("{base}/api/v1/service-onboarding"))
        .header("authorization", bearer(&session))
        .json(&ServiceOnboardingRequest {
            account: Some("svc.onboarded".to_string()),
            ..named("onboarded")
        })
        .send()
        .await
        .expect("the action answers")
        .error_for_status()
        .expect("the action succeeds")
        .json()
        .await
        .expect("the response parses");

    // The deployment: the fragment verbatim, plus where the server is — which
    // the operator already knows and the action does not pretend to.
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("plugin.toml");
    let env = directory.path().join(".env");
    std::fs::write(
        &config,
        format!(
            "{}\n[server]\nstream = \"ssl://{}\"\ncontrol = \"{base}\"\n\n[sidecar]\ntick = \"1s\"\n",
            onboarding.config_fragment, harness.addr,
        ),
    )
    .unwrap();
    std::fs::write(&env, &onboarding.environment).unwrap();
    rustak_core::config::load_env_file(&env).expect("the environment lines load");

    let args = Args {
        config: config.clone(),
        env: env.clone(),
        check: false,
        enroll: false,
    };
    let (seen_tx, mut seen) = mpsc::unbounded_channel();
    let shutdown = harness.context.shutdown().child();
    let running = shutdown.clone();
    let sidecar =
        tokio::spawn(async move { serve(Watcher { seen: seen_tx }, &args, running).await });

    tokio::time::timeout(EXPECT, seen.recv())
        .await
        .expect("the sidecar enrols and connects to the stream")
        .expect("the harness reports the connection");
    assert!(
        directory.path().join("onboarded.pem").exists(),
        "the certificate was written"
    );

    // And it reached the control API with the service token it was given.
    let db = harness.context.db();
    let name = onboarding.name.clone();
    let deadline = tokio::time::Instant::now() + EXPECT;
    let registered = loop {
        if let Some(row) = db.services().get_by_name(&name).await.unwrap() {
            break row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the sidecar never registered"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let account = db
        .users()
        .get_by_username(&onboarding.account)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        registered.user_id, account.id,
        "registered as the onboarded account"
    );

    shutdown.cancel();
    sidecar
        .await
        .expect("the sidecar task joins")
        .expect("the sidecar stops without an error");

    harness.stop().await;
    api_handle.stop(true).await;
}
