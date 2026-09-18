//! `GET /api/v1/me`: who the caller is, as far as this server is concerned.
//!
//! The channels are read per request rather than carried in the token, so that
//! removing somebody from one takes effect at once instead of when their token
//! expires. That is the same reason the administrative flag is read from the
//! account rather than believed from the `scope` claim.

use actix_web::web;

use crate::identity::users;
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Authenticated;

/// Answers with the caller's identity.
///
/// # Errors
///
/// A `500` when the channels cannot be read.
pub async fn me(context: web::Data<AppContext>, caller: Authenticated) -> ApiResult {
    let me = users::me(context.db(), &caller.user, &caller.principal)
        .await
        .map_err(|err| {
            context.session().record_human_error(&err);
            ApiError::from_human(&err)
        })?;

    Ok(json_ok(&me))
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::{AuthVia, Me};

    use super::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    #[actix_web::test]
    async fn the_caller_is_described_with_the_channels_they_hold_now() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);

        let me: Me = test::read_body_json(response).await;

        assert_eq!(me.username.as_str(), "ada");
        assert_eq!(me.via, AuthVia::Bearer);
        assert!(me.is_admin, "the account was created as an administrator");
        assert!(
            me.groups.iter().any(|held| held.group.is_anon()),
            "a new account is in the default channel",
        );
    }

    #[actix_web::test]
    async fn an_ordinary_account_is_not_reported_as_an_administrator() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let me: Me = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert!(!me.is_admin);
    }

    #[actix_web::test]
    async fn disabling_an_account_ends_its_session_at_once() {
        // The account is read per request rather than believed from the token,
        // which is the whole reason this takes effect before the token expires.
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", true).await;

        server
            .db()
            .users()
            .set_disabled(user.id, true)
            .await
            .unwrap();

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
