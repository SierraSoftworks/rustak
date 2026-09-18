//! `/api/v1/groups`: the channels an installation has.
//!
//! Administrative throughout. A channel is not a document somebody owns — it is
//! a routing decision that applies to everybody who holds it, so creating and
//! removing them is an operator's job and reading the list is
//! `GET /api/v1/me`'s.
//!
//! # What a channel cannot do here
//!
//! It cannot be renamed: every membership, every `groups` claim and every
//! client's cached selection refers to a channel by name, so renaming one is
//! deleting it and making another. And deleting one is soft — its bit position
//! stays reserved, because a live subscription holds bit positions rather than
//! names and reusing one would hand this channel's traffic to the next channel
//! created.

use actix_web::{HttpResponse, web};
use rustak_api::{AuditCategory, AuditOutcome, CreateGroupRequest, Group, GroupName, GroupPatch};

use crate::db::AuditEntry;
use crate::identity::groups;
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;
use super::subject::failed;

/// `GET /api/v1/groups`.
///
/// # Errors
///
/// A `500` when the read fails.
pub async fn list(context: web::Data<AppContext>, _: Administrative) -> ApiResult {
    let rows = groups::list(context.db())
        .await
        .map_err(|err| failed(&context, &err))?;

    let listed: Vec<Group> = rows.iter().map(groups::to_dto).collect();

    Ok(json_ok(&listed))
}

/// `POST /api/v1/groups`.
///
/// # Errors
///
/// A `400` when the name is already taken or every bit position is in use, and
/// a `500` when a read or write fails.
pub async fn create(
    context: web::Data<AppContext>,
    body: web::Json<CreateGroupRequest>,
    caller: Administrative,
) -> ApiResult {
    let request = body.into_inner();

    let created = groups::create(context.db(), &request)
        .await
        .map_err(|err| failed(&context, &err))?;

    record(
        &context,
        "group.created",
        &caller,
        &created.name,
        serde_json::json!({ "bitpos": created.bitpos }),
    )
    .await;

    Ok(json_ok(&groups::to_dto(&created)))
}

