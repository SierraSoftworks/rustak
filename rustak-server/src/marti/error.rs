//! What a refused Marti request looks like on the wire.
//!
//! One shape — `{"status", "code", "message"}`, all three always present — with
//! a status string that is the *name* of the Java `HttpStatus` enum constant
//! rather than the number or the reason phrase, because that is what TAK
//! Server's own error body carries and what a client that branches on it has
//! learned to read.
//!
//! # Why a redirect is never one of the answers
//!
//! node-tak treats any status from 200 to 399 as success and parses the
//! redirect's body — which is empty — as the payload. A missing trailing slash
//! or an unsupported method therefore has to be a `404`/`405` with a JSON body,
//! not a `301` to the canonical path. Nothing in the Marti scope installs
//! actix's `NormalizePath` middleware, and [`crate::marti`]'s default
//! service turns every unmatched path and method into one of these.
//!
//! # The two HTML documents
//!
//! A handful of the legacy servlets — `/Marti/sync/*`, `/Marti/GetTime`,
//! `/Marti/ErrorLog` — are served by the container rather than by the
//! application in a real TAK Server, so their failures arrive as HTML error
//! pages. node-tak sniffs for HTML and wraps those with a parsed summary, so
//! emitting one from those paths is the behaviour its error handling was
//! written for. They are written here in our own words, carrying the titles a
//! client's sniffer keys on.

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, ResponseError};

use crate::prelude::*;

use super::response;

/// The `404` document the legacy servlet paths answer with.
pub const HTML_404: &str = concat!(
    "<!DOCTYPE html><html lang=\"en\"><head>",
    "<title>404 TAK Server resource not found</title></head>",
    "<body><h1>404 TAK Server resource not found</h1>",
    "<p>The requested resource is not available on this server.</p>",
    "</body></html>",
);

/// The `503` document the legacy servlet paths answer with.
pub const HTML_UNAVAILABLE: &str = concat!(
    "<!DOCTYPE html><html lang=\"en\"><head>",
    "<title>503 TAK Server temporarily unavailable</title></head>",
    "<body><h1>503 TAK Server temporarily unavailable</h1>",
    "<p>The server could not answer this request. Please try again.</p>",
    "</body></html>",
);

/// The body of a refused Marti request.
///
/// Every field is always serialised, including an empty `message`: a client
/// reading `body.message` must find a string rather than `undefined`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// The name of the Java `HttpStatus` constant, such as `NOT_FOUND`.
    pub status: String,
    /// TAK Server's own numbering, preserved so a client can branch on it.
    pub code: u16,
    /// The prefixed description; see [`MartiError::message`].
    pub message: String,
}

/// Why a Marti request was refused.
#[derive(Debug, Clone)]
pub enum MartiError {
    /// Nothing is addressed by that path, name or identifier.
    NotFound(String),
    /// The request was understood and will not be acted on.
    InvalidRequest(String),
    /// A path that exists and does not serve this method.
    ///
    /// TAK Server leaves these to its servlet container, which answers HTML;
    /// we answer the same JSON shape as everything else, because the one thing
    /// that must not happen is a redirect to a spelling that does serve it.
    MethodNotAllowed(String),
    /// A body we could not parse at all.
    InvalidBody,
    /// Something with that name or identifier already exists.
    Duplicate(String),
    /// The request was well formed and its contents are not acceptable.
    Validation(String),
    /// A failure of ours.
    Internal(String),
    /// The thing addressed was here and has been deleted.
    Gone(String),
    /// No usable credential was presented.
    Unauthorized(String),
    /// The credential was good and is not enough.
    Forbidden(String),
    /// A route that exists and does nothing yet.
    NotImplemented(&'static str),
    /// The legacy servlets' HTML `404`.
    Html404,
    /// The legacy servlets' HTML `503`.
    HtmlUnavailable,
}

impl MartiError {
    /// The status this failure answers with.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) | Self::Html404 => StatusCode::NOT_FOUND,
            Self::InvalidRequest(_) | Self::InvalidBody | Self::Validation(_) => {
                StatusCode::BAD_REQUEST
            }
            Self::MethodNotAllowed(_) => StatusCode::METHOD_NOT_ALLOWED,
            Self::Duplicate(_) => StatusCode::CONFLICT,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Gone(_) => StatusCode::GONE,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
            Self::HtmlUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// TAK Server's own error numbering.
    ///
    /// Not the HTTP status and not sequential: `3` is "not implemented" while
    /// `4` is "duplicate", which is an artefact of the order the cases were
    /// added upstream. Reproduced rather than renumbered.
    pub fn code(&self) -> u16 {
        match self {
            Self::NotFound(_) | Self::Html404 => 1,
            Self::InvalidRequest(_) | Self::InvalidBody | Self::MethodNotAllowed(_) => 2,
            Self::NotImplemented(_) => 3,
            Self::Duplicate(_) => 4,
            Self::Validation(_) => 5,
            Self::Internal(_) | Self::HtmlUnavailable => 6,
            Self::Gone(_) => 8,
            Self::Unauthorized(_) => 9,
            Self::Forbidden(_) => 10,
        }
    }

