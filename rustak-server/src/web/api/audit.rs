//! `GET /api/v1/audit`: what has happened, newest first.
//!
//! Administrative, and paged by identifier rather than by offset: entries are
//! written while somebody is reading, and an offset would silently repeat or
//! skip rows as they arrive.

use actix_web::web;
use rustak_api::AuditCategory;

use crate::db::AuditQuery;
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;

/// The largest page we will assemble, whatever was asked for.
const MAX_LIMIT: usize = 500;

/// The page size when none was asked for.
const DEFAULT_LIMIT: usize = 100;

/// What the caller may narrow the feed by.
#[derive(Debug, Default, Deserialize)]
pub struct AuditFilter {
    /// One area of the system.
    #[serde(default)]
    pub category: Option<String>,
    /// Entries about one user, device or channel.
    #[serde(default)]
    pub subject: Option<String>,
    /// Entries recorded by one actor.
    #[serde(default)]
    pub actor: Option<String>,
    /// Page backwards from an identifier a previous page returned.
    #[serde(default)]
    pub before: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Answers with a page of the audit log.
///
/// # Errors
///
/// A `400` when the category is not one we have, and a `500` when the read
/// fails.
pub async fn list(
    context: web::Data<AppContext>,
    filter: web::Query<AuditFilter>,
    _: Administrative,
) -> ApiResult {
    let category = match filter.category.as_deref() {
        Some(named) => Some(AuditCategory::parse(named).ok_or_else(|| {
            ApiError::bad_request(format!("'{named}' is not one of the areas we record."))
        })?),
        None => None,
    };

    let query = AuditQuery {
        category,
        subject: filter.subject.clone(),
        actor: filter.actor.clone(),
        before: filter.before,
        limit: filter.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
    };

    let records = context.db().audit(query).await.map_err(|err| {
        context.session().record_human_error(&err);
        ApiError::from_human(&err)
    })?;

    Ok(json_ok(&records))
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::{AuditOutcome, AuditRecord};

    use super::*;
    use crate::db::AuditEntry;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    async fn seed(server: &TestServer) {
        for (category, action) in [
            (AuditCategory::Authentication, "login"),
            (AuditCategory::Administration, "user.created"),
            (AuditCategory::Pki, "certificate.issued"),
        ] {
            server
                .db()
                .record(AuditEntry::new(category, action, AuditOutcome::Success).subject("ada"))
                .await
                .unwrap();
        }
    }

    #[actix_web::test]
    async fn the_feed_comes_back_newest_first() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        seed(&server).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let records: Vec<AuditRecord> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/audit?limit=2")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(records.len(), 2);
        assert!(records[0].id > records[1].id);
    }

    #[actix_web::test]
    async fn a_category_narrows_the_feed() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        seed(&server).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let records: Vec<AuditRecord> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/audit?category=pki")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].category, AuditCategory::Pki);
    }

    #[actix_web::test]
    async fn an_area_we_do_not_record_is_a_bad_request_rather_than_an_empty_page() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/audit?category=nonsense")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn the_page_size_is_capped_whatever_was_asked_for() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        seed(&server).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        // Asking for a million rows must not be a way to make the server
        // assemble a million rows.
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/audit?limit=1000000")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn an_ordinary_account_cannot_read_the_audit_log() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/audit")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
