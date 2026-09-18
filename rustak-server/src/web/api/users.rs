//! `GET /api/v1/users` and `PATCH /api/v1/users/{username}`.
//!
//! Both administrative. The patch is the one lever an administrator has that
//! does not need the configuration file editing: it can grant or refuse
//! administrative access regardless of what the access-control expression says,
//! and it can switch an account off.
//!
//! # Why disabling revokes rather than only refusing
//!
//! An account that has been switched off must stop working now, not when its
//! access token expires. The bearer middleware reads the account on every
//! request, which covers the access token; the refresh tokens are revoked here
//! so that a client holding one cannot mint a fresh access token from it.

use actix_web::web;
use rustak_api::{AuditCategory, AuditOutcome, User, UserPatch};

use crate::db::{AuditEntry, repos::Page};
use crate::identity::users;
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;

/// How many accounts one page carries.
const PAGE_SIZE: u32 = 500;

/// Lists the accounts this installation knows about.
///
/// # Errors
///
/// A `500` when the read fails.
pub async fn list(context: web::Data<AppContext>, _: Administrative) -> ApiResult {
    let rows = context
        .db()
        .users()
        .list(Page::first(PAGE_SIZE))
        .await
        .map_err(|err| failed(&context, &err))?;

    let users: Vec<User> = rows.iter().map(users::to_dto).collect();

    Ok(json_ok(&users))
}

/// Changes one account.
///
/// # Errors
///
/// A `400` when the patch would change nothing or would leave the installation
/// with no administrator, a `404` when there is no such account, and a `500`
/// when a read or write fails.
pub async fn patch(
    context: web::Data<AppContext>,
    username: web::Path<String>,
    patch: web::Json<UserPatch>,
    caller: Administrative,
) -> ApiResult {
    if patch.is_empty() {
        return Err(ApiError::bad_request("That change would do nothing."));
    }

    let db = context.db();
    let username = Username::from_storage(username.into_inner());

    let user = db
        .users()
        .get_by_username(&username)
        .await
        .map_err(|err| failed(&context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no account by that name."))?;

    refuse_self_lockout(&caller, &user, &patch)?;

    if let Some(disabled) = patch.disabled {
        db.users()
            .set_disabled(user.id, disabled)
            .await
            .map_err(|err| failed(&context, &err))?;

        if disabled {
            // Refusing new sign-ins is not enough on its own: a client holding
            // a refresh token would otherwise keep minting access tokens.
            db.refresh_tokens()
                .revoke_all_for_user(user.id)
                .await
                .map_err(|err| failed(&context, &err))?;
        }
    }

    if let Some(is_admin) = patch.is_admin {
        db.users()
            .set_admin_override(user.id, Some(is_admin))
            .await
            .map_err(|err| failed(&context, &err))?;
    }

    let updated = db
        .users()
        .get(user.id)
        .await
        .map_err(|err| failed(&context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no account by that name."))?;

    record(&context, &caller, &updated, &patch).await;

    Ok(json_ok(&users::to_dto(&updated)))
}

/// Refuses the change that would leave nobody able to undo it.
///
/// An administrator disabling or demoting themselves is the one mistake here
/// that cannot be corrected from inside the UI, and on an installation with a
/// single administrator it means reaching for the database by hand.
fn refuse_self_lockout(
    caller: &Administrative,
    user: &crate::db::repos::UserRow,
    patch: &UserPatch,
) -> Result<(), ApiError> {
    if caller.user.id != user.id {
        return Ok(());
    }

    if patch.disabled == Some(true) {
        return Err(ApiError::bad_request(
            "You cannot switch off the account you are signed in with.",
        ));
    }

    if patch.is_admin == Some(false) {
        return Err(ApiError::bad_request(
            "You cannot remove your own administrative access.",
        ));
    }

    Ok(())
}

/// Writes what was changed and who changed it.
async fn record(
    context: &AppContext,
    caller: &Administrative,
    user: &crate::db::repos::UserRow,
    patch: &UserPatch,
) {
    let entry = AuditEntry::new(
        AuditCategory::Administration,
        "user.updated",
        AuditOutcome::Success,
    )
    .subject(&user.username)
    .actor(&caller.user.username)
    .detail(serde_json::json!({
        "disabled": patch.disabled,
        "is_admin": patch.is_admin,
    }));

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record an administrative change in the audit log.");
        context.session().record_human_error(&err);
    }
}

/// Reports one of our own failures and generalises it.
fn failed(context: &AppContext, err: &Error) -> ApiError {
    context.session().record_human_error(err);

    ApiError::from_human(err)
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};

    use super::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    #[actix_web::test]
    async fn an_administrator_sees_every_account() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        server.user("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let users: Vec<User> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/users")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(users.len(), 2);
        assert!(users.iter().any(|user| user.username.as_str() == "grace"));
    }

    #[actix_web::test]
    async fn disabling_an_account_stops_its_refresh_tokens_working() {
        let server = TestServer::start().await;
        let (_, admin) = server.signed_in("ada", true).await;
        let (grace, grace_session) = server.signed_in("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::patch()
                .uri("/api/v1/users/grace")
                .insert_header(("authorization", bearer(&admin)))
                .set_json(serde_json::json!({ "disabled": true }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);

        let updated: User = test::read_body_json(response).await;
        assert!(updated.disabled);

        assert!(
            crate::auth::tokens::rotate(
                &server.context,
                &grace_session.refresh_token.unwrap(),
                None
            )
            .await
            .is_err(),
            "a disabled account must not be able to mint a fresh token",
        );

        assert!(
            server
                .db()
                .users()
                .get(grace.id)
                .await
                .unwrap()
                .unwrap()
                .disabled
        );
    }

    #[actix_web::test]
    async fn an_administrator_cannot_lock_themselves_out() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        for patch in [
            serde_json::json!({ "disabled": true }),
            serde_json::json!({ "is_admin": false }),
        ] {
            let response = test::call_service(
                &app,
                test::TestRequest::patch()
                    .uri("/api/v1/users/ada")
                    .insert_header(("authorization", bearer(&session)))
                    .set_json(&patch)
                    .to_request(),
            )
            .await;

            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{patch} should not be something an administrator can do to themselves",
            );
        }
    }

    #[actix_web::test]
    async fn an_account_that_is_not_here_is_a_not_found() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::patch()
                .uri("/api/v1/users/nobody")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({ "disabled": true }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn granting_administrative_access_is_recorded() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        server.user("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let updated: User = test::call_and_read_body_json(
            &app,
            test::TestRequest::patch()
                .uri("/api/v1/users/grace")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({ "is_admin": true }))
                .to_request(),
        )
        .await;

        assert!(updated.is_admin);
        assert_eq!(updated.admin_override, Some(true));

        let records = server
            .db()
            .audit(crate::db::AuditQuery::about("grace", 10))
            .await
            .unwrap();

        assert!(
            records
                .iter()
                .any(|record| record.action == "user.updated"
                    && record.actor.as_deref() == Some("ada")),
            "who made the change has to be in the log as well as what it was",
        );
    }

    #[actix_web::test]
    async fn a_patch_that_would_change_nothing_is_refused() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::patch()
                .uri("/api/v1/users/ada")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({}))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
