//! `grant_type=authorization_code` — redeeming a code from
//! `GET /oauth/authorize`.
//!
//! Here rather than beside the password grant in [`crate::marti::oauth`]
//! because it is *ours*: the password and refresh grants are shapes TAK Server
//! defined and CloudTAK parses, while this one is the tail of the
//! authorization-code flow this module tree implements, and it is the only
//! grant that mints an ID token.
//!
//! # What has to hold, and in which order
//!
//! 1. The client is registered. Checked before the code is looked at, so a
//!    client removed from the configuration cannot redeem a code issued while
//!    it still was.
//! 2. The client authenticated, when it is confidential ([`super::clients`]).
//!    Before the code, so that guessing a secret never tells the guesser
//!    whether a code exists — and rate limited, because it is a secret
//!    somebody could otherwise guess at indefinitely.
//! 3. A public client presented a proof-key verifier at all. A confidential one
//!    needs one only when it registered a challenge; see [`super::codes`].
//! 4. The code's own three bindings — client, redirect URI, proof key — and its
//!    single use, all inside the transaction that spends it.
//!
//! Every refusal of (1), (3) and (4) is the same `invalid_grant`, whichever
//! check failed. (2) is `invalid_client`, because a client that cannot
//! authenticate has to be told that rather than left retrying a good code.
//!
//! # What comes back
//!
//! The session, plus `scope` and — when `openid` was granted — `id_token`. The
//! **password** grant's body is untouched by all of this and is pinned by
//! `compat/oauth.md` §1.

use actix_web::{HttpRequest, HttpResponse};

use crate::auth::{RateLimiter, tokens};
use crate::prelude::*;
use crate::web::helpers::request::client_address;

use super::codes::{self, CodeError};
use super::responses::{
    TOKEN_TYPE, granted, invalid_client, invalid_grant, oauth_error, rate_limited, unavailable,
};
use super::{clients, id_token, scopes};

/// What a client identifier is prefixed with in the rate limiter, so that it
/// cannot collide with the usernames the password grant counts.
const CLIENT_SUBJECT_PREFIX: &str = "oauth-client:";

/// The fields of a token request this grant reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct Form<'a> {
    /// The code from `/oauth/authorize`.
    pub code: Option<&'a str>,
    /// The URI the code was delivered to, repeated so it can be compared.
    pub redirect_uri: Option<&'a str>,
    /// Which registered client is asking, when it is not in a Basic header.
    pub client_id: Option<&'a str>,
    /// `client_secret_post`, when it is not in a Basic header.
    pub client_secret: Option<&'a str>,
    /// The proof-key verifier, when the client registered a challenge.
    pub code_verifier: Option<&'a str>,
}

