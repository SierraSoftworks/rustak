//! `GET /oauth/authorize` — the authorization-code flow, as the server side of
//! it.
//!
//! # What is validated before anything can redirect
//!
//! The client and the redirect URI are resolved **first**, and a failure of
//! either is answered here rather than by redirecting. That ordering is the
//! difference between an authorization server and an open redirector: an error
//! sent to an unvalidated `redirect_uri` is an attacker-chosen bounce off a
//! host people trust, and `error=` in the query string does not make it less
//! of one. Once both are known good, every later refusal goes back to the
//! client the way the specification says, carrying its `state`.
//!
//! # Why proof key for code exchange is not optional for a public client
//!
//! Every client registered here is public unless it says otherwise, and a
//! public client has no secret — so without a proof key the only thing standing
//! between an intercepted code and a session is the code's own secrecy, and the
//! code travels through a browser redirect. `S256` only: `plain` puts the
//! verifier in the authorization request, which is the request an interceptor
//! already has.
//!
//! A **confidential** client (`public = false`) is registered with a secret it
//! presents at `/oauth/token`, and that secret is the binding an interceptor
//! does not have — so its proof key is optional, and enforced only when it sent
//! a `code_challenge`. That is the trade the specification makes, and serving
//! it is the difference between working with CloudTAK's relying party and not:
//! it sends no `code_challenge` at all (`compat/oauth.md` §6).
//!
//! # The OpenID scopes are a separate string
//!
//! `scope` here is read as an OpenID request ([`super::scopes`]) and recorded
//! on the code as what may be *said* about the account. It is never the rustak
//! scope, which is derived from the account at [`deliver_code`] and clamped
//! again at redemption: a client that could widen one by asking for the other
//! would turn "tell me this person's email address" into administrative
//! access.
//!
//! # Consent
//!
//! There is no consent screen. Every client here was registered in this
//! server's own configuration file by the operator, which makes them all
//! first-party; a dialogue asking somebody to approve the administrator's own
//! admin UI teaches people to click through dialogues.

use actix_web::http::StatusCode;
use actix_web::{HttpRequest, HttpResponse, web};

use crate::auth::resolve::{RequestFacts, Resolved, bearer};
use crate::auth::tokens;
use crate::config::OAuthClient;
use crate::db::repos::UserRow;
use crate::prelude::*;
use crate::web::helpers::request::client_ip;

use super::codes::{self, NewCode, S256};
use super::login;
use super::scopes;
use super::state::PendingKind;

/// The query `GET /oauth/authorize` accepts.
///
/// Every field is optional so that a missing one is refused in the OAuth error
/// shape, rather than by actix's extractor with a `text/plain` body a client
/// cannot read.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizeQuery {
    #[serde(default)]
    pub response_type: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub code_challenge: Option<String>,
    #[serde(default)]
    pub code_challenge_method: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    /// The relying party's `nonce`, which its ID token has to echo.
    #[serde(default)]
    pub nonce: Option<String>,
}

