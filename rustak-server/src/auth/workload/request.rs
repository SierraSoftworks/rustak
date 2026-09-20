//! Reading a workload assertion off an HTTP request, and keeping it.
//!
//! Two headers carry the same credential and both are accepted:
//!
//! | Header | Who sends it |
//! |---|---|
//! | `Authorization: Bearer <jwt>` | rustak's own sidecar, and anything written this decade |
//! | `Authorization: Basic <base64(account:jwt)>` | a `commoncommo`-shaped client, which has a username field and a password field and nothing else |
//!
//! The Basic form is not a concession to laziness. ATAK's enrolment is a
//! username and a secret typed into two boxes, and a deployment that wants to
//! put a workload token in the second one should not have to ship a different
//! client. The username it sends is ignored: the **rules** decide the account,
//! and a token that names one account in a header and another in its claims is
//! answered as its claims.
//!
//! # Where it is accepted
//!
//! The enrolment prefix and nothing else on this path — `/Marti/api/tls/**`,
//! which is [`Purpose::Enrollment`](crate::identity::verify::Purpose) and
//! [`Purpose::EnrollmentProfile`](crate::identity::verify::Purpose).
//! `POST /oauth/token` takes the same credential through the RFC 7523
//! `jwt-bearer` grant instead, because that endpoint's vocabulary is a form
//! body rather than a header.
//!
//! # A refusal falls through rather than failing
//!
//! A token this module will not accept leaves the request exactly as it found
//! it, so the enrolment token an installation has always used still works on
//! the same route. Only an assertion that *verified* and was then refused for a
//! reason of ours — an account that is not a service, an access-control
//! expression — ends the request here.

use std::net::IpAddr;
use std::sync::Arc;

use actix_web::{HttpMessage, HttpRequest, web};

use crate::auth::basic::basic_credential;
use crate::auth::ratelimit::RateLimiter;
use crate::auth::resolve::{AuthFailure, ENROLLMENT_PREFIX, RequestFacts, Resolved};
use crate::prelude::*;
use crate::web::api::middleware::bearer_token;

use super::{Assertion, resolve_limited};

/// The assertion this request authenticated with, when it did.
///
/// Read by the enrolment handler, which audits *which* workload enrolled and
/// takes back the certificates the same account held before.
pub fn assertion_of(request: &HttpRequest) -> Option<Arc<Assertion>> {
    request.extensions().get::<Arc<Assertion>>().cloned()
}

/// Resolves a workload assertion presented on an enrolment route.
///
/// [`None`] means "this request carries nothing this module claims", and the
/// caller carries on with the credentials it always had — see the
/// [module documentation](self).
pub async fn from_request<S: Services>(
    services: &S,
    request: &HttpRequest,
    address: Option<IpAddr>,
) -> Option<Result<Resolved, AuthFailure>> {
    if !services.config().auth.workload.is_enabled() {
        return None;
    }

    if !request.path().starts_with(ENROLLMENT_PREFIX) {
        return None;
    }

    let token = presented(request)?;

    // An unlimited credential endpoint is worse than one that is briefly
    // unavailable, so a listener that installed no limiter offers nothing here.
    let Some(limiter) = request.app_data::<web::Data<Arc<RateLimiter>>>() else {
        error!(
            path = request.path(),
            "A listener serving an enrolment path installed no rate limiter."
        );

        return None;
    };

    let facts = RequestFacts {
        method: request.method().as_str(),
        path: request.path(),
        client_ip: address.map(|ip| ip.to_string()),
        headers: request.headers(),
    };

    match resolve_limited(services, limiter, address, &token, &facts).await {
        Ok((resolved, assertion)) => {
            // Kept so the handler can audit the run that enrolled without
            // re-reading a credential this has already spent its checks on.
            request.extensions_mut().insert(Arc::new(assertion));

            Some(Ok(resolved))
        }
        // The token was not one of ours to accept: leave the request as we
        // found it, so an enrolment token on the same route still works.
        Err(AuthFailure::Rejected) => None,
        Err(failure) => Some(Err(failure)),
    }
}

