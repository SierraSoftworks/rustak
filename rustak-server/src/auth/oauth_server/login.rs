//! `/login/auth` and `/login/redirect` — the TAK-shaped federation round trip.
//!
//! A TAK-style browser client knows exactly two things about signing in: send
//! the person to `/login/auth`, and expect them back with an `access_token`
//! cookie. Everything in between is ours to choose, and what is chosen here is
//! the same shape TAK Server uses — a `state` cookie whose SHA-256 is the
//! `state` the identity provider echoes — with the three controls TAK Server
//! does not have layered on top: a one-shot server-side record, a proof key on
//! the provider's code, and a nonce in its ID token. See [`super`] for what
//! each of them stops.
//!
//! # Failures say nothing
//!
//! Every refusal on the callback is the same `400`, whatever went wrong: a
//! tampered `state`, a replayed callback, a provider that refused the code, an
//! ID token we would not accept, an account the access-control expression turns
//! away. Distinguishing them would tell whoever is probing which of the
//! controls they have got past, and the person who legitimately hit one can
//! only do one thing about it — start again.
//!
//! The provider's own error text is deliberately never echoed: it is written
//! for an operator reading a log, it routinely names accounts and policies, and
//! it arrives in a query string somebody may be watching.

use actix_web::http::header::SET_COOKIE;
use actix_web::{HttpRequest, HttpResponse, web};
use rustak_api::{AuditCategory, AuditOutcome};

use crate::auth::acl::{AuthRequestFilter, evaluate};
use crate::auth::tokens;
use crate::db::AuditEntry;
use crate::db::repos::UserRow;
use crate::identity::{settings, users};
use crate::prelude::*;
use crate::web::helpers::oidc::{self, pkce};
use crate::web::helpers::request::{client_ip, request_base_url};

use super::authorize::{deliver_code, redirect};
use super::state::{self, PendingAuth, PendingKind, STATE_COOKIE};
use super::{cookies, session};

/// Where the identity provider returns the browser to.
pub const CALLBACK_PATH: &str = "/login/redirect";

/// Where a browser sign-in with nowhere else to go ends up.
const DEFAULT_RETURN_TO: &str = "/";

/// The query `/login/auth` accepts.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthQuery {
    /// Where to send the browser afterwards. A path on this site only.
    #[serde(default, rename = "returnTo", alias = "return_to")]
    pub return_to: Option<String>,
}

/// The query the identity provider returns the browser with.
#[derive(Debug, Clone, Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

/// `GET /login/auth` — start a sign-in with no OAuth2 client behind it.
pub async fn login_auth(
    request: HttpRequest,
    context: web::Data<AppContext>,
    query: Option<web::Query<AuthQuery>>,
) -> HttpResponse {
    let return_to = query
        .and_then(|query| query.return_to.clone())
        .filter(|path| is_local_path(path));

    begin_federation(
        context.get_ref(),
        &request,
        PendingKind::Browser { return_to },
    )
    .await
}

/// Sends the browser to the identity provider, remembering what it was doing.
///
/// Shared with `GET /oauth/authorize`, which uses it when an authorization
/// request arrives with no session behind it.
pub async fn begin_federation(
    context: &AppContext,
    request: &HttpRequest,
    kind: PendingKind,
) -> HttpResponse {
    let config = context.config();

    let Some(provider) = config.auth.oidc.as_ref() else {
        return session::no_provider();
    };

    let discovery = match oidc::discovery(context, provider).await {
        Ok(discovery) => discovery,
        Err(err) => {
            warn!(error = %err, "Could not reach the identity provider to start a sign-in.");
            context.session().record_human_error(&err);

            return session::refused();
        }
    };

    let Some(base_url) = base_url(context, request).await else {
        return session::refused();
    };

    let callback = format!("{base_url}{CALLBACK_PATH}");
    let secret = state::new_state();
    let nonce = state::new_nonce();
    let binding = state::new_binding();
    let proof = pkce::pkce_pair();

    let recorded = state::begin(
        context.db(),
        &secret,
        PendingAuth {
            kind,
            verifier: proof.verifier,
            nonce: nonce.clone(),
            redirect_uri: callback.clone(),
            // The digest, never the value: what is stored has to be useless to
            // a reader of the database.
            binding: state::state_hash(&binding),
            expires_at: state::expiry(),
        },
    )
    .await;

    if let Err(err) = recorded {
        error!(error = %err, "Could not record a sign-in that was starting.");
        context.session().record_human_error(&err);

        return session::refused();
    }

    let mut url = url::Url::parse(&discovery.authorization_endpoint).unwrap_or_else(|_| {
        // Unreachable in practice: the endpoint came from a parsed discovery
        // document. A relative fallback is still a refusal rather than a panic.
        url::Url::parse("about:blank").expect("about:blank is a URL")
    });

    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &provider.client_id)
        .append_pair("redirect_uri", &callback)
        .append_pair("scope", &provider.scopes().join(" "))
        .append_pair("state", &state::state_hash(&secret))
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &proof.challenge)
        .append_pair("code_challenge_method", "S256");

    let mut response = redirect(url.as_str());

    // Two cookies, two different jobs. `state` is the name and the rule TAK
    // clients expect; `__Host-rustak_login` is the binding a sibling origin
    // cannot write, and therefore cannot transplant into somebody else's
    // browser (R-01 M7).
    for cookie in [
        cookies::state_cookie(&secret),
        cookies::binding_cookie(&binding),
    ] {
        if let Ok(value) = actix_web::http::header::HeaderValue::from_str(&cookie) {
            response.headers_mut().append(SET_COOKIE, value);
        }
    }

    response
}

