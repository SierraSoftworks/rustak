//! Which account a self-service endpoint is acting on.
//!
//! Credentials and devices are both "yours, or anybody's if you administer the
//! installation". That is one rule, and it is written once here rather than
//! four times across two handler files, because the failure mode of getting it
//! wrong in one of them is somebody reading or revoking a stranger's
//! credentials.
//!
//! # Why a stranger's account is a `403` and not a `404`
//!
//! The caller already knows the name they asked for; refusing with a `404`
//! would not hide anything from them, and it would send an administrator
//! hunting for a typo when the real answer is that their session has no
//! administrative scope. A name that genuinely is not here is still a `404`,
//! which only an administrator ever sees.

use rustak_core::prelude::*;

use crate::db::repos::UserRow;
use crate::services::{AppContext, Services as _};

use super::error::ApiError;
use super::extract::Identity;

/// The account an endpoint will act on, and whether it is the caller's own.
#[derive(Debug, Clone)]
pub struct Subject {
    pub user: UserRow,
    /// True when the caller is acting on themselves, which is what the audit
    /// entry and the response shape both turn on.
    pub is_self: bool,
}

impl Subject {
    /// The name to record as the subject of an audit entry.
    pub fn username(&self) -> &Username {
        &self.user.username
    }
}

/// Resolves the account a request names, refusing a stranger's.
///
/// `requested` is whatever the caller asked for — a query parameter or a path
/// segment. [`None`] means their own account, which is the common case and the
/// one an ordinary session is limited to.
///
/// # Errors
///
/// A `403` when somebody who does not administer the installation names an
/// account other than their own, a `404` when an administrator names one that
/// is not here, and a `500` when the read fails.
pub async fn resolve(
    context: &AppContext,
    caller: &Identity,
    requested: Option<&Username>,
) -> Result<Subject, ApiError> {
    let Some(requested) = requested else {
        return Ok(Subject {
            user: caller.user.clone(),
            is_self: true,
        });
    };

    if *requested == caller.user.username {
        return Ok(Subject {
            user: caller.user.clone(),
            is_self: true,
        });
    }

    if !caller.principal.is_admin {
        return Err(ApiError::forbidden(
            "Only an administrator may act on somebody else's account.",
        ));
    }

    let user = context
        .db()
        .users()
        .get_by_username(requested)
        .await
        .map_err(|err| failed(context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no account by that name."))?;

    Ok(Subject {
        user,
        is_self: false,
    })
}

/// Confirms the caller may act on an account somebody else's row belongs to.
///
/// The same rule as [`resolve`], asked the other way round: a device or a
/// credential is read first and the owner checked afterwards, because the
/// caller named the row rather than the account.
///
/// # Errors
///
/// A `403` when the row is somebody else's and the caller does not administer
/// the installation.
pub fn owns(caller: &Identity, owner: UserId) -> Result<bool, ApiError> {
    if caller.user.id == owner {
        return Ok(true);
    }

    if caller.principal.is_admin {
        return Ok(false);
    }

    Err(ApiError::forbidden(
        "Only an administrator may act on somebody else's account.",
    ))
}

/// Reports one of our own failures and generalises it.
pub fn failed(context: &AppContext, err: &Error) -> ApiError {
    context.session().record_human_error(err);

    ApiError::from_human(err)
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;

    use super::*;
    use crate::testing::TestServer;

    async fn identity_for(server: &TestServer, username: &str, is_admin: bool) -> Identity {
        let user = server.user(username, is_admin).await;
        // The scope a session for this account would actually be issued with:
        // `users::principal` treats it as a ceiling, so a helper that always
        // said `api` would silently make every administrator an ordinary user.
        let scope = crate::auth::tokens::scope_for(is_admin);
        let principal = crate::identity::users::principal(
            server.db(),
            &user,
            rustak_core::identity::AuthMethod::Bearer {
                jti: "a-token".to_string(),
                scope: scope.clone(),
            },
            false,
        )
        .await
        .unwrap();

        Identity {
            principal,
            user,
            claims: Some(crate::auth::AccessClaims {
                sub: username.to_string(),
                aud: "rustak".to_string(),
                iss: "https://localhost".to_string(),
                iat: 0,
                nbf: 0,
                exp: 0,
                jti: "a-token".to_string(),
                scope,
                dev: None,
            }),
        }
    }

    #[actix_web::test]
    async fn naming_nobody_means_the_caller_themselves() {
        let server = TestServer::start().await;
        let caller = identity_for(&server, "grace", false).await;

        let subject = resolve(&server.context, &caller, None).await.unwrap();

        assert!(subject.is_self);
        assert_eq!(subject.username().as_str(), "grace");
    }

    #[actix_web::test]
    async fn naming_yourself_is_the_same_as_naming_nobody() {
        let server = TestServer::start().await;
        let caller = identity_for(&server, "grace", false).await;
        let own = Username::parse("grace").unwrap();

        let subject = resolve(&server.context, &caller, Some(&own)).await.unwrap();

        assert!(subject.is_self);
    }

    #[actix_web::test]
    async fn naming_a_stranger_is_refused_rather_than_hidden() {
        let server = TestServer::start().await;
        let caller = identity_for(&server, "grace", false).await;
        server.user("ada", true).await;
        let other = Username::parse("ada").unwrap();

        let refused = resolve(&server.context, &caller, Some(&other))
            .await
            .unwrap_err();

        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn an_administrator_may_name_anybody_and_is_told_when_nobody_is_there() {
        let server = TestServer::start().await;
        let caller = identity_for(&server, "ada", true).await;
        server.user("grace", false).await;

        let subject = resolve(
            &server.context,
            &caller,
            Some(&Username::parse("grace").unwrap()),
        )
        .await
        .unwrap();

        assert!(!subject.is_self);
        assert_eq!(subject.username().as_str(), "grace");

        let missing = resolve(
            &server.context,
            &caller,
            Some(&Username::parse("nobody").unwrap()),
        )
        .await
        .unwrap_err();

        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn a_row_is_the_callers_own_or_an_administrators_to_touch() {
        let server = TestServer::start().await;
        let grace = identity_for(&server, "grace", false).await;
        let ada = identity_for(&server, "ada", true).await;

        assert!(owns(&grace, grace.user.id).unwrap());
        assert!(!owns(&ada, grace.user.id).unwrap(), "not hers, but allowed");

        let refused = owns(&grace, ada.user.id).unwrap_err();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    }
}
