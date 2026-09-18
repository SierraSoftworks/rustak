//! The audit log.
//!
//! One append-only table holds everything worth looking back at: sign-ins,
//! enrolments, certificate issuance and revocation, mission and package
//! changes, and the server's own lifecycle. It is a table rather than entries
//! in the key/value store because every question asked of it — what happened
//! recently, what happened to this user, what has this administrator been doing
//! — wants ordering, filtering and paging, and because it is the one place an
//! operator can find out *why* something was refused.
//!
//! Entries are ordered by id rather than by `occurred_at`: several commonly
//! share a timestamp, and only the id gives a total order stable enough to page
//! with.
//!
//! **Nothing secret goes in here.** The log is rendered in a browser and can be
//! exported, so a token, a password or key material must never reach `detail` —
//! what belongs there is the identifier of the credential that was used.

use std::borrow::Cow;

use rustak_core::prelude::*;

use super::{
    ADVICE_REPORT_DEV, Database,
    row::{Timestamp, enum_col, ts},
};

pub use rustak_api::{AuditCategory, AuditOutcome, AuditRecord};

/// An entry about to be written.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    category: AuditCategory,
    action: Cow<'static, str>,
    outcome: AuditOutcome,
    subject: Option<String>,
    actor: Option<String>,
    message: Option<String>,
    detail: Option<serde_json::Value>,
}

impl AuditEntry {
    /// Begins an entry. `action` names what was attempted, as a dotted token
    /// scoped to the category: `login`, `certificate.issued`, `token.spent`.
    pub fn new(
        category: AuditCategory,
        action: impl Into<Cow<'static, str>>,
        outcome: AuditOutcome,
    ) -> Self {
        Self {
            category,
            action: action.into(),
            outcome,
            subject: None,
            actor: None,
            message: None,
            detail: None,
        }
    }

    /// The thing acted upon: a username, a device uid, a channel name.
    pub fn subject(mut self, subject: impl ToString) -> Self {
        self.subject = Some(subject.to_string());
        self
    }

    /// The person or service responsible, where there was one.
    ///
    /// Left unset for work the server does on its own initiative, which is how
    /// a scheduled renewal is told apart from one somebody asked for.
    pub fn actor(mut self, actor: impl ToString) -> Self {
        self.actor = Some(actor.to_string());
        self
    }

    /// A sentence explaining the entry, written for the person it concerns.
    pub fn message(mut self, message: impl ToString) -> Self {
        self.message = Some(message.to_string());
        self
    }

    /// Structured context that does not belong in the message.
    pub fn detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }

    /// The area of the system this entry concerns.
    pub fn category_of(&self) -> AuditCategory {
        self.category
    }

    /// How the audited operation turned out.
    pub fn outcome_of(&self) -> AuditOutcome {
        self.outcome
    }
}

/// Which entries to read back.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    /// Restrict to one area of the system.
    pub category: Option<AuditCategory>,
    /// Restrict to entries about one user, device or channel.
    pub subject: Option<String>,
    /// Restrict to entries recorded by one actor.
    pub actor: Option<String>,
    /// Return only entries older than this id, for paging backwards.
    pub before: Option<i64>,
    /// How many entries to return.
    pub limit: usize,
}

impl AuditQuery {
    /// The most recent `limit` entries.
    pub fn recent(limit: usize) -> Self {
        Self {
            limit,
            ..Default::default()
        }
    }

    /// The most recent `limit` entries concerning one subject.
    pub fn about(subject: impl ToString, limit: usize) -> Self {
        Self {
            subject: Some(subject.to_string()),
            limit,
            ..Default::default()
        }
    }

    /// Restricts the query to one category.
    pub fn in_category(mut self, category: AuditCategory) -> Self {
        self.category = Some(category);
        self
    }

    /// Restricts the query to one actor.
    pub fn by(mut self, actor: impl ToString) -> Self {
        self.actor = Some(actor.to_string());
        self
    }

    /// Pages backwards from a previously returned [`AuditRecord::id`].
    pub fn before(mut self, id: i64) -> Self {
        self.before = Some(id);
        self
    }
}

