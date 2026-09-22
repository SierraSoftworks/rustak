//! A span per request, with the credentials taken out of it.
//!
//! Telemetry leaves the process. Everything in this file exists because a span
//! that carried an `Authorization` header or an OAuth `code` would put a live
//! credential into whatever collects it — and a trace is exactly the thing
//! people paste into a bug report. Names are kept and values are replaced, so
//! the shape of a request is still legible without any of it being usable.
//!
//! # A span is not a log line
//!
//! The span is always opened, because that is what an OTel collector samples
//! and what every event inside the request hangs off. What it is *not* is
//! something an operator reads: `level_for` decides that, and its answer for a
//! request that succeeded is nothing at all.
//!
//! That is deliberate and it is the whole point of this half of the file. A
//! rustak server with two idle sidecars attached should print **no lines at
//! all** at `LOG_LEVEL=info` — the sidecars hold one event feed and one
//! heartbeat each, all of them successful, and an operator reading a quiet log
//! should be able to take "quiet" at face value. The one that came before this
//! was retired partly for doing the opposite.
//!
//! A refusal is a `debug`: the caller has been told, the audit log has the ones
//! that matter, and the modules that make a security decision
//! (`auth::workload`, `plugins::auth`, `auth::resolve`) log the refusals whose
//! *reason* an operator needs at the level their own posture calls for. A `5xx`
//! is ours, and stays loud.

use std::pin::Pin;
use std::task::{Context, Poll};

use actix_web::dev::*;
use actix_web::http::header::HeaderMap;
use actix_web::{Error, web};
use futures::future::{Ready, ok};
use futures::{Future, FutureExt as _};
use opentelemetry::propagation::Extractor;
use tracing_batteries::prelude::tracing::Level;
use tracing_batteries::prelude::*;

use crate::services::Services;

/// Query parameters whose values are credentials or one-time secrets.
const SENSITIVE_QUERY: &[&str] = &[
    "code",
    "state",
    "id_token",
    "access_token",
    "refresh_token",
    "token",
    "setup_token",
    "registration_token",
    "client_secret",
    "code_verifier",
];

/// Headers whose values are credentials.
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
];

/// What replaces a value we will not record.
const REDACTED: &str = "REDACTED";

/// What one finished request is worth logging, given the status it answered.
///
/// A pure function so the decision can be asserted on directly: "which statuses
/// reach an operator's log" is a policy, and a policy buried in a `match` inside
/// a `map` inside a middleware is a policy nobody can check.
///
/// - **2xx and 3xx**: [`None`]. A request that worked is not news, and a fleet
///   of sidecars doing exactly what they are supposed to must not be a line a
///   second. The span still carries it for tracing.
/// - **4xx**: `DEBUG`. The caller already knows; the audit log keeps the ones
///   that matter; the security decision itself is logged where it is made.
/// - **5xx**: `WARN`. Ours, and rare — the handler that failed has usually
///   logged its own cause at `ERROR` and reported it to the session already,
///   so this is the line that says which request it was.
fn level_for(status: actix_web::http::StatusCode) -> Option<Level> {
    if status.is_server_error() {
        return Some(Level::WARN);
    }

    if status.is_client_error() {
        return Some(Level::DEBUG);
    }

    None
}

/// Records the status on the span, and logs it only if [`level_for`] says so.
fn finished(status: actix_web::http::StatusCode) {
    Span::current().record("http.status_code", display(status));

    // `tracing`'s level is part of the callsite, so the decision is made here
    // and the two callsites exist for it rather than being generated.
    match level_for(status) {
        None => {}
        Some(Level::DEBUG) => {
            debug!(status = status.as_u16(), "Refused a request.");
        }
        Some(_) => {
            warn!(status = status.as_u16(), "A request failed.");
        }
    }
}