/// `GET /login/redirect` — the identity provider brings the browser back.
pub async fn login_redirect(
    request: HttpRequest,
    context: web::Data<AppContext>,
    query: Option<web::Query<CallbackQuery>>,
) -> HttpResponse {
    let Some(query) = query else {
        return session::callback_refused(&request);
    };

    let (Some(code), Some(returned)) = (query.code.as_deref(), query.state.as_deref()) else {
        debug!("A callback arrived without a code and a state.");

        return session::callback_refused(&request);
    };

    // TAK Server's rule, and the only one that says this browser is the one
    // that started the flow.
    let Some(cookie) = cookies::cookie_value(request.headers(), STATE_COOKIE) else {
        debug!("A callback arrived with no state cookie.");

        return session::callback_refused(&request);
    };

    if !state::matches_state(&cookie, returned) {
        warn!("Refused a callback whose state does not belong to this browser's cookie.");

        return session::callback_refused(&request);
    }

    // One shot, so the same callback cannot be presented twice even by the
    // browser that legitimately made it.
    let pending = match state::claim(context.db(), returned).await {
        Ok(pending) => pending,
        Err(err) => {
            debug!(error = %err, "Refused a callback we have no record of.");

            return session::callback_refused(&request);
        }
    };

    // The second binding, checked after the record is spent so that a wrong
    // guess still consumes the pending sign-in rather than letting it be
    // retried. A sibling origin can write `state`; it cannot write a
    // `__Host-`-prefixed cookie for this host at all (R-01 M7).
    let binding = cookies::cookie_value(request.headers(), state::BINDING_COOKIE);

    if !state::matches_binding(binding.as_deref(), &pending.binding) {
        warn!("Refused a callback that did not carry this browser's sign-in binding.");

        return session::callback_refused(&request);
    }

    match sign_in(context.get_ref(), &request, &pending, code).await {
        Ok((user, is_admin)) => {
            complete(context.get_ref(), &request, &pending, &user, is_admin).await
        }
        Err(refusal) => {
            debug!(error = %refusal, "Refused a sign-in from the identity provider.");

            session::callback_refused(&request)
        }
    }
}

/// Redeems the provider's code and turns its claims into one of our accounts.
async fn sign_in(
    context: &AppContext,
    request: &HttpRequest,
    pending: &PendingAuth,
    code: &str,
) -> Result<(UserRow, bool), Error> {
    let config = context.config();
    let provider = config.auth.oidc.as_ref().ok_or_else(|| {
        human_errors::user(
            "This server does not federate to an identity provider.",
            &["Configure [auth.oidc], or sign in with a passkey."],
        )
    })?;

    let discovery = oidc::discovery(context, provider).await?;
    let tokens_from_provider = oidc::exchange_code(
        &context.http_client(),
        provider,
        &discovery,
        code,
        &pending.redirect_uri,
        Some(&pending.verifier),
    )
    .await?;

    // The nonce is checked here and nowhere else: it is what binds the ID token
    // to this flow rather than to any other the provider has issued.
    let claims = oidc::validate_token(
        context,
        provider,
        &tokens_from_provider.id_token,
        Some(&pending.nonce),
    )
    .await?;

    let identity = oidc::identity_from_claims(provider, &discovery.issuer, &claims)?;
    let filterable = oidc::filterable_claims(&claims);
    let acl = evaluate(
        &config.auth,
        &AuthRequestFilter {
            method: request.method().as_str(),
            path: request.path(),
            client_ip: client_ip(
                config.server.trust_proxy,
                request.headers(),
                request.peer_addr(),
            ),
            headers: request.headers(),
            claims: Some(&filterable),
            username: identity.username.as_str(),
            source: "oidc",
        },
    );

    if !acl.allowed {
        record(context, AuditOutcome::Denied, &identity.username).await;

        return Err(human_errors::user(
            "Your account is not permitted to sign in to this server.",
            &["Ask an administrator which directory groups this server admits."],
        ));
    }

    let user = users::provision(
        context.db(),
        provider,
        &identity,
        acl.is_admin,
        config.auth.anon_group_default,
    )
    .await?;

    let is_admin = user.is_effective_admin() || acl.is_admin;

    record(context, AuditOutcome::Success, &user.username).await;

    Ok((user, is_admin))
}