/// Redeems an authorization code.
pub async fn grant(
    request: &HttpRequest,
    context: &AppContext,
    limiter: Option<&RateLimiter>,
    form: Form<'_>,
) -> HttpResponse {
    let (Some(code), Some(redirect_uri)) = (form.code, form.redirect_uri) else {
        return oauth_error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "invalid_request",
            "An authorization_code grant needs a code, a redirect_uri and a client_id.",
        );
    };

    let Some(presented) = clients::presented(request.headers(), form.client_id, form.client_secret)
    else {
        return invalid_client();
    };

    let config = context.config();

    // Before the code is looked at, so that a client removed from the
    // configuration cannot redeem one issued while it was still registered.
    let Some(client) = config.auth.oauth.client(&presented.client_id) else {
        debug!(client = %presented.client_id, "Refused a code grant from an unregistered client.");

        return invalid_grant();
    };

    let address = client_address(
        config.server.trust_proxy,
        request.headers(),
        request.peer_addr(),
    );

    // Only a confidential client authenticates, so only a confidential client
    // is counted. The subject is namespaced because the limiter is keyed by
    // (address, subject) and the password grant's subject is a *username*: an
    // unnamespaced client identifier equal to somebody's username would let a
    // request that needs no credential clear the failures of one that does.
    if !client.public {
        let Some(limiter) = limiter else {
            // A client secret is a guessable credential, so it is not checked
            // at all on a listener that installed no limiter.
            error!("The token endpoint is mounted on a listener with no rate limiter.");

            return unavailable();
        };
        let subject = format!("{CLIENT_SUBJECT_PREFIX}{}", presented.client_id);

        if let Err(retry_after) = limiter.check(address, &subject) {
            return rate_limited(retry_after);
        }

        if !clients::authenticates(client, &presented) {
            warn!(client = %client.id, "Refused a code grant that did not authenticate.");
            limiter.record_failure(address, &subject);

            return invalid_client();
        }

        limiter.record_success(address, &subject);
    }

    // A public client holds no secret, so its proof key is the only thing
    // standing between an intercepted code and a session — and a missing
    // verifier has to be refused here rather than reaching a redemption that
    // would treat it as a code issued without one.
    if client.public && form.code_verifier.is_none_or(str::is_empty) {
        debug!(client = %client.id, "Refused a code grant from a public client with no verifier.");

        return invalid_grant();
    }

    let redemption = match codes::redeem(
        context.db(),
        code,
        &presented.client_id,
        redirect_uri,
        form.code_verifier.filter(|value| !value.is_empty()),
    )
    .await
    {
        Ok(redemption) => redemption,
        Err(CodeError::Unavailable(err)) => {
            error!(error = %err, "Could not redeem an authorization code.");
            context.session().record_human_error(&err);

            return unavailable();
        }
        Err(CodeError::Invalid) => {
            debug!(client = %client.id, "Refused an authorization code.");

            return invalid_grant();
        }
    };

    let Ok(Some(user)) = context.db().users().get(redemption.user_id).await else {
        return invalid_grant();
    };

    if user.disabled {
        return invalid_grant();
    }

    // Derived from the account now rather than from the scope recorded when
    // the code was issued, exactly as `tokens::rotate` does: a code stands for
    // up to ten minutes, and somebody demoted inside that window must not be
    // handed the administrative scope their code still remembers. The recorded
    // scope is not simply trusted — it is the *ceiling*, so a code minted for an
    // ordinary session cannot become an administrative one either.
    let is_admin = user.is_effective_admin() && tokens::grants_admin(&redemption.scope);
    let rustak_scope = tokens::scope_for(is_admin);
    let session = match tokens::issue_session(context, &user, is_admin, Some(&client.id)).await {
        Ok(session) => session,
        Err(err) => {
            error!(error = %err, "Could not issue a session for an authorization code.");
            context.session().record_human_error(&err);

            return unavailable();
        }
    };

    let mut body = serde_json::json!({
        "access_token": session.token,
        "token_type": TOKEN_TYPE,
        "expires_in": session.expires_in,
        "refresh_token": session.refresh_token,
        // The OpenID scopes when the request asked for any, and rustak's own
        // otherwise — which is what this grant has always answered.
        "scope": redemption.oidc_scope.clone().unwrap_or(rustak_scope),
    });

    if let Some(oidc_scope) = redemption
        .oidc_scope
        .as_deref()
        .filter(|granted| scopes::grants(granted, scopes::OPENID))
    {
        let Some(token) = mint_id_token(
            context,
            &user,
            is_admin,
            &client.id,
            oidc_scope,
            &redemption,
            &session,
        )
        .await
        else {
            return unavailable();
        };

        body["id_token"] = token.into();
    }

    info!(
        client = %client.id,
        username = %user.username,
        "Exchanged an authorization code for a session.",
    );

    granted(body)
}

/// The ID token beside the session, or [`None`] when one could not be minted.
///
/// A failure here fails the whole grant rather than answering without it: a
/// relying party that asked for `openid` and got a response with no `id_token`
/// reports "the provider is not an OpenID provider", which is a worse thing to
/// debug than a `503`.
#[allow(clippy::too_many_arguments)]
async fn mint_id_token(
    context: &AppContext,
    user: &crate::db::repos::UserRow,
    is_admin: bool,
    client_id: &str,
    granted_scope: &str,
    redemption: &codes::Redemption,
    session: &rustak_api::TokenResponse,
) -> Option<String> {
    let Ok(jwt) = context.jwt() else {
        return None;
    };

    let issued = id_token::issue(
        context.db(),
        &jwt,
        &context.config().auth.oauth,
        &id_token::Request {
            user,
            is_admin,
            client_id,
            granted: granted_scope,
            nonce: redemption.nonce.as_deref(),
            expires_at: chrono::Utc::now().timestamp()
                + i64::try_from(session.expires_in).unwrap_or(0),
            auth_time: Some(redemption.authorized_at),
        },
    )
    .await;

    match issued {
        Ok(token) => Some(token),
        Err(err) => {
            error!(error = %err, "Could not issue an ID token.");
            context.session().record_human_error(&err);

            None
        }
    }
}