/// The request target, with secret parameter values replaced.
fn redact_target(uri: &actix_web::http::Uri) -> String {
    let Some(query) = uri.query() else {
        return uri.path().to_string();
    };

    let redacted = query
        .split('&')
        .map(|pair| {
            let name = pair.split('=').next().unwrap_or(pair);

            if SENSITIVE_QUERY.iter().any(|p| name.eq_ignore_ascii_case(p)) {
                format!("{name}={REDACTED}")
            } else {
                pair.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&");

    format!("{}?{redacted}", uri.path())
}

/// The headers, with credential values replaced and every name kept.
fn redact_headers(headers: &HeaderMap) -> String {
    headers
        .iter()
        .map(|(name, value)| {
            if SENSITIVE_HEADERS
                .iter()
                .any(|h| name.as_str().eq_ignore_ascii_case(h))
            {
                format!("{name}: {REDACTED}")
            } else {
                format!("{name}: {value:?}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wraps every request in a span, and reports the failures to the session.
pub struct TracingLogger<S: Services + Clone + Send + Sync + 'static> {
    services: std::marker::PhantomData<S>,
}

impl<S: Services + Clone + Send + Sync + 'static> TracingLogger<S> {
    /// A fresh middleware.
    pub fn new() -> Self {
        Self {
            services: std::marker::PhantomData,
        }
    }
}

impl<S: Services + Clone + Send + Sync + 'static> Default for TracingLogger<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, A, B> Transform<A, ServiceRequest> for TracingLogger<S>
where
    A: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    A::Future: 'static,
    S: Services + Clone + Send + Sync + 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Transform = TracingLoggerMiddleware<S, A>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: A) -> Self::Future {
        ok(TracingLoggerMiddleware {
            service,
            services: std::marker::PhantomData,
        })
    }
}

/// The middleware [`TracingLogger`] builds.
#[doc(hidden)]
pub struct TracingLoggerMiddleware<S: Services + Clone + Send + Sync + 'static, A> {
    service: A,
    services: std::marker::PhantomData<S>,
}

impl<S: Services + Clone + Send + Sync + 'static, A, B> Service<ServiceRequest>
    for TracingLoggerMiddleware<S, A>
where
    A: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    A::Future: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, request: ServiceRequest) -> Self::Future {
        let user_agent = request
            .headers()
            .get("User-Agent")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();

        let target = redact_target(request.uri());
        let headers = redact_headers(request.headers());

        let span = info_span!(
            "request",
            "otel.kind" = "server",
            "otel.name" = request.match_pattern().unwrap_or_else(|| request.uri().path().to_string()),
            "net.transport" = "IP.TCP",
            "net.peer.ip" = %request.connection_info().realip_remote_addr().unwrap_or(""),
            "http.target" = %target,
            "http.user_agent" = %user_agent,
            "http.status_code" = EmptyField,
            "http.method" = %request.method(),
            "http.url" = %request.match_pattern().unwrap_or_else(|| request.path().into()),
            "http.headers" = %headers,
        );

        let parent = opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.extract(&Headers::from(request.headers()))
        });
        let _ = span.set_parent(parent);

        let services = request.app_data::<web::Data<S>>().cloned();

        Box::pin(
            self.service
                .call(request)
                .map(move |outcome| {
                    match &outcome {
                        Ok(response) => finished(response.response().status()),
                        Err(error) => {
                            if let Some(services) = services {
                                services.session().record_custom_error(
                                    tracing_batteries::ErrorInfo::new(&error)
                                        .with_metadata("http.target", target)
                                        .with_metadata(
                                            "http.status_code",
                                            error
                                                .as_response_error()
                                                .status_code()
                                                .as_u16()
                                                .to_string(),
                                        ),
                                );
                            }

                            finished(error.as_response_error().status_code());
                        }
                    }

                    outcome
                })
                .instrument(span),
        )
    }
}

/// Reads trace context out of the request's headers.
struct Headers<'a> {
    headers: &'a HeaderMap,
}

impl<'a> From<&'a HeaderMap> for Headers<'a> {
    fn from(headers: &'a HeaderMap) -> Self {
        Self { headers }
    }
}

impl<'a> Extractor for Headers<'a> {
    fn get(&self, key: &str) -> Option<&'a str> {
        self.headers.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.headers.keys().map(|key| key.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::header::{HeaderName, HeaderValue};
    use actix_web::http::{StatusCode, Uri};

    use super::*;

    #[test]
    fn a_request_that_worked_is_not_something_an_operator_has_to_read() {
        // The target this exists for: a server with two idle sidecars attached
        // prints nothing per minute. Each sidecar holds one event feed open and
        // posts one heartbeat a tick, and every one of those is a 2xx.
        for status in [200u16, 201, 204, 206, 301, 302, 304] {
            assert_eq!(
                level_for(StatusCode::from_u16(status).unwrap()),
                None,
                "{status} is routine",
            );
        }
    }

    #[test]
    fn a_refusal_is_a_debug_line_rather_than_an_info_one() {
        // The caller has already been told. The refusals whose *reason* an
        // operator needs are logged where the decision is made — `auth::
        // workload`, `plugins::auth`, `auth::resolve` — and the audit log is a
        // separate record that this does not touch.
        for status in [400u16, 401, 403, 404, 409, 429] {
            assert_eq!(
                level_for(StatusCode::from_u16(status).unwrap()),
                Some(Level::DEBUG),
                "{status}",
            );
        }
    }

    #[test]
    fn a_server_error_stays_loud() {
        for status in [500u16, 502, 503] {
            assert_eq!(
                level_for(StatusCode::from_u16(status).unwrap()),
                Some(Level::WARN),
                "{status}",
            );
        }
    }

    #[test]
    fn a_one_time_code_never_reaches_a_span() {
        let uri: Uri = "/auth/callback?code=the-code&state=the-state&demo"
            .parse()
            .unwrap();

        assert_eq!(
            redact_target(&uri),
            "/auth/callback?code=REDACTED&state=REDACTED&demo"
        );
    }

    #[test]
    fn the_shape_of_a_request_survives_the_redaction() {
        let uri: Uri = "/api/v1/audit?limit=100&category=auth".parse().unwrap();
        assert_eq!(redact_target(&uri), "/api/v1/audit?limit=100&category=auth");

        let uri: Uri = "/api/v1/health".parse().unwrap();
        assert_eq!(redact_target(&uri), "/api/v1/health");
    }

    #[test]
    fn the_setup_token_is_treated_as_the_credential_it_is() {
        let uri: Uri = "/api/v1/setup/admin?setup_token=abcdef".parse().unwrap();

        assert_eq!(
            redact_target(&uri),
            "/api/v1/setup/admin?setup_token=REDACTED"
        );
    }

    #[test]
    fn header_names_are_kept_and_credential_values_are_not() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer a-real-token"),
        );
        headers.insert(
            HeaderName::from_static("x-request-id"),
            HeaderValue::from_static("visible"),
        );

        let rendered = redact_headers(&headers);

        assert!(rendered.contains("authorization: REDACTED"));
        assert!(!rendered.contains("a-real-token"));
        assert!(rendered.contains("x-request-id: \"visible\""));
    }
}
