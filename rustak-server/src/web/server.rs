//! Binding the two HTTP listeners.
//!
//! One `HttpServer` with one binding per configured address, rather than a
//! server per socket: they serve the same routes from the same state, and a
//! second server would be a second worker pool and a second thing to shut down
//! for no benefit.
//!
//! Signals are disabled because the process owns them — the runtime cancels one
//! [`Shutdown`] and everything winds down together, rather than actix taking
//! `SIGINT` for itself and the stream listeners finding out later.
//!
//! # The drain is the operator's budget, minus a second
//!
//! Both servers are given
//! [`listener_drain_seconds`](crate::config::ServerConfig::listener_drain_seconds)
//! rather than a constant of their own. It used to be a flat ten seconds, which
//! is exactly `docker stop`'s default grace period: an idle HTTP/2 keep-alive
//! connection would hold the drain open for all ten, the container would be
//! `SIGKILL`ed at the moment actix gave up, and the WAL checkpoint that runs
//! after the listeners stop would never run. The budget is
//! `[server] shutdown_timeout` now, and actix is given a second less than it so
//! that [`runtime`](crate::runtime)'s own bounded wait is the one which reports
//! a drain that overran.
//!
//! # Why the Marti listener is a second server
//!
//! [`build_marti`] binds `[web.marti]` with a different TLS configuration — a
//! client certificate is **required** there, and
//! [`on_connect_capture`](crate::pki::tls::on_connect_capture) lifts it onto the
//! connection so a handler can resolve it — and serves a narrower set of routes:
//! the TAK surface and nothing else. A browser has no business on that port and
//! the admin UI has no business answering a device, so `/api/v1` and the
//! single-page shell are simply not mounted rather than being refused.

use std::sync::Arc;

use actix_web::{App, HttpServer, dev::Server, web};

use crate::auth::RateLimiter;
use crate::config::ClientCertMode;
use crate::marti;
use crate::prelude::*;
use crate::web::{api, telemetry::TracingLogger, tls::PublicTls, ui};

/// Everything the public listener serves, for one `App`.
///
/// Exposed so that a test can build the same routes over
/// `actix_web::test::init_service` without a socket: what is tested is then the
/// application this function describes rather than a copy of it that has since
/// drifted.
///
/// The rate limiter is passed in rather than built here because actix builds
/// one `App` per worker thread: a limiter created inside this closure would be
/// one bucket per worker, and an attacker would get that many times as many
/// attempts.
pub fn services(
    context: AppContext,
    limiter: Arc<RateLimiter>,
) -> impl FnOnce(&mut web::ServiceConfig) + Clone {
    move |config| {
        config
            .app_data(web::Data::new(context.clone()))
            .app_data(web::Data::new(limiter.clone()))
            .service(api::configure())
            // Ahead of the catch-all, which would answer a missing Marti route
            // with the single-page shell and a `200` — and a TAK client reads
            // that as a mission list.
            .configure(marti::services(marti::ListenerRole::Public))
            // Ahead of the catch-all, which would otherwise answer the
            // authority's validation request with the SPA shell and a `200` —
            // which fails the order with no useful message. Always mounted,
            // and a `404` unless an order is in flight.
            .configure(crate::pki::acme::http01_routes)
            // Ahead of the catch-all, which would otherwise answer it with the
            // SPA shell — and a crawler handed HTML where it asked for
            // `robots.txt` reads that as "no rules".
            .route("/robots.txt", web::get().to(ui::robots))
            .default_service(web::get().to(ui::serve));
    }
}

/// Everything the mutually authenticated listener serves.
///
/// The TAK surface and nothing else: no `/api/v1`, no admin UI, and a default
/// service that answers our own JSON `404` rather than the single-page shell —
/// a TAK client handed HTML with a `200` parses it as a payload.
///
/// The rate limiter is shared with the public listener, so an attacker cannot
/// double their allowance by alternating ports.
pub fn marti_services(
    context: AppContext,
    limiter: Arc<RateLimiter>,
) -> impl FnOnce(&mut web::ServiceConfig) + Clone {
    move |config| {
        config
            .app_data(web::Data::new(context.clone()))
            .app_data(web::Data::new(limiter.clone()))
            .configure(marti::services(marti::ListenerRole::Marti))
            .default_service(web::to(marti_unmatched));
    }
}

/// What a path outside the TAK surface is answered with on `[web.marti]`.
async fn marti_unmatched(request: actix_web::HttpRequest) -> marti::MartiResult {
    Err(marti::MartiError::NotFound(request.path().to_string()))
}

