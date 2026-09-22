//! What a service's configuration may hold, and whether one candidate does.
//!
//! A service's configuration is a JSON object an administrator writes and the
//! service reads. Two things make that safe to edit from a form rather than a
//! text box:
//!
//! | What | Who supplies it | What it catches |
//! |---|---|---|
//! | A **JSON Schema** ([`ServiceDescriptor::config_schema`](crate::service::ServiceDescriptor::config_schema)) | The service, when it registers | Unknown keys, wrong types, values out of range — everything the UI can draw an input for |
//! | A **validation exchange** (this module) | The service, when asked | What a schema cannot say: an API key the upstream refuses, a path that is not there |
//!
//! # The exchange
//!
//! A sidecar dials the server and never the other way round, so the server
//! cannot simply call it. What it has is the server-event feed the sidecar
//! already holds open:
//!
//! 1. an administrator `POST`s a candidate to
//!    `/api/v1/services/<name>/config/validate`;
//! 2. the server checks it against the schema, and — for a service that
//!    advertises [`CONFIG_VALIDATE`] and has a feed open — publishes
//!    `service.config.validate` carrying a request id, to that service alone;
//! 3. the service `GET`s `/api/v1/services/<name>/config/validations/<id>` for
//!    the candidate, and `POST`s a [`ConfigValidation`] back to the same path;
//! 4. the administrator's request answers with a [`ConfigValidationReport`].
//!
//! The candidate travels by `GET` rather than on the feed because it may hold a
//! secret, and nothing secret crosses that bus — see [`crate::event`].
//!
//! No transport is not a failure: [`ServiceCheck`] says whether the service was
//! asked, and the schema's answer stands on its own when it was not.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The capability a service advertises when it answers validation requests.
pub const CONFIG_VALIDATE: &str = "config.validate";

/// One thing wrong with a candidate configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigIssue {
    /// Where in the document, as a JSON Pointer (`/area/lat`). Absent for a
    /// problem with the document as a whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// What is wrong, in words an administrator can act on.
    pub message: String,
}

impl ConfigIssue {
    /// A problem with the document as a whole.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            path: None,
            message: message.into(),
        }
    }

    /// A problem with the value at `path`, a JSON Pointer.
    pub fn at(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: Some(path.into()).filter(|path: &String| !path.is_empty()),
            message: message.into(),
        }
    }
}

/// What a service says about a candidate it was asked to check.
///
/// `{}` is "nothing wrong", so a service with nothing to add answers the
/// smallest possible body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigValidation {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<ConfigIssue>,
}

impl ConfigValidation {
    /// Nothing wrong.
    pub fn accepted() -> Self {
        Self::default()
    }

    /// Something wrong.
    pub fn rejected(issues: impl IntoIterator<Item = ConfigIssue>) -> Self {
        Self {
            issues: issues.into_iter().collect(),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.issues.is_empty()
    }
}

impl From<ConfigIssue> for ConfigValidation {
    fn from(issue: ConfigIssue) -> Self {
        Self::rejected([issue])
    }
}

/// A candidate a service has been asked to check, as it reads it back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigValidationRequest {
    pub id: Uuid,

    /// The candidate. Not yet stored, and possibly never.
    pub config: serde_json::Value,
}

/// Whether the service itself had a say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceCheck {
    /// It answered; its issues are in the report.
    Checked,

    /// It does not advertise [`CONFIG_VALIDATE`].
    NotSupported,

    /// It advertises it, but has no feed open or did not answer in time.
    Unreachable,

    /// The schema had already refused the candidate, so it was not asked.
    Skipped,
}

/// What an administrator is told about a candidate configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigValidationReport {
    /// Whether anything that was able to check it objected.
    pub valid: bool,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<ConfigIssue>,

    pub service: ServiceCheck,
}

impl ConfigValidationReport {
    /// A report over `issues`, valid exactly when there are none.
    pub fn new(issues: Vec<ConfigIssue>, service: ServiceCheck) -> Self {
        Self {
            valid: issues.is_empty(),
            issues,
            service,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_with_nothing_to_add_answers_an_empty_body() {
        let accepted: ConfigValidation = serde_json::from_str("{}").unwrap();

        assert!(accepted.is_valid());
        assert_eq!(serde_json::to_string(&accepted).unwrap(), "{}");
    }

    #[test]
    fn an_issue_names_where_it_is_only_when_it_is_somewhere() {
        for (issue, expected) in [
            (
                ConfigIssue::at("/map_key", "FIRMS refused this key."),
                r#"{"path":"/map_key","message":"FIRMS refused this key."}"#,
            ),
            (
                ConfigIssue::at("", "Not an object."),
                r#"{"message":"Not an object."}"#,
            ),
            (
                ConfigIssue::new("Not an object."),
                r#"{"message":"Not an object."}"#,
            ),
        ] {
            let json = serde_json::to_string(&issue).unwrap();

            assert_eq!(json, expected);
            assert_eq!(serde_json::from_str::<ConfigIssue>(&json).unwrap(), issue);
        }
    }

    #[test]
    fn a_report_is_valid_exactly_when_nothing_objected() {
        let clean = ConfigValidationReport::new(Vec::new(), ServiceCheck::NotSupported);
        let refused = ConfigValidationReport::new(
            vec![ConfigIssue::at("/area/lat", "Out of range.")],
            ServiceCheck::Skipped,
        );

        assert!(clean.valid);
        assert!(!refused.valid);

        let json = serde_json::to_string(&refused).unwrap();
        assert!(json.contains(r#""service":"skipped""#), "{json}");
        assert_eq!(
            serde_json::from_str::<ConfigValidationReport>(&json).unwrap(),
            refused
        );
    }
}
