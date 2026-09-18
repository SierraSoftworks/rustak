//! `/api/v1/users/{username}/groups`: who is in which channel.
//!
//! Administrative, because a membership is a routing right rather than a
//! preference — a person granting themselves a channel would be granting
//! themselves whatever is said on it.
//!
//! # A replacement, not a patch
//!
//! `PUT` sends the whole set an administrator wants, which is what the UI
//! renders and what makes the operation idempotent. Only the grants an
//! administrator made are replaced: the ones mapped from a `groups` claim are
//! rewritten wholesale at each sign-in, so editing one here would last until
//! the member next signed in and no longer.

use actix_web::web;
use rustak_api::{AuditCategory, AuditOutcome, GroupMembership, MembershipSource};

use crate::db::AuditEntry;
use crate::identity::members;
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;
use super::subject::failed;

/// `GET /api/v1/users/{username}/groups`.
///
/// # Errors
///
/// A `404` when there is no such account, and a `500` when a read fails.
pub async fn get(
    context: web::Data<AppContext>,
    username: web::Path<String>,
    _: Administrative,
) -> ApiResult {
    let user = load(&context, &username).await?;

    let held = members::grants_for_user(context.db(), user.id)
        .await
        .map_err(|err| failed(&context, &err))?;

    Ok(json_ok(&held))
}

/// `PUT /api/v1/users/{username}/groups`.
///
/// # Errors
///
/// A `400` when a channel named is not here or the request tries to set a
/// membership the identity provider owns, a `404` when there is no such
/// account, and a `500` when a read or write fails.
pub async fn put(
    context: web::Data<AppContext>,
    username: web::Path<String>,
    body: web::Json<Vec<GroupMembership>>,
    caller: Administrative,
) -> ApiResult {
    let user = load(&context, &username).await?;
    let wanted = body.into_inner();

    if let Some(claimed) = wanted
        .iter()
        .find(|grant| grant.source == Some(MembershipSource::Oidc))
    {
        return Err(ApiError::bad_request(format!(
            "'{}' is granted by your identity provider, so setting it here would not last.",
            claimed.group
        )));
    }

    let held = members::replace_manual(
        context.db(),
        user.id,
        &wanted,
        context.config().auth.anon_group_default,
    )
    .await
    .map_err(|err| failed(&context, &err))?;

    record(&context, &caller, &user.username, &held).await;

    // The forced case (`compat/groups.md` §3): a membership an administrator
    // changed is nobody's own action, so every one of the account's devices is
    // told and none is excluded.
    members::channels_changed(&context, user.id, &user.username, None).await;

    Ok(json_ok(&held))
}

