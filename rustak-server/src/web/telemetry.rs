//! A span per request, with the credentials taken out of it.
//!
//! Telemetry leaves the process. Everything in this file exists because a span
//! that carried an `Authorization` header or an OAuth `code` would put a live
//! credential into whatever collects it — and a trace is exactly the thing
//! people paste into a bug report. Names are kept and values are replaced, so
//! the shape of a request is still legible without any of it being usable.

use std::pin::Pin;
use std::task::{Context, Poll};

use actix_web::dev::*;
use actix_web::http::header::HeaderMap;
use actix_web::{Error, web};
use futures::future::{Ready, ok};
use futures::{Future, FutureExt as _};
use opentelemetry::propagation::Extractor;
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
                        Ok(response) => {
                            Span::current()
                                .record("http.status_code", display(response.response().status()));
                        }
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

                            Span::current().record(
                                "http.status_code",
                                display(error.as_response_error().status_code()),
                            );
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
    use actix_web::http::Uri;
    use actix_web::http::header::{HeaderName, HeaderValue};

    use super::*;

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
