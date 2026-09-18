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
//!
//! # Members are listed here as well as under each account
//!
//! `GET /api/v1/users/{username}/groups` answers "which channels does this
//! person hold"; [`members`] answers the same relation read the other way, so
//! a page about one channel is one request rather than one per account.
//!
//! Each row carries whether that member currently has the channel *switched
//! on*, which is a different question from whether they may use it: the
//! membership is the right and the selection is the preference, and an
//! administrator looking at a channel nobody seems to be talking on wants to
//! see the difference.

use std::collections::HashMap;

use actix_web::{HttpResponse, web};
use rustak_api::{
    AuditCategory, AuditOutcome, CreateGroupRequest, Group, GroupMember, GroupName, GroupPatch,
};

use crate::db::AuditEntry;
use crate::identity::{devices, groups};
use crate::marti::channels::{self, Selection};
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

/// `GET /api/v1/groups/{name}/members`.
///
/// One row per member per direction, which is what storage holds and what the
/// matching per-account endpoint answers with.
///
/// # Errors
///
/// A `400` for a name no channel could have, a `404` when the channel is not
/// here, and a `500` when a read fails.
pub async fn members(
    context: web::Data<AppContext>,
    name: web::Path<String>,
    _: Administrative,
) -> ApiResult {
    let name = parse(&name)?;

    let group = context
        .db()
        .groups()
        .get_by_name(&name)
        .await
        .map_err(|err| failed(&context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no channel by that name."))?;

    let held = context
        .db()
        .members()
        .list_for_group(group.id)
        .await
        .map_err(|err| failed(&context, &err))?;

    // One read for every account's name rather than one per membership: a
    // channel has two rows per member and an installation has tens of accounts.
    let owners = devices::usernames(context.db())
        .await
        .map_err(|err| failed(&context, &err))?;
    let mut selections: HashMap<UserId, Selection> = HashMap::new();
    let mut listed: Vec<GroupMember> = Vec::with_capacity(held.len());

    for membership in &held {
        let Some(username) = owners.get(&membership.user_id) else {
            // The account has gone and the foreign key has not caught up yet;
            // a member the UI cannot open is worse than a row that is absent.
            continue;
        };

        let selection = match selections.get(&membership.user_id) {
            Some(selection) => selection,
            None => {
                let read = channels::selection(&context, membership.user_id, None)
                    .await
                    .map_err(|err| refused(&err))?;

                selections.entry(membership.user_id).or_insert(read)
            }
        };

        listed.push(GroupMember {
            username: username.clone(),
            direction: membership.direction,
            active: selection.is_active(&name, membership.direction),
            source: membership.source,
        });
    }

    listed.sort_by(|left, right| {
        (left.username.as_str(), left.direction.as_str())
            .cmp(&(right.username.as_str(), right.direction.as_str()))
    });

    Ok(json_ok(&listed))
}

/// Reports a channel-selection read that failed.
///
/// [`channels::selection`] only ever fails with one of our own errors, so
/// there is nothing here a caller could act on.
fn refused(err: &crate::marti::MartiError) -> ApiError {
    error!(error = ?err, "Could not read a member's channel selection.");

    ApiError::internal()
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

    #[actix_web::test]
    async fn a_channel_lists_its_members_one_row_per_direction() {
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", true).await;
        let grace = server.user("grace", false).await;
        let blue = server
            .db()
            .groups()
            .create(crate::db::repos::NewGroup::manual(
                GroupName::parse("Blue").unwrap(),
            ))
            .await
            .unwrap();

        server
            .db()
            .members()
            .grant(
                grace.id,
                blue.id,
                rustak_api::Direction::Both,
                rustak_api::MembershipSource::Manual,
            )
            .await
            .unwrap();

        let app = test::init_service(App::new().configure(server.app())).await;

        let members: Vec<GroupMember> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/groups/Blue/members")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(members.len(), 2, "one row per direction, as storage holds");
        assert!(
            members
                .iter()
                .all(|member| member.username.as_str() == "grace")
        );
        assert!(
            members.iter().all(|member| member.active),
            "a member who has said nothing has the channel on",
        );

        // The default channel every account joins is a separate listing, which
        // is what makes the member count on a channel row mean anything.
        let anon: Vec<GroupMember> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/groups/__ANON__/members")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(anon.len(), 4, "two accounts, two directions each");
    }

    #[actix_web::test]
    async fn a_member_who_has_switched_the_channel_off_is_listed_as_off() {
        // A membership is a right and the selection is a preference. An
        // administrator looking at a channel nobody is talking on needs to see
        // which of the two is the reason.
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", true).await;
        let grace = server.user("grace", false).await;

        channels::apply(
            &server.context,
            grace.id,
            &[rustak_api::ActiveGroup {
                group: GroupName::anon(),
                direction: rustak_api::Direction::Out,
                active: false,
            }],
            None,
        )
        .await
        .unwrap();

        let app = test::init_service(App::new().configure(server.app())).await;

        let members: Vec<GroupMember> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/groups/__ANON__/members")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        let hers: Vec<&GroupMember> = members
            .iter()
            .filter(|member| member.username.as_str() == "grace")
            .collect();

        assert_eq!(hers.len(), 2);
        assert!(
            hers.iter()
                .any(|member| member.direction == rustak_api::Direction::Out && !member.active),
        );
        assert!(
            hers.iter()
                .any(|member| member.direction == rustak_api::Direction::In && member.active),
            "switching one direction off leaves the other alone",
        );
    }

    #[actix_web::test]
    async fn only_an_administrator_lists_a_channels_members() {
        let server = TestServer::start().await;
        let (_, grace) = server.signed_in("grace", false).await;
        let (_, ada) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/api/v1/groups/__ANON__/members")
                    .insert_header(("authorization", bearer(&grace)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::FORBIDDEN,
        );

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/api/v1/groups/Vanished/members")
                    .insert_header(("authorization", bearer(&ada)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::NOT_FOUND,
        );
    }
}
