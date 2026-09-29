//! `/api/v1/auth/lockouts`: what the sign-in rate limiter is refusing, and
//! forgiving one key of it.
//!
//! Administrative throughout. A lockout names an address and, for an account,
//! a username, which is operational data about somebody else's failures.
//!
//! # Reading it cannot be locked out by what it reports
//!
//! These routes sit behind the bearer gate, which is not rate limited — the
//! limiter guards the endpoints that *accept a secret*, and an access token is
//! checked by signature, not guessed at. So an administrator already signed in
//! can always read the list, even from an address that is itself locked out of
//! the passkey ceremony. Reading neither sweeps nor touches a bucket
//! (`auth::ratelimit::view`), and it logs nothing above `debug`: the list is
//! addresses and usernames, and a log line per page load would copy them into
//! every log store for no reason.
//!
//! # Clearing is one key, on purpose
//!
//! There is no "clear all". A lockout is the limiter doing its job, and the
//! administrator who clears one is vouching for that one caller; the audit
//! trail records who did it, which key and when.

use actix_web::web;
use rustak_api::{
    AuditCategory, AuditOutcome, ClearLockoutRequest, Lockout, LockoutClass, MAX_LISTED_LOCKOUTS,
};

use crate::auth::ratelimit::subject_for;
use crate::db::AuditEntry;
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;

/// Registers the lockout routes.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/auth/lockouts", web::get().to(list))
        .route("/auth/lockouts/clear", web::post().to(clear));
}

/// `GET /api/v1/auth/lockouts` — every key locked out now, newest first.
///
/// Bounded at [`MAX_LISTED_LOCKOUTS`]; `total` says how many there are.
///
/// # Errors
///
/// A `403` for anybody but an administrator.
pub async fn list(context: web::Data<AppContext>, _: Administrative) -> ApiResult {
    let lockouts = context.rate_limiter().lockouts(MAX_LISTED_LOCKOUTS);

    debug!(
        total = lockouts.total,
        "An administrator read the sign-in lockouts."
    );

    Ok(json_ok(&lockouts))
}

/// `POST /api/v1/auth/lockouts/clear` — forgives one key, and says which.
///
/// # Errors
///
/// A `403` for anybody but an administrator, a `400` for a key its class could
/// never have produced, and a `404` when nothing is locked out under it —
/// including a lockout that ran out between the page loading and the click.
pub async fn clear(
    context: web::Data<AppContext>,
    body: web::Json<ClearLockoutRequest>,
    caller: Administrative,
) -> ApiResult {
    let request = body.into_inner();

    let Some(subject) = subject_for(request.class, &request.key) else {
        return Err(ApiError::bad_request(
            "That key is not one the rate limiter files under that class.",
        ));
    };

    let Some(cleared) = context.rate_limiter().clear(request.address, &subject) else {
        return Err(ApiError::not_found(
            "Nothing is locked out under that key. It may have run out on its own.",
        ));
    };

    // The class and who asked, but not the key: the audit entry below carries
    // that, where only an administrator can read it.
    info!(
        class = cleared.class.as_str(),
        actor = %caller.user.username,
        "Cleared a sign-in lockout at an administrator's request."
    );

    record(&context, &caller, &cleared).await;

    Ok(json_ok(&cleared))
}