    /// The message body, prefix included.
    ///
    /// The prefixes — and the space before the colon in the duplicate case, and
    /// the bare leading colon in several others — are TAK Server's, and a
    /// client that matches on them would stop recognising a tidied version.
    pub fn message(&self) -> String {
        match self {
            Self::NotFound(what) => format!("Not Found: {what}"),
            Self::InvalidRequest(what) | Self::MethodNotAllowed(what) => {
                format!("Invalid Request: {what}")
            }
            Self::InvalidBody => "Invalid Request Body".to_string(),
            Self::Duplicate(what) => format!("Duplicate Exception : {what}"),
            Self::Validation(what) | Self::Gone(what) => format!(": {what}"),
            Self::Unauthorized(what) | Self::Forbidden(what) => format!(": {what}"),
            // Deliberately empty: a `500` names our internals, which are logged
            // and not handed to whoever asked.
            Self::Internal(_) => String::new(),
            Self::NotImplemented(what) => format!("{what} is not implemented"),
            Self::Html404 | Self::HtmlUnavailable => String::new(),
        }
    }

    /// The name of the Java `HttpStatus` constant for this failure's status.
    pub fn status_name(&self) -> String {
        self.status()
            .canonical_reason()
            .unwrap_or("INTERNAL_SERVER_ERROR")
            .to_uppercase()
            .replace([' ', '-'], "_")
    }

    /// Whether this failure is one of the two HTML documents.
    fn is_html(&self) -> bool {
        matches!(self, Self::Html404 | Self::HtmlUnavailable)
    }

    /// The JSON body, for the endpoints that answer with one.
    pub fn body(&self) -> ErrorResponse {
        ErrorResponse {
            status: self.status_name(),
            code: self.code(),
            message: self.message(),
        }
    }
}

impl std::fmt::Display for MartiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl ResponseError for MartiError {
    fn status_code(&self) -> StatusCode {
        self.status()
    }

    fn error_response(&self) -> HttpResponse {
        if self.is_html() {
            let document = match self {
                Self::HtmlUnavailable => HTML_UNAVAILABLE,
                _ => HTML_404,
            };

            return response::html(self.status(), document);
        }

        response::bare_json_with(self.status(), &self.body())
    }
}

impl From<Error> for MartiError {
    /// A user error is shown; one of ours is logged and generalised.
    fn from(err: Error) -> Self {
        if err.is(human_errors::Kind::User) {
            return Self::InvalidRequest(err.description());
        }

        error!(error = %err, "A Marti request failed.");

        Self::Internal(err.description())
    }
}

impl From<serde_json::Error> for MartiError {
    fn from(err: serde_json::Error) -> Self {
        debug!(error = %err, "Refused a Marti request body we could not parse.");

        Self::InvalidBody
    }
}

/// What a handler returns.
pub type MartiResult<T = HttpResponse> = Result<T, MartiError>;

#[cfg(test)]
mod tests {
    use actix_web::body::MessageBody as _;
    use actix_web::http::header::CONTENT_TYPE;

    use super::*;

