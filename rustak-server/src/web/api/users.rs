//! `GET`/`POST /api/v1/users`, `GET` and `PATCH /api/v1/users/{username}`.
//!
//! Administrative, except that reading one account is also "your own": a page
//! that opens on somebody would otherwise have to read the whole listing and
//! filter it, and could not tell "no such account" from "not yours to see".
//!
//! The patch is the one lever an administrator has
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
//!
//! # Clearing a field, and why the email is the odd one out
//!
//! An empty display name is not a name, so `""` clears it and the difference
//! between "unchanged" and "cleared" never rests on telling an absent JSON
//! field from a null one. An empty *email* is a form posting a box somebody
//! emptied, and storing `""` as an address would be storing a wrong answer —
//! so `email` is a three-way value and `null` is what clears it.

use actix_web::http::StatusCode;
use actix_web::web;
use rustak_api::{AuditCategory, AuditOutcome, CreateUserRequest, User, UserKind, UserPatch};

use crate::db::repos::{NewUser, ProfileChange};
use crate::db::{AuditEntry, repos::Page};
use crate::identity::{groups, users};
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok, json_with};
use super::extract::{Administrative, Authenticated};
use super::subject;

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

/// `GET /api/v1/users/{username}`, for an administrator or the account itself.
///
/// The same [`User`] the listing carries, so a page that opens on one account
/// and a page that lists them all render from one shape.
///
/// # Errors
///
/// A `400` for a name no account could have, a `403` when it is somebody
/// else's and the caller does not administer the installation, a `404` when
/// there is no such account, and a `500` when the read fails.
pub async fn get(
    context: web::Data<AppContext>,
    username: web::Path<String>,
    caller: Authenticated,
) -> ApiResult {
    let username = Username::parse(&username)
        .map_err(|err| ApiError::bad_request(format!("That is not a username: {err}")))?;

    let subject = subject::resolve(&context, &caller, Some(&username)).await?;

    Ok(json_ok(&users::to_dto(&subject.user)))
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
            // Refusing new sign-ins is not enough on its own. A client holding
            // a refresh token would keep minting access tokens, a CoT stream
            // session resolves its principal once at the handshake and would
            // keep sending and receiving, and an open event feed would keep
            // delivering — so disabling has to end all three (R-01 H5).
            let closed = crate::identity::sessions::end_all(context.get_ref(), &user).await;

            info!(
                username = %user.username,
                connections = closed,
                "Switched an account off and ended the sessions it had open."
            );
        }
    }

    if let Some(is_admin) = patch.is_admin {
        db.users()
            .set_admin_override(user.id, Some(is_admin))
            .await
            .map_err(|err| failed(&context, &err))?;
    }

    let profile = profile(&patch);

    if !profile.is_empty() {
        db.users()
            .set_profile(user.id, profile)
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

/// What the patch says about who the account belongs to.
///
/// An empty or whitespace-only value clears the field either way: a display
/// name of spaces is not a name, and an address of spaces is not an address.
/// The email's outer [`Option`] is what separates "unchanged" from "cleared";
/// the display name has no such layer, and `""` is its way of saying the same
/// thing.
fn profile(patch: &UserPatch) -> ProfileChange {
    ProfileChange {
        display_name: patch.display_name.as_deref().map(blank_is_nothing),
        email: patch
            .email
            .as_ref()
            .map(|email| email.as_deref().and_then(blank_is_nothing)),
    }
}

/// A trimmed value, or nothing when there was nothing but space.
fn blank_is_nothing(value: &str) -> Option<String> {
    let trimmed = value.trim();

    (!trimmed.is_empty()).then(|| trimmed.to_owned())
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
        // Named rather than quoted: the log says an address was set or taken
        // away without becoming a second copy of everybody's address.
        "display_name_changed": patch.display_name.is_some(),
        "email_changed": patch.email.is_some(),
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

    #[actix_web::test]
    async fn one_account_is_readable_by_itself_and_by_an_administrator_and_by_nobody_else() {
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", true).await;
        let (_, grace) = server.signed_in("grace", false).await;
        server.user("bhavna", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let own: User = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/users/grace")
                .insert_header(("authorization", bearer(&grace)))
                .to_request(),
        )
        .await;

        assert_eq!(own.username.as_str(), "grace");

        let theirs: User = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/users/grace")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(theirs, own, "one account has one shape whoever reads it");

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/api/v1/users/bhavna")
                    .insert_header(("authorization", bearer(&grace)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::FORBIDDEN,
        );
    }

    #[actix_web::test]
    async fn an_account_that_is_not_here_is_a_not_found_and_a_name_that_could_not_be_is_refused() {
        // The two are different answers to different questions, and only an
        // administrator ever sees the first.
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/api/v1/users/nobody")
                    .insert_header(("authorization", bearer(&ada)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::NOT_FOUND,
        );

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/api/v1/users/%20")
                    .insert_header(("authorization", bearer(&ada)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
        );
    }

    #[actix_web::test]
    async fn the_display_name_and_the_email_are_written_and_can_each_be_cleared() {
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", true).await;
        server.user("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;
        let patch = |body: serde_json::Value| {
            test::TestRequest::patch()
                .uri("/api/v1/users/grace")
                .insert_header(("authorization", bearer(&ada)))
                .set_json(body)
                .to_request()
        };

        let set: User = test::call_and_read_body_json(
            &app,
            patch(serde_json::json!({
                "display_name": "Grace Hopper",
                "email": "grace@example.com",
            })),
        )
        .await;

        assert_eq!(set.display_name.as_deref(), Some("Grace Hopper"));
        assert_eq!(set.email.as_deref(), Some("grace@example.com"));

        // An absent field leaves the other alone; this is the case a UI that
        // patches one switch at a time relies on.
        let narrowed: User =
            test::call_and_read_body_json(&app, patch(serde_json::json!({ "disabled": false })))
                .await;

        assert_eq!(narrowed.email.as_deref(), Some("grace@example.com"));

        let cleared: User =
            test::call_and_read_body_json(&app, patch(serde_json::json!({ "email": null }))).await;

        assert_eq!(cleared.email, None);
        assert_eq!(cleared.display_name.as_deref(), Some("Grace Hopper"));

        let anonymous: User =
            test::call_and_read_body_json(&app, patch(serde_json::json!({ "display_name": "  " })))
                .await;

        assert_eq!(anonymous.display_name, None);
        assert_eq!(anonymous.display(), "grace");

        assert!(
            server
                .db()
                .audit(crate::db::AuditQuery::about("grace", 10))
                .await
                .unwrap()
                .iter()
                .any(|record| record
                    .detail
                    .as_ref()
                    .is_some_and(|detail| detail.get("email_changed")
                        == Some(&serde_json::Value::Bool(true)))),
            "the log says an address was changed without becoming a copy of it",
        );
    }

    #[actix_web::test]
    async fn an_email_alone_is_a_change_worth_making() {
        // `is_empty` decides whether a patch is refused as doing nothing, so a
        // patch carrying only the new field has to count.
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::patch()
                    .uri("/api/v1/users/ada")
                    .insert_header(("authorization", bearer(&ada)))
                    .set_json(serde_json::json!({ "email": "ada@example.com" }))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::OK,
        );
    }
}
