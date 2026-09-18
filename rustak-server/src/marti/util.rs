//! The odds and ends: roles, the landing page, the clock, and crash reports.
//!
//! Four endpoints with nothing in common except that a client calls them and
//! would be confused by a refusal.
//!
//! # Roles are a bare array of Spring authority names
//!
//! `/Marti/api/util/user/roles` predates the envelope and TAK Server never
//! wrapped it, so neither do we. The names are Spring Security authorities, and
//! a client matches them as strings: `ROLE_ANONYMOUS` is always present
//! (everybody is at least anonymous), `ROLE_WEBTAK` says the browser client may
//! load, `ROLE_ADMIN` unlocks the administrative pages, and `ROLE_READONLY`
//! says the caller may look and not send.
//!
//! **`ROLE_READONLY` is decided by `IN` membership, not by a flag.** Sending to
//! a channel is `IN`; a caller who holds `IN` on nothing has nowhere to send,
//! which is exactly what read-only means. Deriving it rather than storing it
//! means an administrator who removes the last `IN` grant does not also have to
//! remember to set a flag.
//!
//! # Crash reports are accepted whether or not they are kept
//!
//! A client that cannot file one retries, so `POST /Marti/ErrorLog` answers
//! `200` either way. What `[marti] store_error_logs` decides is whether the
//! body is written to the key/value store — truncated, and with the oldest
//! dropped once `error_log_retention` is reached — or discarded. The *first*
//! discard in a process is audited, so an operator reading the log can see the
//! setting is taking effect, but a client cannot flood the audit log by posting
//! in a loop.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use actix_web::http::StatusCode;
use actix_web::web;
use rustak_api::Direction;
use rustak_api::{AuditCategory, AuditOutcome};

use crate::db::AuditEntry;
use crate::prelude::*;

use super::error::{MartiError, MartiResult};
use super::extract::CiQuery;
use super::principal::MartiPrincipal;
use super::{response, time};

/// Everybody is at least this.
const ROLE_ANONYMOUS: &str = "ROLE_ANONYMOUS";

/// Unlocks the administrative pages.
const ROLE_ADMIN: &str = "ROLE_ADMIN";

/// Says the browser client may load.
const ROLE_WEBTAK: &str = "ROLE_WEBTAK";

/// Says the caller may look and not send.
const ROLE_READONLY: &str = "ROLE_READONLY";

/// Where a client's landing page is, for an ordinary caller.
const HOME_WEBTAK: &str = "/webtak/index.html";

/// Where it is for an administrator.
const HOME_ADMIN: &str = "/Marti/metrics/index.html";

/// The key/value partition crash reports are kept in.
const ERROR_LOG_PARTITION: &str = "marti-error-log";

/// The most bytes of a crash report that are kept.
///
/// A stack trace is a few kilobytes; anything past this is either a client bug
/// or somebody using the endpoint as storage.
const MAX_ERROR_LOG_BYTES: usize = 64 * 1024;

/// Whether this process has already said that it is discarding crash reports.
static DISCARD_AUDITED: AtomicBool = AtomicBool::new(false);

/// Orders crash reports that arrive within the same millisecond; see [`store`].
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// `GET /Marti/api/util/user/roles` — a bare array of authority names.
///
/// # Errors
///
/// [`MartiError::Internal`] if the caller's channel grants cannot be read.
pub async fn roles(who: MartiPrincipal, context: web::Data<AppContext>) -> MartiResult {
    let mut roles = vec![ROLE_ANONYMOUS.to_string()];

    if who.is_admin() {
        roles.push(ROLE_ADMIN.to_string());
    }

    roles.push(ROLE_WEBTAK.to_string());

    if !can_send(&who, &context).await? {
        roles.push(ROLE_READONLY.to_string());
    }

    Ok(response::bare_json(&roles))
}

/// Whether the caller holds `IN` on any channel.
///
/// An anonymous caller holds nothing, so they are read-only — which is what a
/// client with no credential should be told before it tries to send.
async fn can_send(who: &MartiPrincipal, context: &AppContext) -> Result<bool, MartiError> {
    let Some(resolved) = who.identity.as_ref() else {
        return Ok(false);
    };

    let grants = crate::identity::members::grants_for_user(context.db(), resolved.user.id).await?;

    Ok(grants
        .iter()
        .any(|grant| matches!(grant.direction, Direction::In | Direction::Both)))
}