/// `PATCH /api/v1/groups/{name}`.
///
/// # Errors
///
/// A `400` when the patch would change nothing or the name is not one we could
/// have stored, a `404` when the channel is not here, and a `500` when a read
/// or write fails.
pub async fn patch(
    context: web::Data<AppContext>,
    name: web::Path<String>,
    body: web::Json<GroupPatch>,
    caller: Administrative,
) -> ApiResult {
    let change = body.into_inner();

    if change.is_empty() {
        return Err(ApiError::bad_request("That change would do nothing."));
    }

    let name = parse(&name)?;

    let updated = groups::patch(context.db(), &name, &change)
        .await
        .map_err(|err| failed(&context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no channel by that name."))?;

    record(
        &context,
        "group.updated",
        &caller,
        &updated.name,
        serde_json::json!({ "description": updated.description }),
    )
    .await;

    Ok(json_ok(&groups::to_dto(&updated)))
}

/// `DELETE /api/v1/groups/{name}`.
///
/// # Errors
///
/// A `400` when asked to delete the default channel, a `404` when the channel
/// is not here, and a `500` when a read or write fails.
pub async fn remove(
    context: web::Data<AppContext>,
    name: web::Path<String>,
    caller: Administrative,
) -> ApiResult {
    let name = parse(&name)?;

    let deleted = groups::delete(context.db(), &name)
        .await
        .map_err(|err| failed(&context, &err))?;

    if !deleted {
        return Err(ApiError::not_found("There is no channel by that name."));
    }

    record(
        &context,
        "group.deleted",
        &caller,
        &name,
        serde_json::json!({}),
    )
    .await;

    Ok(HttpResponse::NoContent().finish())
}

/// Reads a channel name out of a path segment.
///
/// A name we could never have stored is a `400` rather than a `404`: it is not
/// that the channel is missing, it is that it could not exist.
fn parse(raw: &str) -> Result<GroupName, ApiError> {
    GroupName::parse(raw)
        .map_err(|err| ApiError::bad_request(format!("That is not a channel name: {err}")))
}

/// Writes what was changed and who changed it.
async fn record(
    context: &AppContext,
    action: &'static str,
    caller: &Administrative,
    name: &GroupName,
    detail: serde_json::Value,
) {
    let entry = AuditEntry::new(AuditCategory::Administration, action, AuditOutcome::Success)
        .subject(name)
        .actor(&caller.user.username)
        .detail(detail);

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a channel change in the audit log.");
        context.session().record_human_error(&err);
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::GroupSource;

    use super::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    #[actix_web::test]
    async fn a_channel_is_created_with_a_bit_position_the_caller_did_not_choose() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let created: Group = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/groups")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({
                    "name": "Blue",
                    "description": "The blue team",
                    "bitpos": 99,
                }))
                .to_request(),
        )
        .await;

        assert_eq!(created.name.as_str(), "Blue");
        assert_ne!(created.bitpos, 99, "the bit position is ours to allocate");
        assert_eq!(created.source, GroupSource::Manual);

        let listed: Vec<Group> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/groups")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert!(listed.iter().any(|group| group.name.is_anon()));
        assert!(listed.iter().any(|group| group.name.as_str() == "Blue"));
    }

    #[actix_web::test]
    async fn a_channel_that_is_already_here_is_refused() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;
        let request = || {
            test::TestRequest::post()
                .uri("/api/v1/groups")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({ "name": "Blue" }))
                .to_request()
        };

        assert_eq!(
            test::call_service(&app, request()).await.status(),
            StatusCode::OK,
        );
        assert_eq!(
            test::call_service(&app, request()).await.status(),
            StatusCode::BAD_REQUEST,
        );
    }

    #[actix_web::test]
    async fn the_description_is_the_only_thing_a_patch_changes() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        server
            .db()
            .groups()
            .create(crate::db::repos::NewGroup::manual(
                GroupName::parse("Blue").unwrap(),
            ))
            .await
            .unwrap();

        let app = test::init_service(App::new().configure(server.app())).await;

        let updated: Group = test::call_and_read_body_json(
            &app,
            test::TestRequest::patch()
                .uri("/api/v1/groups/Blue")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({
                    "description": "  The blue team  ",
                    "name": "Renamed",
                }))
                .to_request(),
        )
        .await;

        assert_eq!(updated.name.as_str(), "Blue", "a channel is its name");
        assert_eq!(updated.description.as_deref(), Some("The blue team"));

        let cleared: Group = test::call_and_read_body_json(
            &app,
            test::TestRequest::patch()
                .uri("/api/v1/groups/Blue")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({ "description": "" }))
                .to_request(),
        )
        .await;

        assert_eq!(cleared.description, None);
    }

    #[actix_web::test]
    async fn a_patch_that_would_change_nothing_is_refused() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::patch()
                    .uri("/api/v1/groups/__ANON__")
                    .insert_header(("authorization", bearer(&session)))
                    .set_json(serde_json::json!({}))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
        );
    }

    #[actix_web::test]
    async fn deleting_a_channel_keeps_its_bit_position_and_is_recorded() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        let blue = server
            .db()
            .groups()
            .create(crate::db::repos::NewGroup::manual(
                GroupName::parse("Blue").unwrap(),
            ))
            .await
            .unwrap();

        let app = test::init_service(App::new().configure(server.app())).await;

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::delete()
                    .uri("/api/v1/groups/Blue")
                    .insert_header(("authorization", bearer(&session)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::NO_CONTENT,
        );

        let next = server
            .db()
            .groups()
            .create(crate::db::repos::NewGroup::manual(
                GroupName::parse("Red").unwrap(),
            ))
            .await
            .unwrap();

        assert_ne!(
            next.bitpos, blue.bitpos,
            "a reused bit would hand one channel's traffic to another",
        );

        assert!(
            server
                .db()
                .audit(crate::db::AuditQuery::about("Blue", 10))
                .await
                .unwrap()
                .iter()
                .any(|record| record.action == "group.deleted"),
        );
    }

    #[actix_web::test]
    async fn the_default_channel_cannot_be_deleted() {
        // Every client expects to be able to talk on it the moment it connects,
        // so an installation without it would look broken to every EUD.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::delete()
                    .uri("/api/v1/groups/__ANON__")
                    .insert_header(("authorization", bearer(&session)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
        );
    }

    #[actix_web::test]
    async fn a_channel_that_is_not_here_is_a_not_found() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        for request in [
            test::TestRequest::delete()
                .uri("/api/v1/groups/Missing")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
            test::TestRequest::patch()
                .uri("/api/v1/groups/Missing")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({ "description": "x" }))
                .to_request(),
        ] {
            assert_eq!(
                test::call_service(&app, request).await.status(),
                StatusCode::NOT_FOUND,
            );
        }
    }

    #[actix_web::test]
    async fn an_ordinary_caller_administers_nothing_here() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        for request in [
            test::TestRequest::get()
                .uri("/api/v1/groups")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
            test::TestRequest::post()
                .uri("/api/v1/groups")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({ "name": "Mine" }))
                .to_request(),
            test::TestRequest::delete()
                .uri("/api/v1/groups/__ANON__")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        ] {
            assert_eq!(
                test::call_service(&app, request).await.status(),
                StatusCode::FORBIDDEN,
            );
        }
    }
}