/// Reads and writes the audit log.
#[async_trait::async_trait]
pub trait AuditStore {
    /// Appends an entry.
    async fn record(&self, entry: AuditEntry) -> Result<(), Error>;

    /// Reads entries back, most recent first.
    async fn audit(&self, query: AuditQuery) -> Result<Vec<AuditRecord>, Error>;

    /// Trims the log back to the configured retention, returning how many rows
    /// were removed.
    async fn prune_audit_log(
        &self,
        retain_for: chrono::Duration,
        max_entries: usize,
    ) -> Result<usize, Error>;
}

#[async_trait::async_trait]
impl AuditStore for Database {
    #[instrument("db.audit.record", skip_all, fields(audit.category = entry.category_of().as_str(), audit.outcome = entry.outcome_of().as_str()), err(Display))]
    async fn record(&self, entry: AuditEntry) -> Result<(), Error> {
        let detail = entry
            .detail
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .wrap_system_err(
                "Failed to serialise the detail of an audit entry.",
                ADVICE_REPORT_DEV,
            )?;

        self.write(move |tx| {
            tx.execute(
                "INSERT INTO audit_log \
                   (occurred_at, category, action, outcome, subject, actor, message, detail) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    Timestamp::now(),
                    entry.category.as_str(),
                    entry.action.as_ref(),
                    entry.outcome.as_str(),
                    entry.subject,
                    entry.actor,
                    entry.message,
                    detail,
                ],
            )
        })
        .await?;

        Ok(())
    }

    #[instrument("db.audit.read", skip_all, err(Display))]
    async fn audit(&self, query: AuditQuery) -> Result<Vec<AuditRecord>, Error> {
        let (sql, bindings) = read_query(&query);

        self.read(move |c| {
            let mut statement = c.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(bindings), read_record)?;

            rows.collect()
        })
        .await
    }

    /// Two independent limits, because either alone leaves a gap: an age limit
    /// lets a busy day fill the disk inside the window, and a count limit lets a
    /// quiet installation keep entries for ever.
    #[instrument("db.audit.prune", skip(self), err(Display))]
    async fn prune_audit_log(
        &self,
        retain_for: chrono::Duration,
        max_entries: usize,
    ) -> Result<usize, Error> {
        let cutoff = Timestamp::from(chrono::Utc::now() - retain_for);
        let max_entries = max_entries as i64;

        self.write(move |tx| {
            let by_age = tx.execute("DELETE FROM audit_log WHERE occurred_at < ?1", [cutoff])?;
            let by_count = tx.execute(
                "DELETE FROM audit_log WHERE id <= \
                   COALESCE((SELECT id FROM audit_log ORDER BY id DESC LIMIT 1 OFFSET ?1), -1)",
                [max_entries],
            )?;

            Ok(by_age + by_count)
        })
        .await
    }
}

/// Builds the `WHERE` clause and its bindings.
fn read_query(query: &AuditQuery) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;

    let mut conditions: Vec<&str> = Vec::new();
    let mut bindings: Vec<Value> = Vec::new();

    if let Some(category) = query.category {
        conditions.push("category = ?");
        bindings.push(Value::Text(category.as_str().to_string()));
    }

    if let Some(subject) = &query.subject {
        conditions.push("subject = ?");
        bindings.push(Value::Text(subject.clone()));
    }

    if let Some(actor) = &query.actor {
        conditions.push("actor = ?");
        bindings.push(Value::Text(actor.clone()));
    }

    if let Some(before) = query.before {
        conditions.push("id < ?");
        bindings.push(Value::Integer(before));
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    bindings.push(Value::Integer(query.limit as i64));

    (
        format!(
            "SELECT id, occurred_at, category, action, outcome, subject, actor, message, detail \
             FROM audit_log {where_clause} ORDER BY id DESC LIMIT ?"
        ),
        bindings,
    )
}