/// Writes who cleared which lockout.
///
/// The subject is what an administrator would search the log for: the account
/// or client for those classes, and the address for an endpoint-wide lockout,
/// whose key (`passkey`, `setup-token`) says nothing about who it was.
async fn record(context: &AppContext, caller: &Administrative, cleared: &Lockout) {
    let address = cleared.address.map(|address| address.to_string());
    let subject = match cleared.class {
        LockoutClass::Address => address.clone().unwrap_or_else(|| cleared.key.clone()),
        LockoutClass::Account | LockoutClass::Client => cleared.key.clone(),
    };

    let entry = AuditEntry::new(
        AuditCategory::Administration,
        "lockout.cleared",
        AuditOutcome::Success,
    )
    .subject(subject)
    .actor(&caller.user.username)
    .detail(serde_json::json!({
        "class": cleared.class.as_str(),
        "key": cleared.key,
        "address": address,
        "failures": cleared.failures,
        "started_at": cleared.started_at,
        "ends_at": cleared.ends_at,
    }));

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a cleared lockout in the audit log.");
        context.session().record_human_error(&err);
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::Lockouts;

    use super::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    fn address() -> Option<std::net::IpAddr> {
        Some("198.51.100.4".parse().unwrap())
    }

    /// Locks `subject` out from [`address`], as ten failures would.
    fn lock_out(server: &TestServer, subject: &str) {
        let attempts = server.config().auth.rate_limit.attempts;

        for _ in 0..attempts {
            server.limiter.record_failure(address(), subject);
        }
    }

    #[actix_web::test]
    async fn an_administrator_is_shown_what_is_locked_out() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("root", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        lock_out(&server, "ada");
        lock_out(&server, crate::auth::ratelimit::subjects::PASSKEY);

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/auth/lockouts")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json"
        );

        let body: serde_json::Value = test::read_body_json(response).await;

        assert_eq!(body["total"], 2);
        assert_eq!(body["counters"].as_array().unwrap().len(), 3);
        assert!(body["counting_since"].is_string());

        let ada = body["lockouts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|lockout| lockout["key"] == "ada")
            .expect("ada's lockout is listed");

        assert_eq!(ada["class"], "account");
        assert_eq!(ada["address"], "198.51.100.4");
        assert_eq!(ada["failures"], server.config().auth.rate_limit.attempts);
        assert!(ada["started_at"].is_string());
        assert!(ada["ends_at"].is_string());

        let typed: Lockouts = serde_json::from_value(body).unwrap();
        assert!(
            typed
                .lockouts
                .iter()
                .any(|lockout| lockout.class == LockoutClass::Address && lockout.key == "passkey")
        );
    }

    #[actix_web::test]
    async fn somebody_who_is_not_an_administrator_is_refused_both_routes() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        lock_out(&server, "grace");

        let read = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/auth/lockouts")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;
        let clear = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/lockouts/clear")
                .insert_header(("authorization", bearer(&session)))
                .set_json(ClearLockoutRequest {
                    class: LockoutClass::Account,
                    address: address(),
                    key: "grace".to_string(),
                })
                .to_request(),
        )
        .await;

        assert_eq!(read.status(), StatusCode::FORBIDDEN);
        assert_eq!(clear.status(), StatusCode::FORBIDDEN);
        assert!(
            server.limiter.check(address(), "grace").is_err(),
            "a refused clear forgave nothing",
        );
    }

    #[actix_web::test]
    async fn clearing_forgives_one_key_and_says_who_did_it() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("root", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        lock_out(&server, "ada");
        lock_out(&server, "grace");

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/lockouts/clear")
                .insert_header(("authorization", bearer(&session)))
                .set_json(ClearLockoutRequest {
                    class: LockoutClass::Account,
                    address: address(),
                    key: "ada".to_string(),
                })
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let cleared: Lockout = test::read_body_json(response).await;
        assert_eq!(cleared.key, "ada");

        assert!(server.limiter.check(address(), "ada").is_ok());
        assert!(server.limiter.check(address(), "grace").is_err());

        let entries = server
            .db()
            .audit(crate::db::AuditQuery::about("ada", 10))
            .await
            .unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.action == "lockout.cleared")
            .expect("the clear is in the audit log");

        assert_eq!(entry.actor.as_deref(), Some("root"));
        assert_eq!(entry.detail.as_ref().unwrap()["class"], "account");
        assert_eq!(entry.detail.as_ref().unwrap()["address"], "198.51.100.4");
    }

    #[actix_web::test]
    async fn a_key_that_is_not_locked_out_or_not_of_that_class_is_refused() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("root", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        lock_out(&server, crate::auth::ratelimit::subjects::PASSKEY);

        let clear = |class, key: &str| {
            test::TestRequest::post()
                .uri("/api/v1/auth/lockouts/clear")
                .insert_header(("authorization", bearer(&session)))
                .set_json(ClearLockoutRequest {
                    class,
                    address: address(),
                    key: key.to_string(),
                })
                .to_request()
        };

        let nobody = test::call_service(&app, clear(LockoutClass::Account, "nobody")).await;
        assert_eq!(nobody.status(), StatusCode::NOT_FOUND);

        // An account called `passkey` would otherwise forgive every passkey
        // ceremony from that address.
        let disguised = test::call_service(&app, clear(LockoutClass::Account, "passkey")).await;
        assert_eq!(disguised.status(), StatusCode::BAD_REQUEST);
        assert!(
            server
                .limiter
                .check(address(), crate::auth::ratelimit::subjects::PASSKEY)
                .is_err()
        );
    }
}