/// `GET /Marti/api/home` — where a browser should go next, as plain text.
///
/// Informational: no client depends on it, and the paths are the ones TAK
/// Server names so that a bookmark from one works against the other.
///
/// # Errors
///
/// Never.
pub async fn home(who: MartiPrincipal) -> MartiResult {
    let path = if who.is_admin() {
        HOME_ADMIN
    } else {
        HOME_WEBTAK
    };

    Ok(response::text(StatusCode::OK, path))
}

/// `GET /Marti/GetTime` — the current time, as plain text.
///
/// ATAK uses it to measure clock drift against the server, which is why the
/// format is the padded-millisecond one rather than anything more readable: it
/// is parsed, not displayed.
///
/// # Errors
///
/// Never.
pub async fn get_time() -> MartiResult {
    Ok(response::text(
        StatusCode::OK,
        time::cot_date(chrono::Utc::now()),
    ))
}

/// A crash report as it is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredErrorLog {
    /// When it arrived.
    pub received_at: chrono::DateTime<chrono::Utc>,

    /// Who sent it, when they were identified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    /// The device that sent it, as it named itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_uid: Option<String>,

    /// How many bytes arrived, before truncation.
    pub size: usize,

    /// The report, truncated to 64 KiB and read as UTF-8 with
    /// invalid sequences replaced — it is a client's own text and we are not
    /// going to refuse it over an encoding.
    pub body: String,
}

/// `POST /Marti/ErrorLog` — accept a client's crash report.
///
/// # Errors
///
/// Never as a refusal of the request. A storage failure is logged and the
/// client is still told `200`, because there is nothing it could do about it
/// and a retry loop helps nobody.
pub async fn error_log(
    who: MartiPrincipal,
    query: CiQuery,
    body: web::Bytes,
    context: web::Data<AppContext>,
) -> MartiResult {
    let config = context.config();

    if !config.marti.store_error_logs {
        discard(&context, who.username()).await;

        return Ok(response::text(StatusCode::OK, ""));
    }

    let entry = StoredErrorLog {
        received_at: chrono::Utc::now(),
        username: who.username().map(str::to_string),
        client_uid: query.get("clientUid").map(str::to_string),
        size: body.len(),
        body: String::from_utf8_lossy(&body[..body.len().min(MAX_ERROR_LOG_BYTES)]).into_owned(),
    };

    if let Err(err) = store(&context, &entry, config.marti.error_log_retention).await {
        warn!(error = %err, "Could not keep a client's crash report.");
    }

    Ok(response::text(StatusCode::OK, ""))
}

/// Writes one report and prunes the oldest beyond the retention.
///
/// Keyed by the arrival instant so that the natural string ordering of the keys
/// is chronological — which is what makes pruning a sort and a truncate rather
/// than a second index.
///
/// The counter after the timestamp is what makes that ordering hold *within* a
/// millisecond: a crashing client posts several reports at once, and a random
/// suffix would order them arbitrarily, so the pruner could drop the newest and
/// keep the oldest. It resets when the process does, which only matters for two
/// reports in the same millisecond either side of a restart.
async fn store(
    context: &AppContext,
    entry: &StoredErrorLog,
    retention: usize,
) -> Result<(), Error> {
    let key = format!(
        "{}-{:06}",
        entry.received_at.format("%Y%m%d%H%M%S%3f"),
        SEQUENCE.fetch_add(1, Ordering::Relaxed) % 1_000_000,
    );

    context
        .kv()
        .set(ERROR_LOG_PARTITION, key, entry.clone())
        .await?;

    let mut keys: Vec<String> = context
        .kv()
        .list::<serde_json::Value>(ERROR_LOG_PARTITION)
        .await?
        .into_iter()
        .map(|(key, _)| key)
        .collect();

    if keys.len() <= retention {
        return Ok(());
    }

    keys.sort();
    let excess = keys.len() - retention;

    for key in keys.into_iter().take(excess) {
        context.kv().remove(ERROR_LOG_PARTITION, key).await?;
    }

    Ok(())
}

