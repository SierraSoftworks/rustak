//! Turning a credential into a [`Principal`].
//!
//! In M0 there is one credential to turn: our own bearer token on the public
//! listener. Client certificates arrive with the Marti and stream listeners in
//! M2, and Basic on the enrolment endpoints with them; both will be answered
//! here, beside this, so that "who is this request from" has one answer per
//! listener policy rather than one per endpoint.
//!
//! The resolution is written against [`Services`] and plain request facts
//! rather than against an actix request, so it can be exercised without a
//! server and reused by the listeners that are not actix at all.

use actix_web::http::header::HeaderMap;
use rustak_core::identity::{AuthMethod, Principal};
use rustak_core::prelude::*;

use crate::db::repos::UserRow;
use crate::identity::users;
use crate::prelude::Services;

use super::AccessClaims;
use super::acl::{AuthRequestFilter, evaluate};

/// What a request says about itself, for the access-control expressions.
pub struct RequestFacts<'a> {
    /// The HTTP method, upper case.
    pub method: &'a str,
    /// The path, without the query string.
    pub path: &'a str,
    /// Where the request came from, as
    /// [`client_ip`](crate::web::helpers::request::client_ip) resolved it.
    pub client_ip: Option<String>,
    /// The request headers.
    pub headers: &'a HeaderMap,
}

/// Who a request is from, and what they may do.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The rights this request carries.
    pub principal: Principal,
    /// The account as it is stored.
    pub user: UserRow,
    /// The claims of the token that was presented.
    pub claims: AccessClaims,
}

