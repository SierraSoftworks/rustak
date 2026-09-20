//! Where the browser sign-in endpoints are mounted.
//!
//! The handlers are [`crate::auth::oauth_server`]; this file is the routing
//! table and the two decisions that go with it.
//!
//! # Why these are not in the `/Marti` or `/oauth` scopes
//!
//! Both of those scopes are wrapped in [`marti_headers`], whose
//! `assert_no_redirect` turns any `3xx` into a `500` — because node-tak reads
//! every status below 400 as success and parses the redirect's empty body as
//! the payload. A sign-in flow is made of redirects, so these endpoints are
//! registered beside those scopes rather than inside them. `/oauth/authorize`
//! is registered **before** the `/oauth` scope for the same reason: actix
//! matches services in registration order, and a scope that matches the prefix
//! answers its own unmatched paths rather than letting a later sibling see
//! them.
//!
//! [`marti_headers`]: super::headers::marti_headers
//!
//! # Why only the public listener
//!
//! TAK Server serves `/login/**` only on the ports that ask for no client
//! certificate, and the reasoning carries over: a device on the mutually
//! authenticated listener already has a stronger credential than any cookie
//! this flow could set, and a browser has no business on that port. The
//! [`ListenerRole`] test below is the whole of that policy.

use actix_web::web;

use crate::auth::oauth_server::{authorize, discovery, login, session};

use super::extract::ListenerRole;

/// Registers the sign-in endpoints, on the public listener only.
pub fn routes(role: ListenerRole) -> impl FnOnce(&mut web::ServiceConfig) + Clone {
    move |config| {
        if role != ListenerRole::Public {
            return;
        }

        config
            // Before the `/oauth` scope registered after it; see above.
            .route("/oauth/authorize", web::get().to(authorize::authorize))
            // The path OpenID Connect Discovery reserves, describing **this**
            // server. The `/login/` one below is TAK Server's invention and
            // describes the upstream provider; see `oauth_server::discovery`.
            .route(
                "/.well-known/openid-configuration",
                web::get().to(discovery::openid_configuration),
            )
            .service(
                web::scope("/login")
                    // The two literal children before `/auth`, which would not
                    // match them but sits beside a path that could grow one.
                    .route(
                        "/.well-known/openid-configuration",
                        web::get().to(session::openid_configuration),
                    )
                    .route("/authserver", web::get().to(session::authserver))
                    .route("/auth", web::get().to(login::login_auth))
                    .route("/redirect", web::get().to(login::login_redirect)),
            )
            .route("/token/access", web::get().to(session::token_access))
            .route("/logout", web::get().to(session::logout))
            .route("/logout", web::post().to(session::logout));
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};

    use super::*;
    use crate::testing::TestServer;

    /// Every path this file mounts.
    const PATHS: &[&str] = &[
        "/oauth/authorize",
        "/.well-known/openid-configuration",
        "/login/auth",
        "/login/redirect",
        "/login/authserver",
        "/login/.well-known/openid-configuration",
        "/token/access",
        "/logout",
    ];

    #[actix_web::test]
    async fn every_sign_in_path_is_served_rather_than_falling_through_to_the_shell() {
        // The single-page application's catch-all answers HTML with a `200`; a
        // sign-in endpoint that fell through to it would look like it worked.
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for path in PATHS {
            let response =
                test::call_service(&app, test::TestRequest::get().uri(path).to_request()).await;

            assert_ne!(
                response
                    .headers()
                    .get(actix_web::http::header::CONTENT_TYPE),
                Some(&actix_web::http::header::HeaderValue::from_static(
                    "text/html"
                )),
                "{path} reached the single-page shell",
            );
        }
    }

    #[actix_web::test]
    async fn the_authorization_endpoint_is_not_swallowed_by_the_oauth_scope() {
        // Registration order is load-bearing: the `/oauth` scope's default
        // service answers its own unmatched paths, so a later registration of
        // `/oauth/authorize` would never be reached.
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/oauth/authorize?response_type=code&client_id=nobody&redirect_uri=x")
                .to_request(),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "an unregistered client is our refusal, not the scope's 404",
        );
    }

    #[actix_web::test]
    async fn the_mutually_authenticated_listener_serves_no_sign_in_flow() {
        let server = TestServer::start().await;
        let app = test::init_service(
            App::new()
                .configure(super::super::services(ListenerRole::Marti))
                .app_data(web::Data::new(server.context.clone())),
        )
        .await;

        for path in PATHS {
            let response =
                test::call_service(&app, test::TestRequest::get().uri(path).to_request()).await;

            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }
}