/// Notes, once per process, that crash reports are being thrown away.
///
/// Once rather than per request: an audit entry for every report would be a way
/// for any client to fill the log by posting in a loop, and the operator only
/// needs to be told that the setting is doing what it says.
async fn discard(context: &AppContext, username: Option<&str>) {
    debug!("Discarded a client's crash report; [marti] store_error_logs is off.");

    if DISCARD_AUDITED.swap(true, Ordering::Relaxed) {
        return;
    }

    let mut entry = AuditEntry::new(
        AuditCategory::Administration,
        "marti.error-log.discarded",
        AuditOutcome::Success,
    )
    .message(
        "A client posted a crash report and it was discarded, because [marti] \
         store_error_logs is off. Further discards are not logged.",
    );

    if let Some(username) = username {
        entry = entry.subject(username);
    }

    if let Err(err) = context.audit().record(entry).await {
        warn!(error = %err, "Could not record that a crash report was discarded.");
    }
}

#[cfg(test)]
mod tests {
    use actix_web::body::MessageBody as _;
    use actix_web::{App, test};

    use super::*;
    use crate::db::{AuditQuery, KeyValueStore as _};
    use crate::testing::{TestServer, context::bearer};

    async fn body_of(server: &TestServer, request: test::TestRequest) -> String {
        let app = test::init_service(App::new().configure(server.app())).await;
        let response = test::call_service(&app, request.to_request()).await;

        String::from_utf8(
            response
                .into_body()
                .try_into_bytes()
                .expect("a body")
                .to_vec(),
        )
        .expect("utf-8")
    }

    #[actix_web::test]
    async fn a_caller_with_nowhere_to_send_is_told_they_are_read_only() {
        // Derived from `IN` membership rather than a flag: removing the last
        // grant is enough, with nothing else to remember.
        let server = TestServer::start().await;

        let roles: Vec<String> = serde_json::from_str(
            &body_of(
                &server,
                test::TestRequest::get().uri("/Marti/api/util/user/roles"),
            )
            .await,
        )
        .unwrap();

        assert_eq!(
            roles,
            ["ROLE_ANONYMOUS", "ROLE_WEBTAK", "ROLE_READONLY"],
            "an anonymous caller holds IN on nothing",
        );
    }

    #[actix_web::test]
    async fn somebody_in_a_channel_is_not_read_only() {
        let server = TestServer::start().await;
        // `TestServer::user` puts the account in the default channel, which is
        // an IN grant.
        let (_, session) = server.signed_in("ada", false).await;

        let roles: Vec<String> = serde_json::from_str(
            &body_of(
                &server,
                test::TestRequest::get()
                    .uri("/Marti/api/util/user/roles")
                    .insert_header(("authorization", bearer(&session))),
            )
            .await,
        )
        .unwrap();

        assert_eq!(roles, ["ROLE_ANONYMOUS", "ROLE_WEBTAK"]);
    }

    #[actix_web::test]
    async fn an_administrator_is_told_so_in_the_same_array() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let roles: Vec<String> = serde_json::from_str(
            &body_of(
                &server,
                test::TestRequest::get()
                    .uri("/Marti/api/util/user/roles")
                    .insert_header(("authorization", bearer(&session))),
            )
            .await,
        )
        .unwrap();

