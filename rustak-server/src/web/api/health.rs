//! `GET /api/v1/health`, which is reachable without a session.
//!
//! Deliberately thin. It is public, so it says whether the server is working
//! and which release it is, and nothing that would help somebody decide how to
//! attack it. The database check is one trivial query through a reader
//! connection: a server whose SQLite file has gone is not serving, and every
//! other endpoint would fail in a way far harder to read.

use actix_web::{HttpResponse, web};
use chrono::Utc;
use rustak_api::{ComponentStatus, Health};

use crate::prelude::*;

use super::error::json_ok;

/// Answers the health check.
pub async fn health(context: web::Data<AppContext>) -> HttpResponse {
    let uptime = (Utc::now() - context.started_at()).num_seconds().max(0) as u64;
    let version = env!("CARGO_PKG_VERSION");

    match reachable(context.get_ref()).await {
        Ok(()) => json_ok(&Health::ok(version, uptime)),
        Err(err) => {
            error!(error = %err, "The health check could not reach the database.");
            context.session().record_human_error(&err);

            json_ok(&Health {
                status: ComponentStatus::Down,
                version: version.to_string(),
                uptime_seconds: uptime,
                database: ComponentStatus::Down,
                // Named without detail: a public endpoint should not describe
                // our storage layout to whoever asked.
                message: Some("The database is not answering.".to_string()),
            })
        }
    }
}

/// One query, through a reader.
async fn reachable(context: &AppContext) -> Result<(), Error> {
    context
        .db()
        .read(|connection| connection.query_row("SELECT 1", [], |row| row.get::<_, i64>(0)))
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};

    use super::*;

    #[actix_web::test]
    async fn a_working_server_says_so_and_names_its_release() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(context))
                .route("/health", web::get().to(health)),
        )
        .await;

        let response =
            test::call_service(&app, test::TestRequest::get().uri("/health").to_request()).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json",
        );

        let body: Health = test::read_body_json(response).await;

        assert_eq!(body.status, ComponentStatus::Ok);
        assert_eq!(body.database, ComponentStatus::Ok);
        assert_eq!(body.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(body.message, None);
    }

    #[actix_web::test]
    async fn the_check_says_nothing_that_would_help_somebody_attack_the_server() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(context))
                .route("/health", web::get().to(health)),
        )
        .await;

        let body =
            test::call_and_read_body(&app, test::TestRequest::get().uri("/health").to_request())
                .await;
        let rendered = String::from_utf8_lossy(&body);

        for leak in ["sqlite", "path", "/", "select"] {
            assert!(
                !rendered.to_ascii_lowercase().contains(leak),
                "the public health check mentioned {leak}",
            );
        }
    }
}
