//! The headers every Marti response carries, and the ones it must never.
//!
//! # What this middleware adds
//!
//! `Strict-Transport-Security` when the request arrived over TLS, and the
//! wildcard CORS trio when `[marti] allow_all_origins` says so. Nothing else:
//! the content type is the response builder's business, and the cache headers
//! belong to the single endpoint that needs them.
//!
//! # What it makes sure is absent
//!
//! A `3xx`. node-tak treats every status below 400 as success and parses the
//! redirect's (empty) body as the payload, so a redirect is a client failure
//! that looks like a server success. Two things follow, and both are here
//! rather than implicit:
//!
//! * actix's `NormalizePath` middleware is **not** installed anywhere in the
//!   Marti scope, so `/Marti/api/version/` is a `404` rather than a `301` to
//!   the path without the slash.
//! * `OPTIONS` is answered here — `204` with the CORS headers when they are
//!   configured, `405` otherwise — rather than falling through to a handler
//!   that might redirect to a canonical spelling.
//!
//! `assert_no_redirect` is the belt to those braces: it runs in every build
//! and turns a redirect somebody adds later into a `500` and a loud log line,
//! which is a visible failure rather than a client silently reading nothing.

use actix_web::ResponseError as _;
use actix_web::body::BoxBody;
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::http::Method;
use actix_web::http::header::{HeaderName, HeaderValue};
use actix_web::middleware::Next;
use actix_web::web;

use crate::prelude::*;

use super::error::MartiError;
use super::response;

/// Two years, with subdomains, which is what a preload list requires.
const HSTS: HeaderValue = HeaderValue::from_static("max-age=63072000; includeSubDomains");

/// The wildcard every one of the three CORS headers carries.
const ANY: HeaderValue = HeaderValue::from_static("*");

/// The `api-version` response header `/Marti/sync/content` carries.
const API_VERSION_RESPONSE: HeaderValue = HeaderValue::from_static("3");

/// `Access-Control-Allow-Headers`, which actix has no constant for.
const ALLOW_HEADERS: HeaderName = HeaderName::from_static("access-control-allow-headers");

/// `Access-Control-Allow-Methods`.
const ALLOW_METHODS: HeaderName = HeaderName::from_static("access-control-allow-methods");

/// `Access-Control-Allow-Origin`.
const ALLOW_ORIGIN: HeaderName = HeaderName::from_static("access-control-allow-origin");

/// The `api-version` response header name.
const API_VERSION: HeaderName = HeaderName::from_static("api-version");

/// Adds the transport and CORS headers, and answers preflights.
///
/// # Errors
///
/// Only what the wrapped handler returned; this middleware refuses nothing of
/// its own beyond an unconfigured `OPTIONS`, which it answers rather than
/// failing.
pub async fn marti_headers(
    request: ServiceRequest,
    next: Next<BoxBody>,
) -> Result<ServiceResponse<BoxBody>, actix_web::Error> {
    let allow_origins = request
        .app_data::<web::Data<AppContext>>()
        .is_some_and(|context| context.config().marti.allow_all_origins);
    let secure = request.app_config().secure();

    // The node id goes in every envelope, so it is read back before the first
    // response is built rather than on each one.
    if let Some(context) = request.app_data::<web::Data<AppContext>>().cloned() {
        response::ensure_node_id(context.get_ref()).await;
    }

    if request.method() == Method::OPTIONS {
        let response = if allow_origins {
            actix_web::HttpResponse::NoContent().finish()
        } else {
            MartiError::MethodNotAllowed("OPTIONS".to_string()).error_response()
        };

        return Ok(decorate(
            request.into_response(response),
            secure,
            allow_origins,
        ));
    }

    let response = next.call(request).await?;

    Ok(decorate(response, secure, allow_origins))
}

/// Adds the transport and CORS headers to a response that is otherwise built.
fn decorate(
    mut response: ServiceResponse<BoxBody>,
    secure: bool,
    allow_origins: bool,
) -> ServiceResponse<BoxBody> {
    assert_no_redirect(&mut response);

    let headers = response.headers_mut();

    if secure {
        headers.insert(actix_web::http::header::STRICT_TRANSPORT_SECURITY, HSTS);
    }

    if allow_origins {
        headers.insert(ALLOW_ORIGIN, ANY);
        headers.insert(ALLOW_HEADERS, ANY);
        headers.insert(ALLOW_METHODS, ANY);
    }

    response
}

