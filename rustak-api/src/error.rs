//! The body every failing API response carries.

use serde::{Deserialize, Serialize};

/// What the server says when a request did not work.
///
/// One shape for every failure, so the browser has one thing to parse and one
/// place to render. `error` is written for the person reading it: a
/// `human_errors::Kind::User` message is passed through verbatim, while a
/// `Kind::System` one is replaced with something general, because the detail of
/// an internal failure belongs in the log rather than in a response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    /// The message to show.
    pub error: String,

    /// A stable token naming the failure, for the cases where the UI has to act
    /// on which failure it was rather than just display it.
    ///
    /// Absent unless a caller needs to branch on it. `invalid_grant` on the
    /// token endpoints and `csr_invalid` on enrolment are the ones TAK clients
    /// and CloudTAK look for by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ApiErrorBody {
    /// A failure with nothing for the caller to branch on.
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            code: None,
        }
    }

    /// Names the failure, so a caller can recognise it without matching on the
    /// text of the message.
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_body_round_trips_through_serde() {
        let body = ApiErrorBody::new("That username is already taken.").with_code("conflict");
        let json = serde_json::to_string(&body).unwrap();

        assert_eq!(
            json,
            r#"{"error":"That username is already taken.","code":"conflict"}"#
        );
        assert_eq!(serde_json::from_str::<ApiErrorBody>(&json).unwrap(), body);
    }

    #[test]
    fn a_body_without_a_code_carries_only_the_message() {
        // The UI has parsed `{"error": …}` since before codes existed, so the
        // field has to stay absent rather than serialise as null.
        let body = ApiErrorBody::new("Something went wrong.");

        assert_eq!(
            serde_json::to_string(&body).unwrap(),
            r#"{"error":"Something went wrong."}"#
        );
        assert_eq!(
            serde_json::from_str::<ApiErrorBody>(r#"{"error":"Something went wrong."}"#).unwrap(),
            body
        );
    }
}
