//! The audit log's wire contract.
//!
//! The log is written by the server and read by the admin UI, so its vocabulary
//! lives here rather than beside the storage that holds it — a second
//! definition of these values would be a second place for them to drift.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The area of the system an entry concerns.
///
/// Coarse on purpose: a category is what somebody filters the log by while
/// looking for something, and the detail of what happened is in
/// [`AuditRecord::action`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditCategory {
    /// Somebody signed in, refreshed a token, or was refused.
    Authentication,

    /// A client enrolled: a token was spent, a certificate was issued.
    Enrollment,

    /// An administrator changed users, channels or settings.
    Administration,

    /// The certificate authority: keys, issuance, revocation, ACME orders.
    Pki,

    /// The CoT stream: connections, subscriptions, disconnections.
    Stream,

    /// A mission was created, changed, subscribed to or deleted.
    Mission,

    /// A data package or file was uploaded, downloaded or removed.
    Package,

    /// An enrolment or configuration profile was delivered or changed.
    Profile,

    /// A sidecar registered, reported its health, or went away.
    Service,

    /// The server itself: start, shutdown, migrations, scheduled work.
    System,
}

impl AuditCategory {
    /// Every category, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[
        Self::Authentication,
        Self::Enrollment,
        Self::Administration,
        Self::Pki,
        Self::Stream,
        Self::Mission,
        Self::Package,
        Self::Profile,
        Self::Service,
        Self::System,
    ];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Authentication => "authentication",
            Self::Enrollment => "enrollment",
            Self::Administration => "administration",
            Self::Pki => "pki",
            Self::Stream => "stream",
            Self::Mission => "mission",
            Self::Package => "package",
            Self::Profile => "profile",
            Self::Service => "service",
            Self::System => "system",
        }
    }

    /// A short phrase naming the category for somebody reading the log.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Authentication => "Sign-in",
            Self::Enrollment => "Enrolment",
            Self::Administration => "Administration",
            Self::Pki => "Certificates",
            Self::Stream => "Stream",
            Self::Mission => "Mission",
            Self::Package => "Data package",
            Self::Profile => "Profile",
            Self::Service => "Service",
            Self::System => "System",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|category| category.as_str() == value)
    }
}

/// How an audited operation turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditOutcome {
    /// The operation did what it set out to do.
    Success,

    /// The operation was attempted and did not work.
    Failure,

    /// The operation was deliberately not performed, because there was nothing
    /// to do or a filter excluded it.
    Skipped,

    /// The operation was refused: a bad credential, or a permission check.
    Denied,
}

impl AuditOutcome {
    /// Every outcome, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[Self::Success, Self::Failure, Self::Skipped, Self::Denied];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Skipped => "skipped",
            Self::Denied => "denied",
        }
    }

    /// A short phrase naming the outcome for somebody reading the log.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Success => "Succeeded",
            Self::Failure => "Failed",
            Self::Skipped => "Skipped",
            Self::Denied => "Refused",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|outcome| outcome.as_str() == value)
    }

    /// Whether this outcome is worth drawing attention to.
    pub fn is_concerning(&self) -> bool {
        matches!(self, Self::Failure | Self::Denied)
    }
}

/// An entry read back from the log.
///
/// Nothing secret goes in here. The log is rendered in a browser and can be
/// exported, so a token, a password or key material must never reach
/// [`AuditRecord::detail`] — what belongs there is the identifier of the
/// credential that was used, not the credential.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Monotonic identifier, which is also the pagination cursor.
    pub id: i64,

    pub occurred_at: DateTime<Utc>,
    pub category: AuditCategory,

    /// What happened, as a dotted token: `login`, `certificate.issued`,
    /// `credential.revoked`.
    pub action: String,

    pub outcome: AuditOutcome,

    /// The thing acted upon: a username, a device identifier, a channel name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,

    /// The person or service responsible, where there was one. Absent for work
    /// the server did on its own initiative, which is how a scheduled job is
    /// told apart from something somebody asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Structured context, rendered as-is in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips_through_serde() {
        let record = AuditRecord {
            id: 17,
            occurred_at: "2026-09-18T12:00:00.500Z".parse().unwrap(),
            category: AuditCategory::Enrollment,
            action: "certificate.issued".into(),
            outcome: AuditOutcome::Success,
            subject: Some("alice".into()),
            actor: Some("alice".into()),
            message: Some("Enrolled ANDROID-358240051111110.".into()),
            detail: Some(serde_json::json!({ "credential_id": 4 })),
        };

        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(serde_json::from_str::<AuditRecord>(&json).unwrap(), record);
    }

    #[test]
    fn work_nobody_asked_for_has_no_actor() {
        let record: AuditRecord = serde_json::from_value(serde_json::json!({
            "id": 1,
            "occurred_at": "2026-09-18T12:00:00.500Z",
            "category": "system",
            "action": "startup",
            "outcome": "success",
        }))
        .unwrap();

        assert_eq!(record.actor, None);
        assert_eq!(record.subject, None);
        assert_eq!(record.detail, None);

        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(
            json.as_object().unwrap().keys().count(),
            5,
            "absent context should stay absent rather than serialise as null"
        );
    }

    #[test]
    fn categories_and_outcomes_round_trip_through_their_wire_form() {
        for category in AuditCategory::ALL.iter().copied() {
            let json = serde_json::to_string(&category).unwrap();

            assert_eq!(json, format!("\"{}\"", category.as_str()));
            assert_eq!(
                serde_json::from_str::<AuditCategory>(&json).unwrap(),
                category
            );
            assert_eq!(AuditCategory::parse(category.as_str()), Some(category));
            assert!(!category.label().is_empty());
        }

        for outcome in AuditOutcome::ALL.iter().copied() {
            let json = serde_json::to_string(&outcome).unwrap();

            assert_eq!(json, format!("\"{}\"", outcome.as_str()));
            assert_eq!(
                serde_json::from_str::<AuditOutcome>(&json).unwrap(),
                outcome
            );
            assert_eq!(AuditOutcome::parse(outcome.as_str()), Some(outcome));
        }

        assert_eq!(AuditCategory::parse("nonsense"), None);
        assert!(AuditOutcome::Denied.is_concerning());
        assert!(!AuditOutcome::Success.is_concerning());
    }
}