/// Issues our session, stores it in cookies, and sends the browser onwards.
async fn complete(
    context: &AppContext,
    request: &HttpRequest,
    pending: &PendingAuth,
    user: &UserRow,
    is_admin: bool,
) -> HttpResponse {
    let issued = tokens::issue_session(context, user, is_admin, Some("login")).await;

    let session = match issued {
        Ok(session) => session,
        Err(err) => {
            error!(error = %err, "Could not issue a session for a completed sign-in.");
            context.session().record_human_error(&err);

            return session::callback_refused(request);
        }
    };

    let mut response = match &pending.kind {
        PendingKind::Browser { return_to } => {
            redirect(return_to.as_deref().unwrap_or(DEFAULT_RETURN_TO))
        }
        code_request => deliver_code(context, user, is_admin, code_request).await,
    };

    // On the authorization-code path the cookies are what lets the *next*
    // `/oauth/authorize` skip the provider entirely; on the browser path they
    // are the session itself.
    for value in cookies::access_cookies(&session.token) {
        if let Ok(header) = actix_web::http::header::HeaderValue::from_str(&value) {
            response.headers_mut().append(SET_COOKIE, header);
        }
    }

    for value in cookies::clear_state_cookie() {
        if let Ok(header) = actix_web::http::header::HeaderValue::from_str(&value) {
            response.headers_mut().append(SET_COOKIE, header);
        }
    }

    response
}

/// The base URL this installation is reached on.
async fn base_url(context: &AppContext, request: &HttpRequest) -> Option<String> {
    let config = context.config();

    match settings::base_url(&config, context.db()).await {
        Ok(stored) => stored,
        Err(err) => {
            warn!(error = %err, "Could not read the stored base URL.");
            context.session().record_human_error(&err);

            None
        }
    }
    .or_else(|| request_base_url(config.server.trust_proxy, request))
}

/// Whether a `returnTo` is somewhere on this site.
///
/// A single leading slash and no second one: `//evil.example.com` is a
/// protocol-relative URL, and a redirect to it off the end of a sign-in is a
/// phishing page wearing this server's address bar.
fn is_local_path(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//") && !path.contains('\\')
}

/// Writes a sign-in to the audit log.
async fn record(context: &AppContext, outcome: AuditOutcome, who: &Username) {
    let entry = AuditEntry::new(AuditCategory::Authentication, "login", outcome).subject(who);

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a sign-in in the audit log.");
        context.session().record_human_error(&err);
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::header::LOCATION;

    use super::*;

    #[test]
    fn a_return_to_may_only_be_somewhere_on_this_site() {
        for path in ["/", "/admin", "/admin/devices?tab=1"] {
            assert!(is_local_path(path), "{path}");
        }

        for path in [
            "//evil.example.com",
            "https://evil.example.com",
            "/\\evil.example.com",
            "evil.example.com",
            "",
        ] {
            assert!(
                !is_local_path(path),
                "{path} would be an open redirect on the end of a sign-in",
            );
        }
    }

    #[test]
    fn the_callback_path_is_the_one_the_redirect_uri_is_built_from() {
        // Named once, because the provider compares the value sent at
        // authorization with the value sent at redemption byte for byte.
        assert_eq!(CALLBACK_PATH, "/login/redirect");
    }

    #[actix_web::test]
    async fn a_callback_with_no_state_cookie_is_refused_and_says_nothing() {
        let server = crate::testing::TestServer::start().await;
        let request = actix_web::test::TestRequest::get()
            .uri("/login/redirect?code=x&state=y")
            .app_data(web::Data::new(server.context.clone()))
            .to_http_request();

        let response = login_redirect(
            request,
            web::Data::new(server.context.clone()),
            Some(web::Query(CallbackQuery {
                code: Some("x".to_string()),
                state: Some("y".to_string()),
            })),
        )
        .await;

        assert_eq!(response.status().as_u16(), 400);
        assert!(
            response.headers().get(LOCATION).is_none(),
            "a refusal must not bounce the browser anywhere",
        );
    }

    #[actix_web::test]
    async fn an_installation_with_no_provider_has_nowhere_to_send_anybody() {
        let server = crate::testing::TestServer::start().await;
        let request = actix_web::test::TestRequest::get()
            .uri("/login/auth")
            .app_data(web::Data::new(server.context.clone()))
            .to_http_request();

        let response = login_auth(request, web::Data::new(server.context.clone()), None).await;

        assert_eq!(response.status().as_u16(), 404);
    }
}