/// `GET /oauth/authorize`.
pub async fn authorize(
    request: HttpRequest,
    context: web::Data<AppContext>,
    query: Option<web::Query<AuthorizeQuery>>,
) -> HttpResponse {
    let Some(query) = query else {
        return refused("The authorization request could not be read.");
    };

    let config = context.config();

    let (Some(client_id), Some(redirect_uri)) =
        (query.client_id.as_deref(), query.redirect_uri.as_deref())
    else {
        return refused("An authorization request needs a client_id and a redirect_uri.");
    };

    let Some(client) = config.auth.oauth.client(client_id) else {
        debug!(client = %client_id, "Refused an authorization request from an unregistered client.");

        return refused("That client is not registered with this server.");
    };

    // Before any redirect can happen: see the module documentation.
    if !client.allows(redirect_uri) {
        warn!(
            client = %client_id,
            "Refused an authorization request naming a redirect URI the client is not registered for.",
        );

        return refused("That redirect URI is not registered for this client.");
    }

    if query.response_type.as_deref() != Some("code") {
        return error_redirect(redirect_uri, "unsupported_response_type", &query.state);
    }

    let Ok(challenge) = proof_key(client, &query) else {
        return error_redirect(redirect_uri, "invalid_request", &query.state);
    };

    let pending = PendingKind::AuthorizationCode {
        client_id: client_id.to_string(),
        redirect_uri: redirect_uri.to_string(),
        client_state: query.state.clone(),
        code_challenge: challenge,
        oidc_scope: scopes::granted(query.scope.as_deref()),
        client_nonce: query.nonce.clone(),
    };

    match browser_session(context.get_ref(), &request).await {
        // Already signed in here, so there is nothing to ask anybody: mint the
        // code and send them back.
        Some(resolved) => {
            deliver_code(
                context.get_ref(),
                &resolved.user,
                resolved.principal.is_admin,
                &pending,
            )
            .await
        }

        // Nobody yet. The identity provider establishes who they are and
        // `/login/redirect` finishes this same request.
        None if config.auth.oidc.is_some() => {
            login::begin_federation(context.get_ref(), &request, pending).await
        }

        None => {
            warn!(
                client = %client_id,
                "An authorization request arrived with no session and no identity provider to establish one.",
            );

            error_redirect(redirect_uri, "access_denied", &query.state)
        }
    }
}

/// The browser session on this request, if there is one.
///
/// **Only** the `access_token_N` cookies the `/login/*` flow and the admin UI
/// set — never an `Authorization` header. An authorization code is an upgrade:
/// it is exchanged for a *session*, with a refresh family that outlives the
/// credential that asked for it by weeks. Minting one for any bearer meant that
/// two extra requests turned a password-grant access token — which
/// `compat/oauth.md` §1 deliberately issues with no refresh token — into a
/// 30-day refresh family (R-01 H2). Only a caller who is already holding a
/// browser session has something to upgrade, and a browser session is a cookie.
///
/// A failure to resolve the cookie is [`None`] rather than an error: it means
/// "nobody is signed in here yet", which is the federation path, not a refusal.
async fn browser_session(context: &AppContext, request: &HttpRequest) -> Option<Resolved> {
    let token = super::access_token_from_cookies(request.headers())?;
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

    match bearer(context, &token, &facts).await {
        Ok(resolved) => Some(resolved),
        Err(failure) => {
            debug!(reason = ?failure, "A session cookie established no identity at /oauth/authorize.");

            None
        }
    }
}

/// Mints a code for a signed-in caller and redirects to the client.
///
/// Public so that `/login/redirect` can finish an authorization request the
/// identity provider interrupted.
pub async fn deliver_code(
    context: &AppContext,
    user: &UserRow,
    is_admin: bool,
    pending: &PendingKind,
) -> HttpResponse {
    let PendingKind::AuthorizationCode {
        client_id,
        redirect_uri,
        client_state,
        code_challenge,
        oidc_scope,
        client_nonce,
    } = pending
    else {
        error!("An authorization code was asked for by a flow that never requested one.");

        return refused("That sign-in could not be completed.");
    };

    let scope = tokens::scope_for(is_admin);

    let issued = codes::issue(
        context.db(),
        NewCode {
            client_id: client_id.clone(),
            user_id: user.id,
            redirect_uri: redirect_uri.clone(),
            scope,
            oidc_scope: oidc_scope.clone(),
            nonce: client_nonce.clone(),
            code_challenge: code_challenge.clone(),
        },
    )
    .await;

    let code = match issued {
        Ok(code) => code,
        Err(err) => {
            error!(error = %err, "Could not issue an authorization code.");
            context.session().record_human_error(&err);

            return error_redirect(redirect_uri, "server_error", client_state);
        }
    };

    info!(
        client = %client_id,
        username = %user.username,
        "Issued an authorization code.",
    );

    let mut url = format!("{redirect_uri}{}code={code}", separator(redirect_uri));

    if let Some(state) = client_state {
        url.push_str(&format!("&state={}", encode(state)));
    }

    redirect(&url)
}