/// Why a request could not be answered.
#[derive(Debug)]
pub enum AuthFailure {
    /// No credential was presented, or the one presented is not one we accept.
    ///
    /// One variant for both on purpose: telling a caller that their token was
    /// merely expired, or that the account behind it does not exist, is an
    /// oracle they did not need.
    Rejected,
    /// The credential was good and the answer will not change by presenting
    /// another one.
    Forbidden(&'static str),
    /// Something of ours failed.
    Unavailable(Error),
}

impl From<Error> for AuthFailure {
    fn from(err: Error) -> Self {
        Self::Unavailable(err)
    }
}

/// Resolves one of our own access tokens.
///
/// # Errors
///
/// [`AuthFailure::Rejected`] for a token we would not accept, an account that
/// does not exist and an account that has been switched off;
/// [`AuthFailure::Forbidden`] when a configured access-control expression
/// refuses the request; [`AuthFailure::Unavailable`] when a read fails.
#[instrument("auth.resolve.bearer", skip_all, err(Debug))]
pub async fn bearer<S: Services>(
    services: &S,
    token: &str,
    facts: &RequestFacts<'_>,
) -> Result<Resolved, AuthFailure> {
    let config = services.config();
    let jwt = services.jwt()?;

    let claims = jwt.verify(token).map_err(|err| {
        debug!(error = %err, "Refused a request carrying a token we would not accept.");

        AuthFailure::Rejected
    })?;

    let db = services.db();

    // Revocation is a database read, which is why `JwtIssuer::verify` does not
    // do it: a pure function that reached for the database would put one on
    // every path that only needs to check a signature.
    if db.revoked_jtis().is_revoked(&claims.jti).await? {
        debug!("Refused a request carrying a token that has been revoked.");

        return Err(AuthFailure::Rejected);
    }

    // Looked up per request rather than carried in the token, so that switching
    // an account off takes effect now rather than when its token expires.
    let username = Username::from_storage(&claims.sub);
    let user = match db.users().get_by_username(&username).await? {
        Some(user) if !user.disabled => user,
        _ => return Err(AuthFailure::Rejected),
    };

    let filter = AuthRequestFilter {
        method: facts.method,
        path: facts.path,
        client_ip: facts.client_ip.clone(),
        headers: facts.headers,
        // Our own token carries no provider claims; `user_acl` is evaluated
        // against them where they exist, which is the sign-in itself.
        claims: None,
        username: user.username.as_str(),
        source: user.source.as_str(),
    };

    let outcome = evaluate(&config.auth, &filter);

    if config.auth.user_acl.is_some() && !outcome.allowed {
        info!(
            username = %user.username,
            path = facts.path,
            "An access-control expression refused an authenticated request."
        );

        return Err(AuthFailure::Forbidden(
            "Your account is not permitted to use this.",
        ));
    }

    let acl_admin = config.auth.admin_acl.is_some() && outcome.is_admin;
    let method = AuthMethod::Bearer {
        jti: claims.jti.clone(),
        scope: claims.scope.clone(),
    };
    let principal = users::principal(db, &user, method, acl_admin).await?;

    Ok(Resolved {
        principal,
        user,
        claims,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;

    fn facts(headers: &HeaderMap) -> RequestFacts<'_> {
        RequestFacts {
            method: "GET",
            path: "/api/v1/me",
            client_ip: Some("10.0.0.1".to_string()),
            headers,
        }
    }

    #[tokio::test]
    async fn a_token_we_issued_resolves_to_the_account_it_names() {
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", true).await;
        let headers = HeaderMap::new();

        let resolved = bearer(&server.context, &session.token, &facts(&headers))
            .await
            .unwrap();

        assert_eq!(resolved.user.id, user.id);
        assert!(resolved.principal.is_admin);
    }

    #[tokio::test]
    async fn a_token_we_did_not_sign_is_refused() {
        let server = TestServer::start().await;
        let headers = HeaderMap::new();

        assert!(matches!(
            bearer(&server.context, "not.a.token", &facts(&headers)).await,
            Err(AuthFailure::Rejected)
        ));
    }

    #[tokio::test]
    async fn a_revoked_token_stops_working_before_it_expires() {
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", false).await;
        let claims = server.jwt().unwrap().verify(&session.token).unwrap();
        let headers = HeaderMap::new();

        crate::auth::tokens::revoke(
            &server.context,
            &claims.jti,
            chrono::DateTime::from_timestamp(claims.exp, 0).unwrap(),
            user.id,
        )
        .await
        .unwrap();

        assert!(matches!(
            bearer(&server.context, &session.token, &facts(&headers)).await,
            Err(AuthFailure::Rejected)
        ));
    }

    #[tokio::test]
    async fn a_token_naming_an_account_that_is_gone_is_refused() {
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", false).await;
        let headers = HeaderMap::new();

        server.db().users().delete(user.id).await.unwrap();

        assert!(matches!(
            bearer(&server.context, &session.token, &facts(&headers)).await,
            Err(AuthFailure::Rejected)
        ));
    }

    #[tokio::test]
    async fn a_configured_expression_can_refuse_a_request_it_does_not_like() {
        let server = TestServer::start_with(|config| {
            config.auth.user_acl = Some(filt_rs::Filter::new(r#"path != "/api/v1/me""#).unwrap());
        })
        .await;
        let (_, session) = server.signed_in("ada", false).await;
        let headers = HeaderMap::new();

        assert!(matches!(
            bearer(&server.context, &session.token, &facts(&headers)).await,
            Err(AuthFailure::Forbidden(_))
        ));
    }

    #[tokio::test]
    async fn an_installation_with_no_expression_relies_on_the_account_instead() {
        // Applying a default-deny expression to a surface that cannot satisfy
        // it would refuse every request on an installation that never wrote one.
        let server = TestServer::start_with(|config| {
            config.auth.user_acl = None;
        })
        .await;
        let (_, session) = server.signed_in("ada", false).await;
        let headers = HeaderMap::new();

        assert!(
            bearer(&server.context, &session.token, &facts(&headers))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn an_expression_can_grant_administrative_access_per_request() {
        let server = TestServer::start_with(|config| {
            config.auth.admin_acl = Some(filt_rs::Filter::new(r#"username == "ada""#).unwrap());
        })
        .await;
        let (_, session) = server.signed_in("ada", false).await;
        let headers = HeaderMap::new();

        let resolved = bearer(&server.context, &session.token, &facts(&headers))
            .await
            .unwrap();

        assert!(resolved.principal.is_admin);
    }
}