    fn body_of(response: HttpResponse) -> ErrorResponse {
        let bytes = response.into_body().try_into_bytes().unwrap();

        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn the_mapping_table_is_the_one_tak_server_emits() {
        let cases: &[(MartiError, u16, u16, &str, &str)] = &[
            (
                MartiError::NotFound("mission Alpha".to_string()),
                404,
                1,
                "NOT_FOUND",
                "Not Found: mission Alpha",
            ),
            (
                MartiError::InvalidRequest("secAgo".to_string()),
                400,
                2,
                "BAD_REQUEST",
                "Invalid Request: secAgo",
            ),
            (
                MartiError::InvalidBody,
                400,
                2,
                "BAD_REQUEST",
                "Invalid Request Body",
            ),
            (
                MartiError::NotImplemented("KML export"),
                501,
                3,
                "NOT_IMPLEMENTED",
                "KML export is not implemented",
            ),
            (
                MartiError::MethodNotAllowed("POST".to_string()),
                405,
                2,
                "METHOD_NOT_ALLOWED",
                "Invalid Request: POST",
            ),
            (
                MartiError::Duplicate("Alpha".to_string()),
                409,
                4,
                "CONFLICT",
                "Duplicate Exception : Alpha",
            ),
            (
                MartiError::Validation("name".to_string()),
                400,
                5,
                "BAD_REQUEST",
                ": name",
            ),
            (
                MartiError::Internal("the users table".to_string()),
                500,
                6,
                "INTERNAL_SERVER_ERROR",
                "",
            ),
            (
                MartiError::Gone("Alpha".to_string()),
                410,
                8,
                "GONE",
                ": Alpha",
            ),
            (
                MartiError::Unauthorized("no credential".to_string()),
                401,
                9,
                "UNAUTHORIZED",
                ": no credential",
            ),
            (
                MartiError::Forbidden("channel Blue".to_string()),
                403,
                10,
                "FORBIDDEN",
                ": channel Blue",
            ),
        ];

        for (error, status, code, name, message) in cases {
            assert_eq!(error.status().as_u16(), *status, "{error:?}");
            assert_eq!(error.code(), *code, "{error:?}");
            assert_eq!(error.status_name(), *name, "{error:?}");
            assert_eq!(error.message(), *message, "{error:?}");
        }
    }

    #[test]
    fn a_json_failure_carries_all_three_fields_and_the_exact_content_type() {
        // A client reading `body.message` must find a string, even when the
        // server had nothing to say.
        let response = MartiError::Internal("a table".to_string()).error_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json",
        );

        let body = body_of(response);
        assert_eq!(body.status, "INTERNAL_SERVER_ERROR");
        assert_eq!(body.code, 6);
        assert_eq!(body.message, "");
    }

    #[test]
    fn a_system_error_is_logged_rather_than_described_to_the_caller() {
        let hidden = MartiError::from(human_errors::system(
            "The missions table is missing a column named base_layer.",
            &[],
        ));

        assert_eq!(hidden.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            hidden.message(),
            "",
            "our internals are logged, not handed to whoever asked",
        );
    }

    #[test]
    fn a_user_error_is_shown_because_the_caller_can_act_on_it() {
        let shown = MartiError::from(human_errors::user("That channel does not exist.", &[]));

        assert_eq!(shown.status(), StatusCode::BAD_REQUEST);
        assert!(shown.message().contains("That channel does not exist."));
    }

    #[test]
    fn the_legacy_paths_answer_with_html_a_sniffer_recognises() {
        for (error, status, title) in [
            (
                MartiError::Html404,
                StatusCode::NOT_FOUND,
                "404 TAK Server resource not found",
            ),
            (
                MartiError::HtmlUnavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "503 TAK Server temporarily unavailable",
            ),
        ] {
            let response = error.error_response();

            assert_eq!(response.status(), status);
            assert_eq!(response.headers().get(CONTENT_TYPE).unwrap(), "text/html");

            let body =
                String::from_utf8(response.into_body().try_into_bytes().unwrap().to_vec()).unwrap();

            assert!(body.contains(&format!("<title>{title}</title>")), "{body}");
        }
    }

    #[test]
    fn nothing_here_can_answer_with_a_redirect() {
        // node-tak treats 3xx as success and parses the (empty) redirect body,
        // so a redirect is a silent failure rather than a loud one.
        let every = [
            MartiError::NotFound(String::new()),
            MartiError::InvalidRequest(String::new()),
            MartiError::InvalidBody,
            MartiError::Duplicate(String::new()),
            MartiError::Validation(String::new()),
            MartiError::Internal(String::new()),
            MartiError::Gone(String::new()),
            MartiError::Unauthorized(String::new()),
            MartiError::Forbidden(String::new()),
            MartiError::NotImplemented("x"),
            MartiError::MethodNotAllowed(String::new()),
            MartiError::Html404,
            MartiError::HtmlUnavailable,
        ];

        for error in every {
            assert!(!error.status().is_redirection(), "{error:?}");
        }
    }

    #[test]
    fn a_body_we_could_not_parse_is_the_invalid_body_case() {
        let err: MartiError = serde_json::from_str::<serde_json::Value>("{oops")
            .unwrap_err()
            .into();

        assert!(matches!(err, MartiError::InvalidBody));
        assert_eq!(err.message(), "Invalid Request Body");
    }
}
