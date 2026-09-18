//! `/api/v1/devices`: the clients that have enrolled, and which channels each
//! of them has switched on.
//!
//! Administrative over everybody, self-service over your own — the rule lives
//! in [`super::subject`]. A person seeing their own phones and laptops in the
//! UI is the point; seeing everybody's is an administrator's job.
//!
//! # Why the active-channel state is here as well as on the Marti API
//!
//! `PUT /Marti/api/groups/active?clientUid=` is what ATAK itself calls, and it
//! writes the same rows. This endpoint exists so an administrator can correct a
//! device that has switched a channel off and cannot be reached to switch it
//! back on — and so the admin UI can show the state without speaking the Marti
//! dialect.

use actix_web::{HttpResponse, web};
use rustak_api::{ActiveGroup, AuditCategory, AuditOutcome, Device};

use crate::db::{AuditEntry, repos::Page};
use crate::identity::{devices, members};
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Authenticated;
use super::subject::{self, failed};

/// How many devices one page carries.
const PAGE_SIZE: u32 = 500;

/// What a listing may be narrowed by.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    /// Whose devices to list. Absent means every device for an administrator
    /// and the caller's own for anybody else.
    #[serde(default)]
    pub username: Option<Username>,
}

/// `GET /api/v1/devices`.
///
/// # Errors
///
/// A `403` when somebody names an account that is not theirs, a `404` when an
/// administrator names one that is not here, and a `500` when a read fails.
pub async fn list(
    context: web::Data<AppContext>,
    query: web::Query<ListQuery>,
    caller: Authenticated,
) -> ApiResult {
    let rows = match (&query.username, caller.principal.is_admin) {
        (None, true) => devices::list(context.db(), Page::first(PAGE_SIZE)).await,
        (requested, _) => {
            let subject = subject::resolve(&context, &caller, requested.as_ref()).await?;

            devices::list_for_user(context.db(), subject.user.id).await
        }
    }
    .map_err(|err| failed(&context, &err))?;

    let listed: Vec<Device> = devices::to_dtos(context.db(), &rows)
        .await
        .map_err(|err| failed(&context, &err))?;

    Ok(json_ok(&listed))
}

/// `GET /api/v1/devices/{uid}`.
///
/// # Errors
///
/// A `400` for a uid no client could have sent, a `403` when the device is
/// somebody else's, a `404` when it is not here, and a `500` when a read fails.
pub async fn get(
    context: web::Data<AppContext>,
    uid: web::Path<String>,
    caller: Authenticated,
) -> ApiResult {
    let (row, owner) = load(&context, &uid, &caller).await?;

    Ok(json_ok(&devices::to_dto(&row, owner)))
}

/// `DELETE /api/v1/devices/{uid}`.
///
/// Forgetting a device does not revoke the certificates it enrolled with: those
/// belong to the account, and taking them back is what
/// `DELETE /api/v1/credentials/{id}` and the certificate endpoints are for.
///
/// # Errors
///
/// As [`get`], plus a `500` when the write fails.
pub async fn remove(
    context: web::Data<AppContext>,
    uid: web::Path<String>,
    caller: Authenticated,
) -> ApiResult {
    let (row, owner) = load(&context, &uid, &caller).await?;

    devices::delete(context.db(), row.id)
        .await
        .map_err(|err| failed(&context, &err))?;

    record(
        &context,
        "device.removed",
        &caller,
        &owner,
        serde_json::json!({ "device": row.uid.as_str() }),
    )
    .await;

    Ok(HttpResponse::NoContent().finish())
}

