//! What a handler asks for in its signature to say who it will serve.
//!
//! [`Authenticated`] resolves for anybody the middleware let through;
//! [`Administrative`] refuses everybody else before the handler body runs. The
//! check lives in the extractor rather than in a route wrapper so that an
//! endpoint cannot be mounted somewhere that forgets it — reviewing which
//! endpoints administer the installation is then a matter of searching for one
//! type rather than reading every handler.

use std::future::{Ready, ready};
use std::ops::Deref;

use actix_web::http::StatusCode;
use actix_web::{FromRequest, HttpMessage as _, HttpRequest, dev::Payload};

use super::error::ApiError;

/// Who a request speaks for, as the middleware resolved it.
///
/// Carries the stored row and the token's claims beside the principal because
/// almost everything that wants one wants another: `/me` renders the row, the
/// audit log names it, and signing out needs the `jti`.
pub type Identity = crate::auth::resolve::Resolved;

/// A request from somebody signed in.
#[derive(Debug, Clone)]
pub struct Authenticated(pub Identity);

impl Deref for Authenticated {
    type Target = Identity;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromRequest for Authenticated {
    type Error = ApiError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(identity(request).map(Authenticated))
    }
}

/// A request from somebody who administers this installation.
#[derive(Debug, Clone)]
pub struct Administrative(pub Identity);

impl Deref for Administrative {
    type Target = Identity;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromRequest for Administrative {
    type Error = ApiError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(identity(request).and_then(|identity| {
            if identity.principal.is_admin {
                return Ok(Administrative(identity));
            }

            Err(ApiError::forbidden("Only an administrator may do that."))
        }))
    }
}

/// Pulls the identity the middleware attached.
///
/// Its absence is a routing mistake — an endpoint mounted outside the
/// authenticating scope — rather than anything the caller did, so it is a `500`
/// and a log line rather than a `401` that would send somebody to sign in
/// again for no reason.
fn identity(request: &HttpRequest) -> Result<Identity, ApiError> {
    request
        .extensions()
        .get::<Identity>()
        .cloned()
        .ok_or_else(|| {
            tracing::error!(
                path = request.path(),
                "An endpoint outside the authenticating scope asked who the caller is."
            );

            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong on the server. Please try again.",
            )
        })
}

#[cfg(test)]
mod tests {
    use actix_web::test::TestRequest;
    use rustak_api::{UserKind, UserSource};
    use rustak_core::identity::{AuthMethod, Principal, PrincipalKind};
    use rustak_core::prelude::*;

    use super::*;
    use crate::auth::AccessClaims;
    use crate::db::repos::UserRow;

    fn identity_for(is_admin: bool) -> Identity {
        let username = Username::parse("ada").unwrap();
        let now = chrono::Utc::now();

        let mut principal = Principal::new(
            UserId::new(1),
            username.clone(),
            PrincipalKind::Person,
            AuthMethod::Bearer {
                jti: "a-token".to_string(),
                scope: "api".to_string(),
            },
        );
        principal.is_admin = is_admin;

        Identity {
            principal,
            user: UserRow {
                id: UserId::new(1),
                username,
                kind: UserKind::Person,
                display_name: None,
                email: None,
                is_admin,
                admin_override: None,
                disabled: false,
                source: UserSource::Local,
                oidc_issuer: None,
                oidc_subject: None,
                created_at: now,
                updated_at: now,
                last_seen_at: None,
                last_login_at: None,
            },
            claims: Some(AccessClaims {
                sub: "ada".to_string(),
                aud: "rustak".to_string(),
                iss: "https://tak.example.com".to_string(),
                iat: now.timestamp(),
                nbf: now.timestamp(),
                exp: now.timestamp() + 3600,
                jti: "a-token".to_string(),
                scope: "api".to_string(),
                dev: None,
            }),
        }
    }

    #[actix_web::test]
    async fn a_signed_in_caller_is_handed_to_the_handler() {
        let request = TestRequest::default().to_http_request();
        request.extensions_mut().insert(identity_for(false));

        let extracted = Authenticated::extract(&request).await.unwrap();

        assert_eq!(extracted.user.username.as_str(), "ada");
    }

    #[actix_web::test]
    async fn an_ordinary_caller_never_reaches_an_administrative_handler() {
        let request = TestRequest::default().to_http_request();
        request.extensions_mut().insert(identity_for(false));

        let refused = Administrative::extract(&request).await.unwrap_err();

        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn an_administrator_does() {
        let request = TestRequest::default().to_http_request();
        request.extensions_mut().insert(identity_for(true));

        assert!(Administrative::extract(&request).await.is_ok());
    }

    #[actix_web::test]
    async fn an_endpoint_mounted_outside_the_scope_is_our_mistake_rather_than_the_callers() {
        let request = TestRequest::default().to_http_request();

        let failed = Authenticated::extract(&request).await.unwrap_err();

        assert_eq!(
            failed.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "a 401 here would send somebody to sign in again for a routing bug",
        );
    }
}
