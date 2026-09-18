//! Who a control-API request is from.
//!
//! `/api/v1/services/*` and `/api/v1/events` are the two places in the admin API
//! that a program rather than a person calls, and the credential it calls with
//! is one the rest of `/api/v1` does not accept: a **service token**, or the
//! client certificate the sidecar already opens the CoT stream with. That is why
//! these routes sit outside [`api_auth`](crate::web::api::middleware::api_auth)
//! and resolve their caller here instead — the gate the browser API sits behind
//! takes one of our own RS256 access tokens and nothing else, and widening it
//! would widen it for every route under it.
//!
//! # The order, and why
//!
//! Certificate, then service token, then access token — strongest first, the
//! same order [`resolve_principal`](crate::auth::resolve::resolve_principal)
//! uses. A sidecar that has enrolled presents a certificate that cannot be
//! replayed out of a log; one that has not yet enrolled has only the token it was
//! configured with, which is exactly the case the token exists for.
//!
//! # Why the token is not rate limited
//!
//! `identity::credentials::mint` gives a service token 30 bytes of entropy
//! behind an `rsk_` prefix, and this endpoint names no account, so there is
//! nothing to enumerate and nothing to guess at a useful rate — the same
//! argument our own bearer tokens rest on. HTTP Basic is rate limited because a
//! password is a human-chosen secret attached to a username somebody can guess;
//! neither is true here.

use actix_web::HttpRequest;
use rustak_api::{CredentialKind, UserKind};
use rustak_core::identity::{AuthMethod, lookup_hint, verify_blocking};
use rustak_core::prelude::*;

use crate::auth::resolve::{AuthFailure, RequestFacts, Resolved, bearer};
use crate::db::repos::{CredentialRow, ServiceRow};
use crate::identity::users;
use crate::pki::PeerCertificate;
use crate::prelude::*;
use crate::web::helpers::request::client_ip;

/// What a caller is told when their credential is good but not good enough.
const NOT_YOURS: &str = "That service is not yours to change.";

/// Who is calling the control API.
#[derive(Debug, Clone)]
pub struct Caller {
    /// The account, the rights and the token, as the credential resolved.
    pub identity: Resolved,
}

impl Caller {
    /// Whether this caller administers the installation.
    pub fn is_admin(&self) -> bool {
        self.identity.principal.is_admin
    }

    /// Whether this caller is a sidecar rather than a person.
    pub fn is_service(&self) -> bool {
        self.identity.user.kind == UserKind::Service
    }

    /// The account this caller is.
    pub fn username(&self) -> &Username {
        &self.identity.user.username
    }

    /// Whether this caller may read or change `service`.
    ///
    /// An administrator may act on any registration; a service may act only on
    /// its own. The comparison is on the account the registration belongs to
    /// rather than on the name, because the name is what a caller supplies.
    pub fn owns(&self, service: &ServiceRow) -> bool {
        self.is_admin() || self.identity.user.id == service.user_id
    }

    /// [`owns`](Self::owns) as a refusal.
    ///
    /// # Errors
    ///
    /// [`AuthFailure::Forbidden`] — a `403` rather than a `401`, because
    /// presenting a different credential of the same kind will not help.
    pub fn require_owns(&self, service: &ServiceRow) -> Result<(), AuthFailure> {
        if self.owns(service) {
            return Ok(());
        }

        Err(AuthFailure::Forbidden(NOT_YOURS))
    }

    /// Refuses everybody who does not administer the installation.
    ///
    /// # Errors
    ///
    /// [`AuthFailure::Forbidden`] for a caller who is not an administrator.
    pub fn require_admin(&self) -> Result<(), AuthFailure> {
        if self.is_admin() {
            return Ok(());
        }

        Err(AuthFailure::Forbidden("Only an administrator may do that."))
    }
}

/// Resolves who a control-API request is from.
///
/// # Errors
///
/// [`AuthFailure::Rejected`] when nothing usable was presented — one answer for
/// every cause, so the endpoint cannot be asked whether a token exists;
/// [`AuthFailure::Forbidden`] when a certificate resolved to an account it does
/// not match; [`AuthFailure::Unavailable`] when a read fails.
#[instrument("plugins.auth", skip_all, err(Debug))]
pub async fn caller(context: &AppContext, request: &HttpRequest) -> Result<Caller, AuthFailure> {
    if let Some(peer) = request.conn_data::<PeerCertificate>() {
        let identity = crate::auth::cert::client_cert(context, peer).await?;

        return Ok(Caller { identity });
    }

    let Some(token) = crate::web::api::middleware::bearer_token(request.headers()) else {
        return Err(AuthFailure::Rejected);
    };

    if let Some(identity) = service_token(context, token).await? {
        return Ok(Caller { identity });
    }

    let config = context.config();
    let facts = RequestFacts {
        method: request.method().as_str(),
        path: request.path(),
        client_ip: client_ip(
            config.server.trust_proxy,
            request.headers(),
            request.peer_addr(),
        )
        .map(|ip| ip.to_string()),
        headers: request.headers(),
    };

    bearer(context, token, &facts)
        .await
        .map(|identity| Caller { identity })
}