/// `PUT /api/v1/devices/{uid}/active-groups`.
///
/// Answers with the device's whole state rather than an echo of the request, so
/// a caller that sent a channel which has since been deleted can see that it
/// was dropped.
///
/// # Errors
///
/// As [`get`], plus a `500` when a write fails.
pub async fn set_active_groups(
    context: web::Data<AppContext>,
    uid: web::Path<String>,
    body: web::Json<Vec<ActiveGroup>>,
    caller: Authenticated,
) -> ApiResult {
    let (row, owner) = load(&context, &uid, &caller).await?;
    let requested = body.into_inner();

    let applied = members::set_active(context.db(), row.id, &requested)
        .await
        .map_err(|err| failed(&context, &err))?;

    record(
        &context,
        "device.channels-changed",
        &caller,
        &owner,
        serde_json::json!({
            "device": row.uid.as_str(),
            "requested": requested.len(),
            "applied": applied,
        }),
    )
    .await;

    let state = members::active_for_device(context.db(), row.id)
        .await
        .map_err(|err| failed(&context, &err))?;

    // TODO(M2-08): emit `t-x-g-c` to this account's *other* devices and
    // re-authenticate any live subscription this device holds, per
    // `compat/groups.md` §3. Nothing streams yet, so there is nothing to tell.
    Ok(json_ok(&state))
}

