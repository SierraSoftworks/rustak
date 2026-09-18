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
//! An explicit credential first — service token, then access token — and the
//! client certificate only when the `Authorization` header established nobody.
//! That is the opposite of
//! [`resolve_principal`](crate::auth::resolve::resolve_principal)'s order, and
//! deliberately so: on the TAK surface a certificate is the strongest thing a
//! caller can present, but this is the *control* API, where a header is a
//! deliberate act and a certificate is ambient. `/api/v1` is mounted only on
//! the public listener today, where no certificate is captured at all — but if
//! it were ever mounted on the mTLS listener, certificate-first would mean any
//! EUD's certificate silently outranking an administrator's bearer token
//! (R-01 M5). A sidecar that has enrolled and sends no header still resolves as
//! its certificate, which is the case that order was written for.
//!
//! # What a service token is bounded by
//!
//! The same things every other credential is: the account must not be disabled,
//! `[auth] user_acl` must allow the request, the registration must not have been
//! switched off, and `max_uses` must not be spent. Each of those used to be
//! skipped here because the arm returned before `bearer` — which is where three
//! of the four live (R-01 M5, M11, L7).
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

use crate::auth::acl::{AuthRequestFilter, evaluate};
use crate::auth::resolve::{AuthFailure, RequestFacts, Resolved, bearer};
use crate::db::repos::{CredentialRow, ServiceRow, UserRow};
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

    if let Some(token) = crate::web::api::middleware::bearer_token(request.headers()) {
        if let Some(identity) = service_token(context, token, &facts).await? {
            return Ok(Caller { identity });
        }

        match bearer(context, token, &facts).await {
            Ok(identity) => return Ok(Caller { identity }),
            // A header that established nobody is not the end of it when a
            // certificate is also on the connection: the same header carries
            // mission tokens, and refusing here would break a call that never
            // claimed to be a control-API credential.
            Err(AuthFailure::Unavailable(err)) => return Err(AuthFailure::Unavailable(err)),
            Err(failure) if request.conn_data::<PeerCertificate>().is_none() => {
                return Err(failure);
            }
            Err(failure) => debug!(reason = ?failure, "A bearer token established no identity."),
        }
    }

    let Some(peer) = request.conn_data::<PeerCertificate>() else {
        return Err(AuthFailure::Rejected);
    };

    let identity = crate::auth::cert::client_cert(context, peer).await?;

    Ok(Caller { identity })
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
    facts: &RequestFacts<'_>,
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

        // A registration an operator switched off is refused here rather than
        // at each endpoint: `enabled = 0` was a fail-open kill switch that
        // nothing read, which is worse than no switch at all (R-01 M11). A
        // service that has not registered yet has no row and is unaffected —
        // registering is the one thing it still has to be able to do.
        if let Some(registration) = db.services().get_by_user(user.id).await?
            && !registration.enabled
        {
            debug!(service = %registration.name, "Refused a token for a service that is switched off.");

            return Err(AuthFailure::Forbidden(
                "That service has been switched off by an administrator.",
            ));
        }

        allowed_by_acl(context, &user, facts)?;

        // The only thing that moves `uses` and `last_used_at`, and therefore
        // the only thing that makes `max_uses` mean anything for a service
        // token; `usable` above reads what this wrote (R-01 L7). Not consuming:
        // a service token is reusable by design.
        if let Err(err) =
            crate::identity::credentials::record_use(db, &candidate, false, cache()).await
        {
            debug!(error = %err, "Could not record the use of a service token.");
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

/// The cache a recorded use has to be consistent with.
fn cache() -> &'static crate::identity::VerifiedSecretCache {
    crate::identity::VerifiedSecretCache::shared()
}