/// Resolves a service token, or answers that the secret was not one.
///
/// A bearer header that is not a service token is *not* a refusal — the same
/// header carries our own access tokens — so this answers [`None`] and the
/// caller falls through to [`bearer`].
///
/// The lookup is by `lookup_hint` rather than by account, because a service
/// token names no account: it *is* the account. That is also why there is no
/// dummy hash on the miss path — with no username in the request there is
/// nothing a timing difference could reveal.
async fn service_token(
    context: &AppContext,
    secret: &str,
) -> Result<Option<Resolved>, AuthFailure> {
    let db = context.db();
    let candidates = db.credentials().find_by_hint(&lookup_hint(secret)).await?;

    for candidate in candidates {
        if candidate.kind != CredentialKind::ServiceToken || !usable(&candidate) {
            continue;
        }

        if !verify_blocking(Secret::new(secret), candidate.secret_hash.clone()).await? {
            continue;
        }

        let Some(user) = db.users().get(candidate.user_id).await? else {
            continue;
        };

        if user.disabled || user.kind != UserKind::Service {
            debug!("Refused a service token belonging to an account that may not use one.");

            return Err(AuthFailure::Rejected);
        }

        let via = AuthMethod::Basic {
            credential_id: candidate.id,
            kind: candidate.kind,
        };
        let principal = users::principal(db, &user, via, false).await?;

        return Ok(Some(Resolved {
            principal,
            user,
            claims: None,
        }));
    }

    Ok(None)
}

/// Whether a matched credential is still one we would accept.
///
/// `find_by_hint` already excludes revoked rows; what is left is the lifetime
/// and the use count, which a service token normally has neither of.
fn usable(candidate: &CredentialRow) -> bool {
    let now = chrono::Utc::now();

    candidate.expires_at.is_none_or(|expires| expires > now)
        && candidate.max_uses.is_none_or(|max| candidate.uses < max)
}

#[cfg(test)]
mod tests {
    use actix_web::test::TestRequest;
    use rustak_api::CredentialKind;

    use super::*;
    use crate::db::repos::{NewService, NewUser, UserRow};
    use crate::identity::credentials::{MintRequest, mint};
    use crate::testing::TestServer;

    /// A server with the signing keys installed.
    ///
    /// `TestServer` rather than `AppContext::new_mock`, because a bearer token
    /// that is not a service token falls through to our own access-token
    /// verification — which needs the keys, and answers "unavailable" rather
    /// than "rejected" without them.
    async fn started() -> TestServer {
        TestServer::start().await
    }

    /// A service account with a token, and the token.
    async fn service(context: &AppContext, name: &str) -> (UserRow, String) {
        let username = Username::parse(&format!("svc.{name}")).unwrap();
        let user = context
            .db()
            .users()
            .create(NewUser::service(username.clone()))
            .await
            .unwrap();

        let minted = mint(
            context.db(),
            &context.config().auth,
            &user,
            MintRequest::new(CredentialKind::ServiceToken, "Test sidecar", &username),
        )
        .await
        .unwrap();

        context
            .db()
            .services()
            .register(NewService::new(ServiceName::parse(name).unwrap(), user.id))
            .await
            .unwrap();

        (user, minted.secret.expose().to_string())
    }

    fn with_bearer(token: &str) -> HttpRequest {
        TestRequest::default()
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_http_request()
    }

    #[actix_web::test]
    async fn a_service_token_resolves_to_the_account_that_holds_it() {
        let server = started().await;
        let context = server.context.clone();
        let (user, token) = service(&context, "weather").await;

        let resolved = caller(&context, &with_bearer(&token)).await.unwrap();

        assert!(resolved.is_service());
        assert!(!resolved.is_admin());
        assert_eq!(resolved.username(), &user.username);
    }

    #[actix_web::test]
    async fn a_request_with_no_credential_is_refused() {
        let server = started().await;
        let context = server.context.clone();

        let refused = caller(&context, &TestRequest::default().to_http_request()).await;

        assert!(matches!(refused, Err(AuthFailure::Rejected)));
    }

    #[actix_web::test]
    async fn a_token_that_is_not_one_of_ours_is_refused_the_same_way() {
        // The same answer as "no credential": telling a caller that their token
        // was merely the wrong shape is an oracle they did not need.
        let server = started().await;
        let context = server.context.clone();

        let refused = caller(&context, &with_bearer("rsk_not-a-real-token")).await;

        assert!(matches!(refused, Err(AuthFailure::Rejected)));
    }

    #[actix_web::test]
    async fn a_revoked_token_stops_working() {
        let server = started().await;
        let context = server.context.clone();
        let (user, token) = service(&context, "weather").await;
        let held = context
            .db()
            .credentials()
            .list_for_user(user.id, false)
            .await
            .unwrap();
        crate::identity::credentials::revoke(
            context.db(),
            held[0].id,
            &user.username,
            crate::identity::VerifiedSecretCache::shared(),
        )
        .await
        .unwrap();

        assert!(matches!(
            caller(&context, &with_bearer(&token)).await,
            Err(AuthFailure::Rejected)
        ));
    }

    #[actix_web::test]
    async fn one_service_may_not_act_on_another_services_registration() {
        let server = started().await;
        let context = server.context.clone();
        let (_, weather_token) = service(&context, "weather").await;
        let (_adsb, _) = service(&context, "adsb").await;
        let theirs = context
            .db()
            .services()
            .get_by_name(&ServiceName::parse("adsb").unwrap())
            .await
            .unwrap()
            .unwrap();

        let weather = caller(&context, &with_bearer(&weather_token))
            .await
            .unwrap();

        assert!(!weather.owns(&theirs));
        assert!(matches!(
            weather.require_owns(&theirs),
            Err(AuthFailure::Forbidden(_))
        ));
        assert!(matches!(
            weather.require_admin(),
            Err(AuthFailure::Forbidden(_))
        ));
    }
}
