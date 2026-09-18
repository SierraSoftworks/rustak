//! One shape for every failure, and one content type for every response.
//!
//! The body is always `{"error": …}`, optionally with a `code` a caller can
//! branch on. The UI has parsed that shape since before there were codes, and
//! CloudTAK looks for `invalid_grant` and `csr_invalid` by name, so neither the
//! field nor its absence is ours to change.
//!
//! # Why the content type is written out
//!
//! `application/json`, with no `charset` parameter. CloudTAK compares the
//! header, and actix's own `.json()` would add `; charset=utf-8` on some paths.
//! Every response in the admin API therefore goes through [`json_ok`] or
//! [`json_error`] rather than building its own.
//!
//! # Why a system error is generalised
//!
//! A `Kind::System` failure is ours, and its description names our internals —
//! a table, a file path, a provider's hostname. It is logged in full and the
//! caller is told that something went wrong, which is all they can act on
//! anyway.

use actix_web::http::StatusCode;
use actix_web::http::header::{CONTENT_TYPE, HeaderValue};
use actix_web::{HttpResponse, ResponseError};
use rustak_api::ApiErrorBody;
use rustak_core::prelude::*;

/// The exact content type every JSON response carries.
pub const JSON: HeaderValue = HeaderValue::from_static("application/json");

/// What a caller is told when the failure was ours.
const SYSTEM_MESSAGE: &str = "Something went wrong on the server. Please try again.";

/// A JSON body with the exact content type.
pub fn json_ok<T: Serialize>(value: &T) -> HttpResponse {
    render(StatusCode::OK, value)
}

/// A JSON body with a status of the caller's choosing.
pub fn json_with<T: Serialize>(status: StatusCode, value: &T) -> HttpResponse {
    render(status, value)
}

/// The `{"error": …}` body.
pub fn json_error(status: StatusCode, message: impl Into<String>) -> HttpResponse {
    render(status, &ApiErrorBody::new(message))
}

/// The `{"error": …, "code": …}` body, for the failures a caller branches on.
pub fn json_error_code(
    status: StatusCode,
    message: impl Into<String>,
    code: impl Into<String>,
) -> HttpResponse {
    render(status, &ApiErrorBody::new(message).with_code(code))
}

/// Serialises, or reports the serialisation failure as itself.
fn render<T: Serialize>(status: StatusCode, value: &T) -> HttpResponse {
    match serde_json::to_vec(value) {
        Ok(body) => HttpResponse::build(status)
            .insert_header((CONTENT_TYPE, JSON))
            .body(body),
        Err(err) => {
            error!(error = %err, "Could not serialise an API response.");

            HttpResponse::build(StatusCode::INTERNAL_SERVER_ERROR)
                .insert_header((CONTENT_TYPE, JSON))
                .body(
                    br#"{"error":"Something went wrong on the server. Please try again."}"#
                        .as_slice(),
                )
        }
    }
}

/// A failure a handler returns.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
    code: Option<String>,
}

impl ApiError {
    /// A failure with a status and a message the caller may be shown.
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            code: None,
        }
    }

    /// Names the failure so a caller can recognise it without matching on text.
    #[must_use]
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    /// `400`, for a request we understood and will not act on.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// `401`, for a request carrying no usable credential.
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    /// `403`, for a credential that will never be enough.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    /// `404`, for something that is not here.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    /// `409`, for something that has already been done.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    /// `410`, which is what the setup routes answer once the wizard is done.
    pub fn gone(message: impl Into<String>) -> Self {
        Self::new(StatusCode::GONE, message)
    }

    /// `500`, generalised, for a failure that is ours.
    pub fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, SYSTEM_MESSAGE)
    }

    /// Turns one of our own errors into a response.
    ///
    /// A user error is shown; a system error is logged and generalised. The
    /// distinction is the whole reason `human-errors` separates them.
    pub fn from_human(err: &Error) -> Self {
        if err.is(human_errors::Kind::User) {
            return Self::bad_request(err.description());
        }

        error!(error = %err, "An API request failed.");

        Self::internal()
    }

    /// The status this failure will answer with.
    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        self.status
    }

    fn error_response(&self) -> HttpResponse {
        match &self.code {
            Some(code) => json_error_code(self.status, self.message.clone(), code.clone()),
            None => json_error(self.status, self.message.clone()),
        }
    }
}

impl From<Error> for ApiError {
    fn from(err: Error) -> Self {
        Self::from_human(&err)
    }
}

/// What a handler returns.
pub type ApiResult<T = HttpResponse> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use actix_web::body::MessageBody as _;

    use super::*;

    fn body_of(response: HttpResponse) -> serde_json::Value {
        let bytes = response.into_body().try_into_bytes().unwrap();

        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn every_response_says_application_json_and_nothing_else() {
        // CloudTAK compares this header, so a charset parameter is a bug even
        // though it would be correct.
        for response in [
            json_ok(&serde_json::json!({ "ok": true })),
            json_error(StatusCode::NOT_FOUND, "gone"),
            ApiError::internal().error_response(),
        ] {
            assert_eq!(
                response.headers().get(CONTENT_TYPE).unwrap(),
                "application/json",
            );
        }
    }

    #[test]
    fn a_failure_is_the_shape_the_ui_has_always_parsed() {
        let body = body_of(json_error(StatusCode::FORBIDDEN, "Not for you."));

        assert_eq!(body, serde_json::json!({ "error": "Not for you." }));
    }

    #[test]
    fn a_code_is_only_present_when_somebody_branches_on_it() {
        let body = body_of(
            ApiError::bad_request("Bad credentials")
                .with_code("invalid_grant")
                .error_response(),
        );

        assert_eq!(
            body,
            serde_json::json!({ "error": "Bad credentials", "code": "invalid_grant" })
        );
    }

    #[test]
    fn a_user_error_is_shown_and_a_system_error_is_not() {
        let shown = ApiError::from_human(&human_errors::user(
            "That host name is not one we can use.",
            &[],
        ));
        assert_eq!(shown.status(), StatusCode::BAD_REQUEST);
        assert_eq!(shown.to_string(), "That host name is not one we can use.");

        let hidden = ApiError::from_human(&human_errors::system(
            "The users table is missing a column named display_name.",
            &[],
        ));
        assert_eq!(hidden.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            !hidden.to_string().contains("display_name"),
            "our internals are logged, not handed to whoever asked",
        );
    }

    #[test]
    fn the_statuses_are_the_ones_the_endpoints_promise() {
        assert_eq!(
            ApiError::gone("The setup wizard has been completed.").status(),
            StatusCode::GONE
        );
        assert_eq!(ApiError::conflict("already").status(), StatusCode::CONFLICT);
        assert_eq!(
            ApiError::unauthorized("no").status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(ApiError::forbidden("no").status(), StatusCode::FORBIDDEN);
    }
}