        assert_eq!(roles, ["ROLE_ANONYMOUS", "ROLE_ADMIN", "ROLE_WEBTAK"]);
    }

    #[actix_web::test]
    async fn the_roles_endpoint_is_a_bare_array_rather_than_an_envelope() {
        // It predates the envelope and TAK Server never wrapped it.
        let server = TestServer::start().await;

        let body = body_of(
            &server,
            test::TestRequest::get().uri("/Marti/api/util/user/roles"),
        )
        .await;

        assert!(body.starts_with('['), "{body}");
        assert!(!body.contains("\"type\""), "{body}");
    }

    #[actix_web::test]
    async fn the_landing_page_depends_on_who_is_asking() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        assert_eq!(
            body_of(&server, test::TestRequest::get().uri("/Marti/api/home")).await,
            "/webtak/index.html",
        );
        assert_eq!(
            body_of(
                &server,
                test::TestRequest::get()
                    .uri("/Marti/api/home")
                    .insert_header(("authorization", bearer(&session))),
            )
            .await,
            "/Marti/metrics/index.html",
        );
    }

    #[actix_web::test]
    async fn the_clock_is_served_in_the_format_a_client_parses() {
        // ATAK measures drift against it, so it is parsed rather than read.
        let server = TestServer::start().await;

        let body = body_of(&server, test::TestRequest::get().uri("/Marti/GetTime")).await;

        let parsed = time::parse_date(&body).expect("our own formatter round-trips");
        assert!(
            (chrono::Utc::now() - parsed).num_seconds().abs() < 5,
            "{body}",
        );
        assert_eq!(time::cot_date(parsed), body, "padded millis, literal Z");
    }

    #[actix_web::test]
    async fn a_crash_report_is_kept_with_who_sent_it() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;
        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/Marti/ErrorLog?clientUid=ANDROID-1")
                .insert_header(("authorization", bearer(&session)))
                .set_payload("java.lang.NullPointerException")
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), 200);

        let kept: Vec<(String, StoredErrorLog)> =
            server.db().list(ERROR_LOG_PARTITION).await.unwrap();

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].1.username.as_deref(), Some("ada"));
        assert_eq!(kept[0].1.client_uid.as_deref(), Some("ANDROID-1"));
        assert_eq!(kept[0].1.body, "java.lang.NullPointerException");
    }

    #[actix_web::test]
    async fn an_enormous_report_is_truncated_rather_than_refused() {
        // A client that cannot file a crash report retries; refusing one would
        // trade a large write for an endless loop of them.
        let server = TestServer::start().await;
        let payload = "x".repeat(MAX_ERROR_LOG_BYTES * 2);

        let app = test::init_service(App::new().configure(server.app())).await;
        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/Marti/ErrorLog")
                .set_payload(payload.clone())
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), 200);

        let kept: Vec<(String, StoredErrorLog)> =
            server.db().list(ERROR_LOG_PARTITION).await.unwrap();

        assert_eq!(kept[0].1.size, payload.len(), "the real size is recorded");
        assert_eq!(kept[0].1.body.len(), MAX_ERROR_LOG_BYTES);
    }

    #[actix_web::test]
    async fn the_oldest_reports_are_dropped_once_the_cap_is_reached() {
        let server = TestServer::start_with(|config| {
            config.marti.error_log_retention = 2;
        })
        .await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for index in 0..5 {
            let response = test::call_service(
                &app,
                test::TestRequest::post()
                    .uri("/Marti/ErrorLog")
                    .set_payload(format!("report {index}"))
                    .to_request(),
            )
            .await;

            assert_eq!(response.status(), 200);
        }

        let kept: Vec<(String, StoredErrorLog)> =
            server.db().list(ERROR_LOG_PARTITION).await.unwrap();

        assert_eq!(kept.len(), 2);
        assert!(
            kept.iter().any(|(_, entry)| entry.body == "report 4"),
            "the most recent one is the one that is kept",
        );
    }

    #[actix_web::test]
    async fn an_installation_that_keeps_none_still_answers_and_says_so_once() {
        let server = TestServer::start_with(|config| {
            config.marti.store_error_logs = false;
        })
        .await;
        // The "say it once" flag is process-wide, so this test asserts the
        // storage behaviour and that the audit entry is at most one.
        let app = test::init_service(App::new().configure(server.app())).await;

        for _ in 0..3 {
            let response = test::call_service(
                &app,
                test::TestRequest::post()
                    .uri("/Marti/ErrorLog")
                    .set_payload("a report")
                    .to_request(),
            )
            .await;

            assert_eq!(response.status(), 200);
        }

        let kept: Vec<(String, StoredErrorLog)> =
            server.db().list(ERROR_LOG_PARTITION).await.unwrap();
        assert!(kept.is_empty(), "nothing is kept when storage is off");

        let audited = server
            .db()
            .audit(AuditQuery::recent(50))
            .await
            .unwrap()
            .into_iter()
            .filter(|record| record.action == "marti.error-log.discarded")
            .count();

        assert!(
            audited <= 1,
            "a client must not be able to fill the audit log by posting in a loop",
        );
    }
}