/// Applies `[auth] user_acl` to a service-token request.
///
/// `bearer` evaluates it for every other credential and this arm returned
/// before reaching it, so an operator who wrote `user_acl = 'client_ip in
/// 10.0.0.0/8'` had it enforced on all of `/api/v1` *except* the two routes a
/// program calls (R-01 M5).
///
/// # Errors
///
/// [`AuthFailure::Forbidden`] when a configured expression refuses the request.
fn allowed_by_acl(
    context: &AppContext,
    user: &UserRow,
    facts: &RequestFacts<'_>,
) -> Result<(), AuthFailure> {
    let config = context.config();

    if config.auth.user_acl.is_none() {
        return Ok(());
    }

    let outcome = evaluate(
        &config.auth,
        &AuthRequestFilter {
            method: facts.method,
            path: facts.path,
            client_ip: facts.client_ip.clone(),
            headers: facts.headers,
            // A service token carries no provider claims, exactly as our own
            // access tokens do not.
            claims: None,
            username: user.username.as_str(),
            source: user.source.as_str(),
        },
    );

    if outcome.allowed {
        return Ok(());
    }

    info!(
        username = %user.username,
        path = facts.path,
        "An access-control expression refused a service token."
    );

    Err(AuthFailure::Forbidden(
        "Your account is not permitted to use this.",
    ))
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

    #[actix_web::test]
    async fn a_service_token_is_refused_where_user_acl_refuses_the_request() {
        // R-01 M5. `user_acl` is evaluated inside `bearer`, and this arm
        // returned before reaching it — so an operator who wrote
        // `user_acl = 'client_ip in 10.0.0.0/8'` had it enforced on all of
        // `/api/v1` *except* the two routes a program calls.
        let server = TestServer::start_with(|config| {
            config.auth.user_acl = Some(filt_rs::Filter::new(r#"username == "nobody""#).unwrap());
        })
        .await;
        let context = server.context.clone();
        let (_, token) = service(&context, "weather").await;

        let refused = caller(&context, &with_bearer(&token)).await;

        assert!(
            matches!(refused, Err(AuthFailure::Forbidden(_))),
            "{refused:?}",
        );
    }

    #[actix_web::test]
    async fn a_service_token_still_works_where_the_expression_allows_it() {
        let server = TestServer::start_with(|config| {
            config.auth.user_acl = Some(filt_rs::Filter::new("true").unwrap());
        })
        .await;
        let context = server.context.clone();
        let (_, token) = service(&context, "weather").await;

        assert!(caller(&context, &with_bearer(&token)).await.is_ok());
    }

    #[actix_web::test]
    async fn a_service_an_operator_switched_off_stops_authenticating() {
        // R-01 M11. `services.enabled` was a fail-open kill switch that nothing
        // read: a service marked `enabled = 0` went on registering,
        // heartbeating, reading its configuration and opening the event feed.
        let server = TestServer::start().await;
        let context = server.context.clone();
        let (user, token) = service(&context, "weather").await;

        let registration = context
            .db()
            .services()
            .get_by_user(user.id)
            .await
            .unwrap()
            .expect("the registration under test");

        assert!(caller(&context, &with_bearer(&token)).await.is_ok());

        context
            .db()
            .services()
            .set_enabled(registration.id, false)
            .await
            .unwrap();

        let refused = caller(&context, &with_bearer(&token)).await;

        assert!(
            matches!(refused, Err(AuthFailure::Forbidden(_))),
            "{refused:?}",
        );
    }

    #[actix_web::test]
    async fn a_service_token_minted_for_one_use_works_once() {
        // R-01 L7. `usable` checks `uses < max_uses` and nothing in this module
        // moved `uses`, so `max_uses` was dead and `last_used_at` was always
        // null — an operator could not tell a live sidecar from a leaked
        // dormant token.
        let server = TestServer::start().await;
        let context = server.context.clone();
        let username = Username::parse("svc.once").unwrap();
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
            MintRequest {
                max_uses: Some(1),
                ..MintRequest::new(CredentialKind::ServiceToken, "One shot", &username)
            },
        )
        .await
        .unwrap();
        let token = minted.secret.expose().to_string();

        assert!(caller(&context, &with_bearer(&token)).await.is_ok());

        let refused = caller(&context, &with_bearer(&token)).await;

        assert!(matches!(refused, Err(AuthFailure::Rejected)), "{refused:?}");

        let held = context
            .db()
            .credentials()
            .list_for_user(user.id, true)
            .await
            .unwrap();

        assert_eq!(held[0].uses, 1);
        assert!(held[0].last_used_at.is_some(), "a live token says so");
    }

    #[actix_web::test]
    async fn an_explicit_header_outranks_an_ambient_certificate() {
        // R-01 M5, the related half: `/api/v1` is mounted only on the public
        // listener today, where no certificate is captured — but the order has
        // to be right before that changes, or any EUD's certificate would
        // silently outrank an administrator's bearer token on the control API.
        let server = started().await;
        let context = server.context.clone();
        let (user, token) = service(&context, "weather").await;

        let resolved = caller(&context, &with_bearer(&token)).await.unwrap();

        assert_eq!(resolved.username(), &user.username);
    }
}