/// The client's proof-key challenge, when the request carries a usable one.
///
/// Three answers rather than two. `Ok(Some(_))` is a challenge to register;
/// `Ok(None)` is a confidential client that sent none, which is allowed because
/// it will present a secret at `/oauth/token` instead; `Err(())` is a request
/// that may not proceed at all.
///
/// A **public** client with no challenge is `Err(())` and always will be: it
/// holds no secret, so an intercepted code would be a session. `plain` is
/// `Err(())` for every client, because it puts the verifier in the
/// authorization request — which is the request an interceptor already has.
#[allow(clippy::result_unit_err)]
fn proof_key(client: &OAuthClient, query: &AuthorizeQuery) -> Result<Option<String>, ()> {
    let method = query.code_challenge_method.as_deref();

    match query.code_challenge.as_deref() {
        Some(challenge) if !challenge.is_empty() => {
            if method.is_some_and(|method| method != S256) {
                debug!(method = ?method, "Refused a proof-key method that is not S256.");

                return Err(());
            }

            Ok(Some(challenge.to_string()))
        }
        _ if client.public => {
            debug!(
                client = %client.id,
                "Refused an authorization request from a public client that carries no proof key.",
            );

            Err(())
        }
        _ => Ok(None),
    }
}

/// An error sent back to a validated redirect URI.
pub fn error_redirect(redirect_uri: &str, error: &str, state: &Option<String>) -> HttpResponse {
    let mut url = format!("{redirect_uri}{}error={error}", separator(redirect_uri),);

    if let Some(state) = state {
        url.push_str(&format!("&state={}", encode(state)));
    }

    redirect(&url)
}

/// A refusal answered here rather than by redirecting anywhere.
pub fn refused(description: &str) -> HttpResponse {
    HttpResponse::build(StatusCode::BAD_REQUEST)
        .insert_header((actix_web::http::header::CONTENT_TYPE, "application/json"))
        .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
        .body(
            serde_json::json!({
                "error": "invalid_request",
                "error_description": description,
            })
            .to_string(),
        )
}

/// A `302`, with nothing cached.
pub fn redirect(location: &str) -> HttpResponse {
    let mut response = HttpResponse::Found();

    response.insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"));

    match actix_web::http::header::HeaderValue::from_str(location) {
        Ok(value) => response
            .insert_header((actix_web::http::header::LOCATION, value))
            .finish(),
        Err(_) => {
            error!("Refused to redirect to a location that is not a usable header value.");

            refused("That sign-in could not be completed.")
        }
    }
}

/// `?` or `&`, depending on what the registered URI already carries.
fn separator(redirect_uri: &str) -> char {
    if redirect_uri.contains('?') { '&' } else { '?' }
}

