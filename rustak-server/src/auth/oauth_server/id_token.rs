//! The ID token rustak issues when a client asked for `openid`.
//!
//! An ID token is not a credential for this server and never becomes one — it
//! is a signed statement *about* a sign-in, for the relying party that asked
//! for it. [`JwtIssuer::verify`] deserialises the access-token claims, which
//! require a `jti`, a `scope` and an `nbf` this token does not carry, and an
//! audience of `[auth] audience` rather than a client identifier. Presenting
//! one as a bearer token is refused for three independent reasons, and the test
//! at the bottom of this file asserts it.
//!
//! # What is in it, and why
//!
//! | Claim | Value |
//! |---|---|
//! | `iss` | `[auth] issuer` — the same `iss` the access token carries |
//! | `sub` | the rustak username, which is also the access token's `sub` and the certificate's common name |
//! | `aud` | the `client_id` it was issued to |
//! | `exp`, `iat` | the access token's own window, so the two expire together |
//! | `auth_time` | when the session behind it began, and `iat` when that is not known |
//! | `nonce` | echoed byte for byte when the client sent one, absent when it did not |
//! | `name`, `email`, `groups` | [`super::claims`], according to the granted scopes |
//!
//! `aud` is the client identifier and nothing else. A relying party checks it
//! against its own, which is what stops a token minted for one client being
//! replayed at another — so it may never be widened to an array of every
//! registered client "for convenience".
//!
//! The signature is RS256 with the same key the access tokens use, published at
//! `/oauth/jwks`. Unlike an access token, the header carries a `kid`: an ID
//! token is read by relying-party libraries rather than by CloudTAK's hand
//! parser, so there is no 27-byte header to preserve and a `kid` is how a
//! library picks the right key out of the set.
//!
//! [`JwtIssuer::verify`]: crate::auth::JwtIssuer::verify

use chrono::{DateTime, Utc};

use crate::auth::JwtIssuer;
use crate::config::OAuthServerConfig;
use crate::db::Database;
use crate::db::repos::UserRow;
use crate::prelude::*;

use super::claims;

/// Everything one ID token is minted from.
pub struct Request<'a> {
    /// Whose sign-in it describes.
    pub user: &'a UserRow,

    /// Whether this session carries administrative rights, which decides the
    /// marker group in `groups`.
    pub is_admin: bool,

    /// The client it is issued to, which becomes `aud`.
    pub client_id: &'a str,

    /// The OpenID scopes granted, which decide the claims released.
    pub granted: &'a str,

    /// The client's `nonce`, echoed when it sent one.
    pub nonce: Option<&'a str>,

    /// When the access token beside it expires, so the two expire together.
    pub expires_at: i64,

    /// When the session behind this token began, when that is known.
    pub auth_time: Option<DateTime<Utc>>,
}