/// The token this request presents, from either header.
///
/// The Basic username is deliberately not read: the binding rules decide the
/// account, and a header that disagrees with the claims must not be able to
/// move the answer.
fn presented(request: &HttpRequest) -> Option<String> {
    if let Some(token) = bearer_token(request.headers()) {
        return Some(token.to_string());
    }

    basic_credential(request.headers()).map(|credential| credential.secret)
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;
    use base64::Engine as _;

    fn basic(pair: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(pair)
        )
    }

    #[actix_web::test]
    async fn either_header_carries_the_same_credential() {
        let bearer = TestRequest::get()
            .insert_header(("authorization", "Bearer a.b.c"))
            .to_http_request();
        let compat = TestRequest::get()
            .insert_header(("authorization", basic("ais:a.b.c")))
            .to_http_request();

        assert_eq!(presented(&bearer).as_deref(), Some("a.b.c"));
        assert_eq!(
            presented(&compat).as_deref(),
            Some("a.b.c"),
            "the username a Basic header carries is not what decides the account",
        );
        assert_eq!(presented(&TestRequest::get().to_http_request()), None);
    }

    #[actix_web::test]
    async fn a_bearer_header_wins_over_a_basic_one() {
        // Only reachable from a client that sent both, which is a client doing
        // something odd; answering as the stronger-looking header is the same
        // order `resolve_principal` takes.
        let request = TestRequest::get()
            .insert_header(("authorization", "Bearer from-the-bearer-header"))
            .to_http_request();

        assert_eq!(
            presented(&request).as_deref(),
            Some("from-the-bearer-header")
        );
    }

    #[actix_web::test]
    async fn an_installation_with_no_issuers_claims_nothing() {
        let server = crate::testing::TestServer::start().await;
        let request = TestRequest::get()
            .uri("/Marti/api/tls/config")
            .insert_header(("authorization", "Bearer a.b.c"))
            .to_http_request();

        assert!(
            from_request(&server.context, &request, None)
                .await
                .is_none(),
            "a token nobody registered an issuer for is not a credential here",
        );
    }

    #[actix_web::test]
    async fn nothing_outside_the_enrolment_prefix_is_offered_this_credential() {
        let server = crate::testing::TestServer::start_with(|config| {
            config.auth.workload = toml::from_str(
                r#"
                [[issuers]]
                name = "nomad"
                jwks_url = "https://nomad.example.com/jwks"
                audience = "rustak"
                "#,
            )
            .unwrap();
        })
        .await;

        for path in [
            "/api/v1/services/ais",
            "/Marti/api/groups/all",
            "/oauth/token",
        ] {
            let request = TestRequest::get()
                .uri(path)
                .insert_header(("authorization", "Bearer a.b.c"))
                .app_data(web::Data::new(Arc::clone(&server.limiter)))
                .to_http_request();

            assert!(
                from_request(&server.context, &request, None)
                    .await
                    .is_none(),
                "{path} must not read a workload assertion",
            );
        }
    }

    #[actix_web::test]
    async fn an_enrolment_path_with_no_rate_limiter_offers_nothing_rather_than_guessing_freely() {
        let server = crate::testing::TestServer::start_with(|config| {
            config.auth.workload = toml::from_str(
                r#"
                [[issuers]]
                name = "nomad"
                jwks_url = "https://nomad.example.com/jwks"
                audience = "rustak"
                "#,
            )
            .unwrap();
        })
        .await;
        let request = TestRequest::get()
            .uri("/Marti/api/tls/config")
            .insert_header(("authorization", "Bearer a.b.c"))
            .to_http_request();

        assert!(
            from_request(&server.context, &request, None)
                .await
                .is_none()
        );
    }

    #[actix_web::test]
    async fn rubbish_leaves_the_request_exactly_as_it_found_it() {
        // The property that keeps enrolment tokens working on the same route:
        // a credential this module will not accept is not a refusal, it is
        // silence.
        let server = crate::testing::TestServer::start_with(|config| {
            config.auth.workload = toml::from_str(
                r#"
                [[issuers]]
                name = "nomad"
                jwks_url = "https://nomad.example.com/jwks"
                audience = "rustak"
                "#,
            )
            .unwrap();
        })
        .await;
        let request = TestRequest::get()
            .uri("/Marti/api/tls/signClient/v2")
            .insert_header(("authorization", basic("ada:an-enrolment-token")))
            .app_data(web::Data::new(Arc::clone(&server.limiter)))
            .to_http_request();

        assert!(
            from_request(&server.context, &request, None)
                .await
                .is_none()
        );
        assert!(
            assertion_of(&request).is_none(),
            "and nothing was left behind in the request",
        );
    }
}