/// Percent-encodes a query value.
///
/// Only `state` goes through here — an opaque string the client chose — and it
/// has to come back byte for byte or the client cannot match it to the request
/// it made. Public so that [`super::session`]'s sign-out redirect returns a
/// client's `state` by the same rule this one does.
pub fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(challenge: Option<&str>, method: Option<&str>) -> AuthorizeQuery {
        AuthorizeQuery {
            response_type: Some("code".to_string()),
            client_id: Some("app".to_string()),
            redirect_uri: Some("https://app.example.com/cb".to_string()),
            state: Some("the-clients-state".to_string()),
            code_challenge: challenge.map(str::to_string),
            code_challenge_method: method.map(str::to_string),
            scope: None,
            nonce: None,
        }
    }

    fn client(public: bool) -> OAuthClient {
        OAuthClient {
            id: "app".to_string(),
            redirect_uris: vec!["https://app.example.com/cb".to_string()],
            public,
            secret: (!public).then(|| "s3cret".to_string()),
            post_logout_redirect_uris: Vec::new(),
        }
    }

    #[test]
    fn a_public_client_cannot_start_a_flow_without_a_proof_key() {
        // It holds no secret, so without one an intercepted code is a session.
        assert_eq!(proof_key(&client(true), &query(None, None)), Err(()));
        assert_eq!(
            proof_key(&client(true), &query(Some(""), Some("S256"))),
            Err(()),
        );
    }

    #[test]
    fn plain_is_not_a_proof_key_this_server_will_register() {
        // `plain` puts the verifier in the authorization request, which is the
        // request an interceptor already has — for either kind of client.
        assert_eq!(
            proof_key(&client(true), &query(Some("a-challenge"), Some("plain"))),
            Err(()),
        );
        assert_eq!(
            proof_key(&client(false), &query(Some("a-challenge"), Some("plain"))),
            Err(()),
        );
    }

    #[test]
    fn an_s256_challenge_is_taken_as_given_and_a_missing_method_means_s256() {
        assert_eq!(
            proof_key(&client(true), &query(Some("a-challenge"), Some("S256"))),
            Ok(Some("a-challenge".to_string())),
        );
        assert_eq!(
            proof_key(&client(true), &query(Some("a-challenge"), None)),
            Ok(Some("a-challenge".to_string())),
        );
    }

    #[test]
    fn a_confidential_client_may_start_a_flow_without_a_proof_key() {
        // Its secret at `/oauth/token` is the binding an interceptor does not
        // have, which is what the specification trades the proof key for — and
        // CloudTAK's relying party sends no `code_challenge` at all.
        assert_eq!(proof_key(&client(false), &query(None, None)), Ok(None));
        assert_eq!(
            proof_key(&client(false), &query(Some(""), Some("S256"))),
            Ok(None),
        );
    }

    #[test]
    fn a_confidential_client_that_did_send_one_is_still_held_to_it() {
        // Offering a proof key and then not being asked for it would make the
        // control optional at the attacker's choice rather than the client's.
        assert_eq!(
            proof_key(&client(false), &query(Some("a-challenge"), Some("S256"))),
            Ok(Some("a-challenge".to_string())),
        );
    }

    #[test]
    fn the_openid_scopes_are_read_from_the_request_and_narrowed() {
        assert_eq!(
            scopes::granted(Some("openid profile email groups offline_access")).as_deref(),
            Some("openid profile email groups"),
        );
        assert_eq!(scopes::granted(None), None);
    }

    #[test]
    fn a_state_comes_back_byte_for_byte() {
        let response = error_redirect(
            "https://app.example.com/cb",
            "access_denied",
            &Some("a b&c=d".to_string()),
        );

        let location = response
            .headers()
            .get(actix_web::http::header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();

        assert_eq!(
            location,
            "https://app.example.com/cb?error=access_denied&state=a%20b%26c%3Dd",
        );
    }

    #[test]
    fn a_redirect_uri_that_already_has_a_query_gains_another_parameter() {
        let response = error_redirect(
            "https://app.example.com/cb?tenant=blue",
            "invalid_request",
            &None,
        );

        assert_eq!(
            response
                .headers()
                .get(actix_web::http::header::LOCATION)
                .unwrap(),
            "https://app.example.com/cb?tenant=blue&error=invalid_request",
        );
    }

    #[test]
    fn nothing_this_endpoint_answers_is_ever_cached() {
        // A cached authorization response is a code served twice from a proxy.
        for response in [
            refused("no"),
            error_redirect("https://app.example.com/cb", "invalid_request", &None),
            redirect("https://app.example.com/cb"),
        ] {
            assert_eq!(
                response
                    .headers()
                    .get(actix_web::http::header::CACHE_CONTROL)
                    .unwrap(),
                "no-store",
            );
        }
    }

    #[test]
    fn a_location_that_could_not_be_a_header_is_a_refusal_rather_than_a_panic() {
        let response = redirect("https://app.example.com/cb\r\nX-Injected: 1");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
