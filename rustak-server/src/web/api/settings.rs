//! `GET /api/v1/settings`: what this server calls itself and where it is
//! reached.
//!
//! Administrative, because the host names and the node identifier are what an
//! enrolment package is built from, and an installation should not have to
//! publish them to everybody who can sign in.

use actix_web::web;

use crate::identity::settings;
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;

/// Answers with the resolved settings: the configuration file's values where it
/// gives any, the wizard's where it does not.
///
/// # Errors
///
/// A `500` when the stored settings cannot be read.
pub async fn get(context: web::Data<AppContext>, _: Administrative) -> ApiResult {
    let resolved = settings::resolve(&context.config(), context.db())
        .await
        .map_err(|err| {
            context.session().record_human_error(&err);
            ApiError::from_human(&err)
        })?;

    Ok(json_ok(&resolved))
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::{ServerSettings, ServerSettingsRequest};

    use super::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    #[actix_web::test]
    async fn an_administrator_is_told_what_the_server_is() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        settings::save(
            server.db(),
            &ServerSettingsRequest {
                name: "Hilltop TAK".to_string(),
                domains: vec!["tak.example.com".to_string()],
                base_url: None,
            },
            None,
        )
        .await
        .unwrap();

        let app = test::init_service(App::new().configure(server.app())).await;

        let settings: ServerSettings = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/settings")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(
            settings.domains,
            vec!["localhost".to_string()],
            "the configuration file's host name wins over the wizard's",
        );
        assert_eq!(settings.name, "Hilltop TAK");
        assert!(settings.node_id.is_some());
    }

    #[actix_web::test]
    async fn somebody_who_is_not_an_administrator_is_refused() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/settings")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