/// Turns a redirect that reached this point into a visible failure.
///
/// A `3xx` on a Marti route is read by node-tak as a success carrying an empty
/// body, so the client fails somewhere else entirely — in a `TypeError` while
/// indexing into what it thought was a mission list. A `500` is worse for the
/// one request and far better for whoever has to find out why.
fn assert_no_redirect(response: &mut ServiceResponse<BoxBody>) {
    if !response.status().is_redirection() {
        return;
    }

    error!(
        status = response.status().as_u16(),
        path = response.request().path(),
        "A Marti route answered with a redirect, which every TAK client reads as success.",
    );

    let replacement = MartiError::Internal("a Marti route redirected".to_string()).error_response();

    *response.response_mut() = replacement;
}

/// The `api-version: 3` header `/Marti/sync/content` alone carries.
///
/// Exposed here rather than written inline so that the one endpoint which needs
/// it and the contract test which asserts it name the same constant.
pub fn api_version_header() -> (HeaderName, HeaderValue) {
    (API_VERSION, API_VERSION_RESPONSE)
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::http::header::{CONTENT_TYPE, STRICT_TRANSPORT_SECURITY};
    use actix_web::middleware::from_fn;
    use actix_web::{App, HttpResponse, test};

    use super::*;

    /// An app whose one route answers with whatever the test asked for.
    fn app_config(context: AppContext) -> impl FnOnce(&mut web::ServiceConfig) + Clone {
        move |config| {
            config.app_data(web::Data::new(context.clone())).service(
                web::scope("/Marti")
                    .wrap(from_fn(marti_headers))
                    .route(
                        "/ok",
                        web::get().to(|| async { HttpResponse::Ok().finish() }),
                    )
                    .route(
                        "/redirect",
                        web::get().to(|| async {
                            HttpResponse::MovedPermanently()
                                .insert_header(("location", "/Marti/ok"))
                                .finish()
                        }),
                    ),
            );
        }
    }

    #[actix_web::test]
    async fn a_redirect_becomes_a_loud_failure_rather_than_a_silent_one() {
        // node-tak would read a 301 as success and parse its empty body.
        let server = crate::testing::TestServer::start().await;
        let app =
            test::init_service(App::new().configure(app_config(server.context.clone()))).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get().uri("/Marti/redirect").to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json",
        );
    }

    #[actix_web::test]
    async fn a_plaintext_listener_does_not_promise_transport_security() {
        // The test harness binds without TLS; claiming HSTS there would pin a
        // browser to a scheme this listener does not serve.
        let server = crate::testing::TestServer::start().await;
        let app =
            test::init_service(App::new().configure(app_config(server.context.clone()))).await;

        let response =
            test::call_service(&app, test::TestRequest::get().uri("/Marti/ok").to_request()).await;

        assert!(response.headers().get(STRICT_TRANSPORT_SECURITY).is_none());
    }

    #[actix_web::test]
    async fn cross_origin_access_is_off_until_an_operator_turns_it_on() {
        let server = crate::testing::TestServer::start().await;
        let app =
            test::init_service(App::new().configure(app_config(server.context.clone()))).await;

        let response =
            test::call_service(&app, test::TestRequest::get().uri("/Marti/ok").to_request()).await;

        assert!(response.headers().get(ALLOW_ORIGIN).is_none());
    }

    #[actix_web::test]
    async fn turning_it_on_adds_all_three_headers_and_answers_preflights() {
        let server = crate::testing::TestServer::start_with(|config| {
            config.marti.allow_all_origins = true;
        })
        .await;
        let app =
            test::init_service(App::new().configure(app_config(server.context.clone()))).await;

        let response =
            test::call_service(&app, test::TestRequest::get().uri("/Marti/ok").to_request()).await;

        assert_eq!(response.headers().get(ALLOW_ORIGIN).unwrap(), "*");
        assert_eq!(response.headers().get(ALLOW_HEADERS).unwrap(), "*");
        assert_eq!(response.headers().get(ALLOW_METHODS).unwrap(), "*");

        let preflight = test::call_service(
            &app,
            test::TestRequest::default()
                .method(Method::OPTIONS)
                .uri("/Marti/ok")
                .to_request(),
        )
        .await;

        assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
        assert_eq!(preflight.headers().get(ALLOW_ORIGIN).unwrap(), "*");
    }

    #[actix_web::test]
    async fn a_preflight_nobody_asked_for_is_refused_as_json_rather_than_text() {
        let server = crate::testing::TestServer::start().await;
        let app =
            test::init_service(App::new().configure(app_config(server.context.clone()))).await;

        let response = test::call_service(
            &app,
            test::TestRequest::default()
                .method(Method::OPTIONS)
                .uri("/Marti/ok")
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json",
        );
    }

    #[actix_web::test]
    async fn the_content_endpoints_version_header_is_named_once() {
        let (name, value) = api_version_header();

        assert_eq!(name.as_str(), "api-version");
        assert_eq!(value, "3");
    }
}
