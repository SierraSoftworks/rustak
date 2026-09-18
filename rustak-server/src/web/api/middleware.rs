//! The gate every `/api/v1` route but the public ones sits behind.
//!
//! The decision itself is [`crate::auth::resolve::bearer`]; what is here is the
//! actix half of it — reading the header, turning a refusal into the same
//! `{"error": …}` body every other failure uses, and attaching the resolved
//! identity to the request so the extractors can find it.
//!
//! # Why a refusal is a response rather than an error
//!
//! A middleware that returned `Err` would be rendered by actix's default error
//! handler, which writes `text/plain`. Every other failure in this API is
//! JSON, and a client that has to parse two shapes will eventually parse one of
//! them wrong.

use actix_web::body::BoxBody;
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::http::StatusCode;
use actix_web::http::header::AUTHORIZATION;
use actix_web::middleware::Next;
use actix_web::{HttpMessage as _, HttpResponse, web};

use crate::auth::resolve::{AuthFailure, RequestFacts, bearer};
use crate::prelude::*;
use crate::web::helpers::request::client_ip;

use super::error::json_error;

/// What a caller is told whichever way their token failed.
///
/// One message for every cause, so that the endpoint cannot be used to find out
/// whether an account exists or whether a token was merely expired.
const REJECTED: &str = "Your session is not valid. Please sign in again.";

/// What a caller is told when something of ours failed.
const UNAVAILABLE: &str = "Something went wrong on the server. Please try again.";

/// Reads a bearer token, accepting either capitalisation of the scheme.
pub fn bearer_token(headers: &actix_web::http::header::HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// Resolves the caller and attaches them to the request.
///
/// # Errors
///
/// Never as an `Err`; see the module documentation.
pub async fn api_auth(
    request: ServiceRequest,
    next: Next<BoxBody>,
) -> Result<ServiceResponse<BoxBody>, actix_web::Error> {
    let Some(context) = request.app_data::<web::Data<AppContext>>().cloned() else {
        error!("A request reached the authenticating scope with no application context.");

        return Ok(
            request.into_response(json_error(StatusCode::INTERNAL_SERVER_ERROR, UNAVAILABLE))
        );
    };

    let Some(token) = bearer_token(request.headers()).map(str::to_string) else {
        return Ok(request.into_response(json_error(StatusCode::UNAUTHORIZED, REJECTED)));
    };

    let config = context.config();
    let facts = RequestFacts {
        method: request.method().as_str(),
        path: request.path(),
        client_ip: client_ip(
            config.server.trust_proxy,
            request.headers(),
            request.peer_addr(),
        ),
        headers: request.headers(),
    };

    match bearer(context.get_ref(), &token, &facts).await {
        Ok(resolved) => {
            drop(facts);
            request.extensions_mut().insert(resolved);

            next.call(request).await
        }
        Err(failure) => {
            let response = refusal(context.get_ref(), failure);

            Ok(request.into_response(response))
        }
    }
}

/// What each way of being refused looks like on the wire.
fn refusal(context: &AppContext, failure: AuthFailure) -> HttpResponse {
    match failure {
        AuthFailure::Rejected => json_error(StatusCode::UNAUTHORIZED, REJECTED),
        // A `403` rather than a `401`: the credential was good and the answer
        // will not change by presenting another one, so bouncing the browser
        // through a sign-in would only waste somebody's time.
        AuthFailure::Forbidden(message) => json_error(StatusCode::FORBIDDEN, message),
        // Only the Basic arm rate limits, and `/api/v1` never accepts Basic —
        // but the shape has to be answered rather than assumed unreachable.
        AuthFailure::RateLimited(retry_after) => json_error(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "Too many attempts. Try again in {} minutes.",
                retry_after.num_minutes().max(1)
            ),
        ),
        AuthFailure::Unavailable(err) => {
            error!(error = %err, "Could not resolve who a request is from.");
            context.session().record_human_error(&err);

            json_error(StatusCode::INTERNAL_SERVER_ERROR, UNAVAILABLE)
        }
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::header::{HeaderMap, HeaderName, HeaderValue};

    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_str(value).unwrap(),
        );

        map
    }

    #[test]
    fn either_capitalisation_of_the_scheme_is_accepted() {
        // Some TAK tooling sends a lower-case scheme, and the specification
        // says the scheme is case-insensitive.
        assert_eq!(bearer_token(&headers("Bearer a-token")), Some("a-token"));
        assert_eq!(bearer_token(&headers("bearer a-token")), Some("a-token"));
    }

    #[test]
    fn anything_that_is_not_a_bearer_token_is_not_one() {
        assert_eq!(bearer_token(&HeaderMap::new()), None);
        assert_eq!(bearer_token(&headers("Basic dXNlcjpwYXNz")), None);
        assert_eq!(bearer_token(&headers("Bearer ")), None);
        assert_eq!(bearer_token(&headers("a-token")), None);
    }
}