/// Reads the account a path segment names.
async fn load(context: &AppContext, username: &str) -> Result<crate::db::repos::UserRow, ApiError> {
    let username = Username::parse(username)
        .map_err(|err| ApiError::bad_request(format!("That is not a username: {err}")))?;

    context
        .db()
        .users()
        .get_by_username(&username)
        .await
        .map_err(|err| failed(context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no account by that name."))
}

/// Writes what the account now holds and who decided it.
async fn record(
    context: &AppContext,
    caller: &Administrative,
    username: &Username,
    held: &[GroupMembership],
) {
    let granted: Vec<String> = held
        .iter()
        .map(|grant| format!("{}:{}", grant.group, grant.direction.as_str()))
        .collect();

    let entry = AuditEntry::new(
        AuditCategory::Administration,
        "user.channels-changed",
        AuditOutcome::Success,
    )
    .subject(username)
    .actor(&caller.user.username)
    .detail(serde_json::json!({ "groups": granted }));

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a channel change in the audit log.");
        context.session().record_human_error(&err);
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::{Direction, GroupName};

    use super::*;
    use crate::db::repos::NewGroup;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    async fn channel(server: &TestServer, name: &str) {
        server
            .db()
            .groups()
            .create(NewGroup::manual(GroupName::parse(name).unwrap()))
            .await
            .unwrap();
    }

    #[actix_web::test]
    async fn a_replacement_is_what_the_account_then_holds() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        server.user("grace", false).await;
        channel(&server, "Blue").await;
        channel(&server, "Red").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let held: Vec<GroupMembership> = test::call_and_read_body_json(
            &app,
            test::TestRequest::put()
                .uri("/api/v1/users/grace/groups")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!([
                    { "group": "Blue", "direction": "BOTH" },
                    { "group": "Red", "direction": "OUT" },
                ]))
                .to_request(),
        )
        .await;

        let named: Vec<(&str, Direction)> = held
            .iter()
            .map(|grant| (grant.group.as_str(), grant.direction))
            .collect();

        assert!(named.contains(&("Blue", Direction::In)));
        assert!(named.contains(&("Blue", Direction::Out)));
        assert!(named.contains(&("Red", Direction::Out)));
        assert!(!named.contains(&("Red", Direction::In)));
        assert!(
            named.contains(&("__ANON__", Direction::In)),
            "the default channel is the installation's decision, not this call's",
        );

        let read_back: Vec<GroupMembership> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/users/grace/groups")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(read_back, held);
    }

    #[actix_web::test]
    async fn a_replacement_removes_what_is_no_longer_listed() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        server.user("grace", false).await;
        channel(&server, "Blue").await;

        let app = test::init_service(App::new().configure(server.app())).await;
        let set = |body: serde_json::Value| {
            test::TestRequest::put()
                .uri("/api/v1/users/grace/groups")
                .insert_header(("authorization", bearer(&session)))
                .set_json(body)
                .to_request()
        };

        test::call_service(
            &app,
            set(serde_json::json!([
                { "group": "Blue", "direction": "BOTH" },
            ])),
        )
        .await;

        let held: Vec<GroupMembership> =
            test::call_and_read_body_json(&app, set(serde_json::json!([]))).await;

        assert!(
            !held.iter().any(|grant| grant.group.as_str() == "Blue"),
            "{held:?}",
        );
    }

    #[actix_web::test]
    async fn a_channel_that_is_not_here_is_named_in_the_refusal() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        server.user("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::put()
                .uri("/api/v1/users/grace/groups")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!([
                    { "group": "Nowhere", "direction": "IN" },
                ]))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body: serde_json::Value = test::read_body_json(response).await;
        assert!(
            body["error"].as_str().unwrap().contains("Nowhere"),
            "{body}",
        );
    }

    #[actix_web::test]
    async fn a_grant_the_identity_provider_owns_is_refused_rather_than_silently_lost() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        server.user("grace", false).await;
        channel(&server, "Blue").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::put()
                .uri("/api/v1/users/grace/groups")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!([
                    { "group": "Blue", "direction": "IN", "source": "oidc" },
                ]))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn the_change_is_recorded_with_what_the_account_now_holds() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        server.user("grace", false).await;
        channel(&server, "Blue").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        test::call_service(
            &app,
            test::TestRequest::put()
                .uri("/api/v1/users/grace/groups")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!([
                    { "group": "Blue", "direction": "IN" },
                ]))
                .to_request(),
        )
        .await;

        let records = server
            .db()
            .audit(crate::db::AuditQuery::about("grace", 10))
            .await
            .unwrap();

        let entry = records
            .iter()
            .find(|record| record.action == "user.channels-changed")
            .expect("the change should be in the log");

        assert_eq!(entry.actor.as_deref(), Some("ada"));
        assert!(
            entry
                .detail
                .as_ref()
                .unwrap()
                .to_string()
                .contains("Blue:IN"),
            "{entry:?}",
        );
    }

    #[actix_web::test]
    async fn an_ordinary_caller_cannot_grant_themselves_a_channel() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;
        channel(&server, "Blue").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        for request in [
            test::TestRequest::get()
                .uri("/api/v1/users/grace/groups")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
            test::TestRequest::put()
                .uri("/api/v1/users/grace/groups")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!([
                    { "group": "Blue", "direction": "BOTH" },
                ]))
                .to_request(),
        ] {
            assert_eq!(
                test::call_service(&app, request).await.status(),
                StatusCode::FORBIDDEN,
            );
        }
    }

    #[actix_web::test]
    async fn an_account_that_is_not_here_is_a_not_found() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/api/v1/users/nobody/groups")
                    .insert_header(("authorization", bearer(&session)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::NOT_FOUND,
        );
    }
}