/// Reads a device and confirms the caller may act on it.
async fn load(
    context: &AppContext,
    uid: &str,
    caller: &Authenticated,
) -> Result<(crate::db::repos::DeviceRow, Username), ApiError> {
    let uid = DeviceUid::parse(uid)
        .map_err(|err| ApiError::bad_request(format!("That is not a device identifier: {err}")))?;

    let row = devices::get(context.db(), &uid)
        .await
        .map_err(|err| failed(context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no such device."))?;

    if subject::owns(caller, row.user_id)? {
        return Ok((row, caller.user.username.clone()));
    }

    let owner = context
        .db()
        .users()
        .get(row.user_id)
        .await
        .map_err(|err| failed(context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no such device."))?;

    Ok((row, owner.username))
}

/// Writes what was changed, to whose device, and by whom.
async fn record(
    context: &AppContext,
    action: &'static str,
    caller: &Authenticated,
    owner: &Username,
    detail: serde_json::Value,
) {
    let entry = AuditEntry::new(AuditCategory::Administration, action, AuditOutcome::Success)
        .subject(owner)
        .actor(&caller.user.username)
        .detail(detail);

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a device change in the audit log.");
        context.session().record_human_error(&err);
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::{Direction, GroupName};

    use super::*;
    use crate::db::repos::DeviceSeen;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    async fn device_for(server: &TestServer, username: &str, uid: &str) -> DeviceId {
        let user = server
            .db()
            .users()
            .get_by_username(&Username::parse(username).unwrap())
            .await
            .unwrap()
            .unwrap();

        devices::upsert_seen(
            server.db(),
            &DeviceUid::parse(uid).unwrap(),
            user.id,
            DeviceSeen {
                callsign: Some("ALPHA-1".into()),
                platform: Some("ATAK-CIV".into()),
                ..DeviceSeen::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    #[actix_web::test]
    async fn a_person_sees_their_own_devices_and_an_administrator_sees_everybodys() {
        let server = TestServer::start().await;
        let (_, grace) = server.signed_in("grace", false).await;
        let (_, ada) = server.signed_in("ada", true).await;
        device_for(&server, "grace", "ANDROID-1").await;
        device_for(&server, "ada", "ANDROID-2").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let hers: Vec<Device> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/devices")
                .insert_header(("authorization", bearer(&grace)))
                .to_request(),
        )
        .await;

        assert_eq!(hers.len(), 1);
        assert_eq!(hers[0].uid.as_str(), "ANDROID-1");
        assert_eq!(hers[0].username.as_str(), "grace");
        assert_eq!(hers[0].display(), "ALPHA-1");

        let all: Vec<Device> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/devices")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(all.len(), 2);

        let narrowed: Vec<Device> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/devices?username=grace")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(narrowed.len(), 1);
    }

    #[actix_web::test]
    async fn a_stranger_cannot_read_change_or_remove_somebody_elses_device() {
        let server = TestServer::start().await;
        let (_, grace) = server.signed_in("grace", false).await;
        server.user("ada", true).await;
        device_for(&server, "ada", "ANDROID-2").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        for request in [
            test::TestRequest::get()
                .uri("/api/v1/devices/ANDROID-2")
                .insert_header(("authorization", bearer(&grace)))
                .to_request(),
            test::TestRequest::delete()
                .uri("/api/v1/devices/ANDROID-2")
                .insert_header(("authorization", bearer(&grace)))
                .to_request(),
            test::TestRequest::put()
                .uri("/api/v1/devices/ANDROID-2/active-groups")
                .insert_header(("authorization", bearer(&grace)))
                .set_json(serde_json::json!([]))
                .to_request(),
        ] {
            assert_eq!(
                test::call_service(&app, request).await.status(),
                StatusCode::FORBIDDEN,
            );
        }
    }

    #[actix_web::test]
    async fn switching_a_channel_off_is_recorded_and_narrows_the_subscription() {
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("grace", false).await;
        let device = device_for(&server, "grace", "ANDROID-1").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let state: Vec<ActiveGroup> = test::call_and_read_body_json(
            &app,
            test::TestRequest::put()
                .uri("/api/v1/devices/ANDROID-1/active-groups")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!([
                    { "group": "__ANON__", "direction": "OUT", "active": false },
                ]))
                .to_request(),
        )
        .await;

        assert_eq!(state.len(), 1);
        assert!(!state[0].active);

        let effective = members::effective_for_device(server.db(), user.id, device, true)
            .await
            .unwrap();
        let anon = server
            .db()
            .groups()
            .get_by_name(&GroupName::anon())
            .await
            .unwrap()
            .unwrap();

        assert!(!effective.contains(anon.bitpos, Direction::Out));
        assert!(effective.contains(anon.bitpos, Direction::In));

        assert!(
            server
                .db()
                .audit(crate::db::AuditQuery::about("grace", 10))
                .await
                .unwrap()
                .iter()
                .any(|record| record.action == "device.channels-changed"),
        );
    }

    #[actix_web::test]
    async fn a_channel_that_has_gone_is_dropped_from_the_state_rather_than_refused() {
        // ATAK sends back the list it was given; a channel deleted since would
        // otherwise break every client that still had it cached.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;
        device_for(&server, "grace", "ANDROID-1").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let state: Vec<ActiveGroup> = test::call_and_read_body_json(
            &app,
            test::TestRequest::put()
                .uri("/api/v1/devices/ANDROID-1/active-groups")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!([
                    { "group": "Vanished", "direction": "BOTH", "active": true },
                ]))
                .to_request(),
        )
        .await;

        assert!(state.is_empty());
    }

    #[actix_web::test]
    async fn an_administrator_can_forget_a_device_without_touching_the_account() {
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", true).await;
        let grace = server.user("grace", false).await;
        device_for(&server, "grace", "ANDROID-1").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::delete()
                .uri("/api/v1/devices/ANDROID-1")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            devices::get(server.db(), &DeviceUid::parse("ANDROID-1").unwrap())
                .await
                .unwrap()
                .is_none(),
        );
        assert!(server.db().users().get(grace.id).await.unwrap().is_some());

        assert!(
            server
                .db()
                .audit(crate::db::AuditQuery::about("grace", 10))
                .await
                .unwrap()
                .iter()
                .any(|record| record.action == "device.removed"
                    && record.actor.as_deref() == Some("ada")),
        );
    }

    #[actix_web::test]
    async fn a_device_that_is_not_here_is_a_not_found_and_a_bad_uid_is_a_bad_request() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/api/v1/devices/ANDROID-404")
                    .insert_header(("authorization", bearer(&session)))
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
                    .uri("/api/v1/devices/%20")
                    .insert_header(("authorization", bearer(&session)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
        );
    }
}