/// Binds `[web.marti]` and returns the server, unstarted.
///
/// [`None`] when the listener is switched off, which is a configuration an
/// operator chose rather than a failure.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the authority has not been
/// installed or cannot produce a TLS configuration, and a
/// [`human_errors::Kind::User`] error when the address cannot be bound.
#[instrument("web.server.build_marti", skip_all, err(Display))]
pub fn build_marti(context: AppContext) -> Result<Option<Server>, Error> {
    let config = context.config();

    if !config.web.marti.enabled {
        info!("The Marti listener is switched off in the configuration.");

        return Ok(None);
    }

    let pki = context.pki()?;
    let required = matches!(config.web.marti.client_cert, ClientCertMode::Required);
    let drain = config.server.listener_drain_seconds();
    let tls = pki.marti_server_config(required)?;
    let limiter = Arc::new(RateLimiter::new(&config.auth.rate_limit));

    let mut server = HttpServer::new(move || {
        App::new()
            .wrap(TracingLogger::<AppContext>::new())
            .configure(marti_services(context.clone(), limiter.clone()))
    })
    // Without this a handler has no way to reach the certificate rustls
    // verified, and `:8443` would authenticate nobody.
    .on_connect(crate::pki::tls::on_connect_capture)
    .disable_signals()
    .shutdown_timeout(drain);

    for socket in config.web.marti.listen.to_socket_addrs()? {
        server = server
            .bind_rustls_0_23(socket, tls.clone())
            .map_err(|err| cannot_bind(socket, &err))?;

        info!(address = %socket, client_cert = %required, "The Marti listener is bound.");
    }

    Ok(Some(server.run()))
}

/// Binds every configured public address and returns the server, unstarted.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when an address cannot be bound —
/// something else is on the port, or the port needs privileges this process
/// does not have — or when no address was configured at all.
#[instrument("web.server.build", skip_all, err(Display))]
pub fn build_public(context: AppContext, tls: PublicTls) -> Result<Server, Error> {
    let config = context.config();
    let addresses = &config.web.public.listen;

    if addresses.is_empty() {
        return Err(human_errors::user(
            "The public listener has no addresses to bind.",
            &["Set [web.public] listen to at least one address, such as \"0.0.0.0:8446\"."],
        ));
    }

    let limiter = Arc::new(RateLimiter::new(&config.auth.rate_limit));
    let drain = config.server.listener_drain_seconds();

    let mut server = HttpServer::new(move || {
        App::new()
            .wrap(TracingLogger::<AppContext>::new())
            .configure(services(context.clone(), limiter.clone()))
    })
    .disable_signals()
    .shutdown_timeout(drain);

    for address in addresses {
        for socket in address.to_socket_addrs()? {
            server = match tls.clone() {
                Some(tls) => server
                    .bind_rustls_0_23(socket, (*tls).clone())
                    .map_err(|err| cannot_bind(socket, &err))?,
                None => server
                    .bind(socket)
                    .map_err(|err| cannot_bind(socket, &err))?,
            };

            info!(
                address = %socket,
                tls = tls.is_some(),
                "The public listener is bound."
            );
        }
    }

    Ok(server.run())
}

/// What to say when a socket will not open.
fn cannot_bind(socket: std::net::SocketAddr, err: &std::io::Error) -> Error {
    human_errors::user(
        format!("We could not bind a listener to {socket}: {err}"),
        &[
            "Check that nothing else is already listening on that address.",
            "Ports below 1024 need CAP_NET_BIND_SERVICE, or a proxy in front.",
        ],
    )
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::test;

    use super::*;

    #[actix_web::test]
    async fn the_routes_a_test_builds_are_the_routes_the_listener_serves() {
        let server = crate::testing::TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get().uri("/robots.txt").to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        let response = test::call_service(
            &app,
            test::TestRequest::get().uri("/api/v1/health").to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn an_unknown_path_reaches_the_single_page_shell() {
        let server = crate::testing::TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get().uri("/admin/settings").to_request(),
        )
        .await;

        assert_ne!(response.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn a_listener_with_nowhere_to_bind_says_so() {
        let context = AppContext::new_mock(|config| {
            config.web.public.listen = Vec::new();
        })
        .await
        .unwrap();

        let refused = match build_public(context, None) {
            Ok(_) => panic!("a listener with nowhere to bind must not come up"),
            Err(err) => err,
        };

        assert!(refused.is(human_errors::Kind::User));
    }

    #[actix_web::test]
    async fn a_plaintext_listener_binds_the_address_it_was_given() {
        let directory = tempfile::tempdir().unwrap();
        let context = AppContext::new_mock(|config| {
            *config = crate::config::Config::testing(directory.path());
        })
        .await
        .unwrap();

        // Port zero, so concurrent suites do not race for a fixed number.
        assert!(build_public(context, None).is_ok());
    }
}