/// Maps a result row onto an [`AuditRecord`].
fn read_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditRecord> {
    let detail: Option<String> = row.get(8)?;

    Ok(AuditRecord {
        id: row.get(0)?,
        occurred_at: ts(row, 1)?,
        category: enum_col(row, 2, AuditCategory::parse)?,
        action: row.get(3)?,
        outcome: enum_col(row, 4, AuditOutcome::parse)?,
        subject: row.get(5)?,
        actor: row.get(6)?,
        message: row.get(7)?,
        // A detail we cannot parse is worth less than the entry around it, so it
        // is dropped rather than failing the whole query.
        detail: detail.and_then(|detail| serde_json::from_str(&detail).ok()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn log() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    fn entry(action: &'static str, outcome: AuditOutcome) -> AuditEntry {
        AuditEntry::new(AuditCategory::Authentication, action, outcome)
    }

    #[tokio::test]
    async fn an_entry_reads_back_with_everything_it_was_given() {
        let db = log().await;

        db.record(
            entry("login", AuditOutcome::Success)
                .subject("j.smith")
                .actor("admin")
                .message("Signed in from the admin UI.")
                .detail(serde_json::json!({ "credential_id": 4 })),
        )
        .await
        .unwrap();

        let records = db.audit(AuditQuery::recent(10)).await.unwrap();
        assert_eq!(records.len(), 1);

        let record = &records[0];
        assert_eq!(record.category, AuditCategory::Authentication);
        assert_eq!(record.action, "login");
        assert_eq!(record.outcome, AuditOutcome::Success);
        assert_eq!(record.subject.as_deref(), Some("j.smith"));
        assert_eq!(record.actor.as_deref(), Some("admin"));
        assert_eq!(record.detail, Some(serde_json::json!({"credential_id": 4})));
    }

    #[tokio::test]
    async fn entries_come_back_newest_first_and_page_by_id() {
        let db = log().await;

        for action in ["first", "second", "third"] {
            db.record(entry(action, AuditOutcome::Success))
                .await
                .unwrap();
        }

        let page = db.audit(AuditQuery::recent(2)).await.unwrap();
        assert_eq!(
            page.iter().map(|r| r.action.as_str()).collect::<Vec<_>>(),
            vec!["third", "second"]
        );

        let next = db
            .audit(AuditQuery::recent(2).before(page[1].id))
            .await
            .unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].action, "first");
    }

    #[tokio::test]
    async fn queries_filter_by_category_subject_and_actor() {
        let db = log().await;

        db.record(
            entry("login", AuditOutcome::Success)
                .subject("a")
                .actor("admin"),
        )
        .await
        .unwrap();
        db.record(
            AuditEntry::new(
                AuditCategory::Pki,
                "certificate.issued",
                AuditOutcome::Success,
            )
            .subject("b")
            .actor("system"),
        )
        .await
        .unwrap();

        assert_eq!(db.audit(AuditQuery::about("a", 10)).await.unwrap().len(), 1);
        assert_eq!(
            db.audit(AuditQuery::recent(10).in_category(AuditCategory::Pki))
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.audit(AuditQuery::recent(10).by("admin"))
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn pruning_applies_both_the_age_and_the_count_limit() {
        let db = log().await;

        for index in 0..5 {
            db.record(entry("login", AuditOutcome::Success).subject(index))
                .await
                .unwrap();
        }

        let removed = db
            .prune_audit_log(chrono::TimeDelta::days(30), 3)
            .await
            .unwrap();
        assert_eq!(removed, 2);
        assert_eq!(db.audit(AuditQuery::recent(10)).await.unwrap().len(), 3);

        let removed = db
            .prune_audit_log(chrono::TimeDelta::seconds(-1), 100)
            .await
            .unwrap();
        assert_eq!(removed, 3);
        assert!(db.audit(AuditQuery::recent(10)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_entry_written_by_a_newer_release_fails_the_read_loudly() {
        let db = log().await;

        db.write(|tx| {
            tx.execute(
                "INSERT INTO audit_log (occurred_at, category, action, outcome) \
                 VALUES ('2026-01-01T00:00:00.000Z', 'telepathy', 'read-mind', 'success')",
                [],
            )
        })
        .await
        .unwrap();

        assert!(db.audit(AuditQuery::recent(10)).await.is_err());
    }
}
