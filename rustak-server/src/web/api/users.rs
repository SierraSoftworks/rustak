//! `GET`/`POST /api/v1/users` and `PATCH /api/v1/users/{username}`.
//!
//! All three administrative. The patch is the one lever an administrator has
//! that does not need the configuration file editing: it can grant or refuse
//! administrative access regardless of what the access-control expression says,
//! and it can switch an account off.
//!
//! # Creating an account hands out nothing
//!
//! [`create`] makes a row and stops. rustak has no local passwords, so there is
//! no secret to return and nothing to leak: the new account signs in with a
//! passkey it registers itself, arrives again through the identity provider, or
//! — for a service or an EUD — is given a credential through
//! `POST /api/v1/credentials`, which is the one endpoint that ever emits one.
//! Without this endpoint the only ways an account can come into existence are
//! the first-run wizard and an OIDC sign-in, which leaves an installation with
//! no identity provider unable to add anybody.
//!
//! # Why disabling revokes rather than only refusing
//!
//! An account that has been switched off must stop working now, not when its
//! access token expires. The bearer middleware reads the account on every
//! request, which covers the access token; the refresh tokens are revoked here
//! so that a client holding one cannot mint a fresh access token from it.

use actix_web::http::StatusCode;
use actix_web::web;
use rustak_api::{AuditCategory, AuditOutcome, CreateUserRequest, User, UserKind, UserPatch};

use crate::db::repos::NewUser;
use crate::db::{AuditEntry, repos::Page};
use crate::identity::{groups, users};
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok, json_with};
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

/// Creates an account.
///
/// # Errors
///
/// A `409` when the name is taken — checked first and reported again from the
/// write, because two administrators can ask at the same moment — and a `500`
/// when a read or write fails.
pub async fn create(
    context: web::Data<AppContext>,
    body: web::Json<CreateUserRequest>,
    caller: Administrative,
) -> ApiResult {
    let db = context.db();
    let request = body.into_inner();

    if db
        .users()
        .get_by_username(&request.username)
        .await
        .map_err(|err| failed(&context, &err))?
        .is_some()
    {
        return Err(taken(&request.username));
    }

    let new = NewUser {
        display_name: request.display_name.clone(),
        email: request.email.clone(),
        ..match request.kind {
            UserKind::Person => NewUser::person(request.username.clone()),
            UserKind::Service => NewUser::service(request.username.clone()),
        }
    };

    let created = match db.users().create(new).await {
        Ok(created) => created,
        // The unique index is the real guard; the read above only turns the
        // common case into a message an administrator can act on.
        Err(err) if err.description().contains("UNIQUE") => return Err(taken(&request.username)),
        Err(err) => return Err(failed(&context, &err)),
    };

    // Everybody starts in the default channel, for the same reason the wizard's
    // first administrator does: an account in no channel can neither send nor
    // receive, and an administrator who has to remember a second step will not.
    if context.config().auth.anon_group_default
        && let Err(err) = groups::join_default(db, created.id).await
    {
        warn!(error = %err, "Could not put a new account in the default channel.");
        context.session().record_human_error(&err);
    }

    created_record(&context, &caller, &created).await;

    Ok(json_with(StatusCode::CREATED, &users::to_dto(&created)))
}

/// What a name somebody already holds is answered with.
fn taken(username: &Username) -> ApiError {
    ApiError::conflict(format!("There is already an account called {username}."))
}

/// Writes the creation to the audit log.
async fn created_record(
    context: &AppContext,
    caller: &Administrative,
    user: &crate::db::repos::UserRow,
) {
    let entry = AuditEntry::new(
        AuditCategory::Administration,
        "user.created",
        AuditOutcome::Success,
    )
    .subject(&user.username)
    .actor(&caller.user.username)
    .message(format!(
        "{} created the account {}.",
        caller.user.username, user.username
    ))
    .detail(serde_json::json!({ "kind": user.kind.as_str() }));

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a new account in the audit log.");
        context.session().record_human_error(&err);
    }
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
    async fn an_administrator_can_add_an_account_and_it_carries_no_secret() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/users")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({
                    "username": "grace",
                    "display_name": "Grace Hopper",
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::CREATED);

        let body: serde_json::Value = test::read_body_json(response).await;

        assert_eq!(body["username"], "grace");
        assert_eq!(body["kind"], "person");
        assert_eq!(body["is_admin"], false);
        assert!(
            body.get("secret").is_none() && body.get("password").is_none(),
            "creating an account must not hand anybody a credential: {body}",
        );

        assert!(
            server
                .db()
                .users()
                .get_by_username(&Username::parse("grace").unwrap())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[actix_web::test]
    async fn a_service_account_is_created_as_one() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let created: User = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/users")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({ "username": "etl", "kind": "service" }))
                .to_request(),
        )
        .await;

        assert_eq!(created.kind, rustak_api::UserKind::Service);
        assert_eq!(created.source, rustak_api::UserSource::Service);
    }

    #[actix_web::test]
    async fn a_name_that_is_already_taken_is_a_conflict() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/users")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({ "username": "ada" }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[actix_web::test]
    async fn an_ordinary_account_cannot_create_one() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/users")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({ "username": "grace" }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn creating_an_account_is_recorded() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/users")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({ "username": "grace" }))
                .to_request(),
        )
        .await;

        let records = server
            .db()
            .audit(crate::db::AuditQuery::about("grace", 10))
            .await
            .unwrap();

        assert!(records.iter().any(
            |record| record.action == "user.created" && record.actor.as_deref() == Some("ada")
        ),);
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