/// Mints an ID token.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the channel read fails or the
/// token cannot be signed.
#[instrument("auth.oauth.id_token.issue", skip_all, fields(client = %request.client_id), err(Display))]
pub async fn issue(
    db: &Database,
    jwt: &JwtIssuer,
    oauth: &OAuthServerConfig,
    request: &Request<'_>,
) -> Result<String, Error> {
    let issued_at = Utc::now().timestamp();
    let mut claims = claims::released(
        db,
        oauth,
        request.user,
        request.is_admin,
        Some(request.granted),
    )
    .await?;

    claims.insert("iss".to_string(), jwt.issuer().into());
    claims.insert("aud".to_string(), request.client_id.into());
    claims.insert("exp".to_string(), request.expires_at.into());
    claims.insert("iat".to_string(), issued_at.into());
    claims.insert(
        "auth_time".to_string(),
        request
            .auth_time
            .map_or(issued_at, |when| when.timestamp())
            .into(),
    );

    // Byte for byte, or not at all. A relying party compares it with the value
    // it generated, and a token whose nonce was normalised in any way is one it
    // will refuse for a reason nobody could see.
    if let Some(nonce) = request.nonce {
        claims.insert("nonce".to_string(), nonce.into());
    }

    jwt.sign_id_token(&serde_json::Value::Object(claims))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::NewUser;
    use crate::testing::TestServer;

    /// A server, an account and the issuer the tokens come from.
    async fn fixture(is_admin: bool) -> (TestServer, UserRow) {
        let server = TestServer::start().await;
        let user = server
            .db()
            .users()
            .create(NewUser {
                display_name: Some("Ada Lovelace".to_string()),
                email: Some("ada@example.com".to_string()),
                is_admin,
                ..NewUser::person(Username::parse("ada").unwrap())
            })
            .await
            .unwrap();

        crate::identity::groups::join_default(server.db(), user.id)
            .await
            .unwrap();

        (server, user)
    }

    /// The claims of a token, without checking its signature.
    fn payload(token: &str) -> serde_json::Value {
        use base64::Engine as _;

        let segment = token.split('.').nth(1).expect("a JWT has three segments");
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(segment)
            .expect("the payload is base64url");

        serde_json::from_slice(&decoded).expect("the payload is JSON")
    }

    /// The header of a token.
    fn header(token: &str) -> serde_json::Value {
        use base64::Engine as _;

        let segment = token.split('.').next().expect("a JWT has three segments");
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(segment)
            .expect("the header is base64url");

        serde_json::from_slice(&decoded).expect("the header is JSON")
    }

    async fn issued(server: &TestServer, user: &UserRow, request: Request<'_>) -> String {
        issue(
            server.db(),
            &server.jwt().unwrap(),
            &server.config().auth.oauth,
            &Request { user, ..request },
        )
        .await
        .expect("an ID token")
    }

    fn request<'a>(user: &'a UserRow, granted: &'a str, nonce: Option<&'a str>) -> Request<'a> {
        Request {
            user,
            is_admin: false,
            client_id: "cloudtak",
            granted,
            nonce,
            expires_at: Utc::now().timestamp() + 3600,
            auth_time: None,
        }
    }

    #[actix_web::test]
    async fn the_token_names_this_server_the_account_and_the_client_it_was_issued_to() {
        let (server, user) = fixture(false).await;
        let token = issued(
            &server,
            &user,
            request(&user, "openid profile email groups", Some("a-nonce")),
        )
        .await;

        let claims = payload(&token);

        assert_eq!(claims["iss"], server.jwt().unwrap().issuer());
        assert_eq!(claims["sub"], "ada");
        assert_eq!(claims["preferred_username"], "ada");
        assert_eq!(
            claims["aud"], "cloudtak",
            "a string, never an array: it is what stops a replay at another client",
        );
        assert_eq!(claims["nonce"], "a-nonce");
        assert_eq!(claims["name"], "Ada Lovelace");
        assert_eq!(claims["email"], "ada@example.com");
        assert!(claims["groups"].is_array());
        assert!(claims["iat"].is_i64());
        assert!(claims["exp"].as_i64().unwrap() > claims["iat"].as_i64().unwrap());
    }

    #[actix_web::test]
    async fn a_flow_that_sent_no_nonce_gets_a_token_with_no_nonce_claim() {
        // Rather than an empty string, which a relying party comparing it with
        // its own absent value would read as a mismatch.
        let (server, user) = fixture(false).await;
        let token = issued(&server, &user, request(&user, "openid", None)).await;

        assert!(payload(&token).get("nonce").is_none());
    }

    #[actix_web::test]
    async fn auth_time_is_the_sessions_own_start_when_it_is_known() {
        let (server, user) = fixture(false).await;
        let began = Utc::now() - chrono::Duration::hours(2);
        let token = issued(
            &server,
            &user,
            Request {
                auth_time: Some(began),
                ..request(&user, "openid", None)
            },
        )
        .await;

        let claims = payload(&token);

        assert_eq!(claims["auth_time"], began.timestamp());
        assert!(claims["auth_time"].as_i64().unwrap() < claims["iat"].as_i64().unwrap());
    }

    #[actix_web::test]
    async fn the_header_carries_a_kid_so_a_library_can_pick_the_key() {
        let (server, user) = fixture(false).await;
        let token = issued(&server, &user, request(&user, "openid", None)).await;
        let header = header(&token);

        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["kid"], server.jwt().unwrap().active_kid());
    }

    #[actix_web::test]
    async fn an_id_token_is_not_a_credential_for_this_server() {
        // Three independent reasons — no `jti`, no `nbf`, and an audience that
        // is the client rather than `[auth] audience` — and this asserts the
        // consequence rather than any one of them.
        let (server, user) = fixture(true).await;
        let token = issued(
            &server,
            &user,
            request(&user, "openid profile email groups", None),
        )
        .await;

        assert!(
            server.jwt().unwrap().verify(&token).is_err(),
            "an ID token presented as a bearer token has to be refused",
        );
    }
}
