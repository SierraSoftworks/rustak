//! The `access_token_N` cookies TAK Server invented, and where a cookie may be
//! a credential at all.
//!
//! # Why the token is chopped up
//!
//! TAK Server splits the access token into 4000-character pieces and sets them
//! as `access_token_0`, `access_token_1`, … because a single cookie past about
//! 4 KiB is silently dropped by browsers and proxies. Its own client code
//! reassembles them in order, so a WebTAK-style page written against a real TAK
//! Server finds what it expects here. Ours are usually one chunk — an RS256
//! token over a 2048-bit key is well under the limit — but the naming is part
//! of the contract, not an artefact of the size.
//!
//! # Where a cookie is accepted
//!
//! [`cookies_allowed`] is the whole rule and it is deliberately a **path**
//! rule rather than a listener flag: `/login/*`, `/logout`, `/token/access` and
//! the TAK surface, never `/api/v1`.
//!
//! A cookie is attached by the browser to every request to this origin whatever
//! page caused it, which is the entirety of what cross-site request forgery is.
//! The admin API is bearer-only by construction — its middleware reads the
//! `Authorization` header and never looks at a cookie — so it has no such
//! surface, and nothing here may hand it one. The TAK surface accepts cookies
//! because a browser-based TAK client has no other way to call it, and it is
//! the same trade TAK Server makes on its own `:8446`.
//!
//! `SameSite=Lax` is what makes that trade survivable: the cookie is not sent
//! on a cross-site `POST` or `fetch` at all, only on a top-level navigation.

use actix_web::cookie::{Cookie, SameSite, time::Duration as CookieDuration};
use actix_web::http::header::HeaderMap;

use super::state::STATE_COOKIE;

/// The prefix of every chunk of a stored access token.
pub const ACCESS_PREFIX: &str = "access_token_";

/// How large one chunk may be, in characters. TAK Server's own number.
const CHUNK: usize = 4000;

/// How many chunks are looked for. A token needing more than this is not one
/// we issue, and an unbounded loop over attacker-supplied cookie names is not
/// something to write.
const MAX_CHUNKS: usize = 8;

/// The path the `state` cookie is scoped to: only the endpoints that use it.
const STATE_PATH: &str = "/login";

/// The path prefixes a cookie may authenticate a request on.
///
/// `/api/v1` is deliberately absent; see the module documentation.
const COOKIE_PREFIXES: &[&str] = &["/login", "/Marti", "/files/api"];

/// The exact paths a cookie may authenticate a request on.
///
/// `/oauth/authorize` is here and `/oauth` is not: the authorization endpoint
/// is a top-level navigation that has to know who is already signed in, and it
/// only ever hands a code to a **registered** redirect URI, so a cross-site
/// navigation to it delivers the code to the legitimate client and nowhere
/// else. `/oauth/token` carries a grant and has no business reading a cookie at
/// all.
const COOKIE_PATHS: &[&str] = &["/logout", "/token/access", "/oauth/authorize"];

/// Whether a cookie may be read as a credential for this path.
pub fn cookies_allowed(path: &str) -> bool {
    if COOKIE_PATHS.contains(&path) {
        return true;
    }

    COOKIE_PREFIXES.iter().any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// The `Set-Cookie` values that store `token`, in order.
///
/// `Secure` unconditionally rather than only on a TLS request: everything this
/// server serves a browser is served over TLS (`[web.public]` refuses plaintext
/// unless an operator says so twice), and a cookie that drops its `Secure`
/// attribute because a reverse proxy forwarded the request as HTTP is a session
/// handed to whoever is watching the network.
pub fn access_cookies(token: &str) -> Vec<String> {
    chunks(token)
        .enumerate()
        .map(|(index, chunk)| {
            let mut cookie = Cookie::new(format!("{ACCESS_PREFIX}{index}"), chunk.to_string());

            cookie.set_http_only(true);
            cookie.set_secure(true);
            cookie.set_same_site(SameSite::Lax);
            cookie.set_path("/");

            cookie.to_string()
        })
        .collect()
}

/// The `Set-Cookie` value that stores the `state` for this flow.
pub fn state_cookie(state: &str) -> String {
    let mut cookie = Cookie::new(STATE_COOKIE, state.to_string());

    cookie.set_http_only(true);
    cookie.set_secure(true);
    cookie.set_same_site(SameSite::Lax);
    // Narrower than the access token's `/`: nothing outside the sign-in flow
    // has any use for it, and a cookie sent where it is not needed is one more
    // place it can leak from.
    cookie.set_path(STATE_PATH);

    cookie.to_string()
}

/// The `Set-Cookie` value that removes the `state` cookie.
///
/// Set the moment a callback is consumed — successfully or not — so that a
/// browser is never left holding a value some later callback could be matched
/// against.
pub fn clear_state_cookie() -> String {
    expire(STATE_COOKIE, STATE_PATH)
}

/// The `Set-Cookie` values that remove every cookie this module sets.
///
/// Every `access_token*` cookie the request actually carried is named, rather
/// than a fixed list: a token that was stored as three chunks by an older build
/// has to be cleared as three chunks, or a stale piece is reassembled into
/// nonsense on the next request.
pub fn clearing_cookies(headers: &HeaderMap) -> Vec<String> {
    let mut names: Vec<String> = present(headers)
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("access_token"))
        .collect();

    names.sort();
    names.dedup();

    let mut cleared: Vec<String> = names.iter().map(|name| expire(name, "/")).collect();

    cleared.push(expire(STATE_COOKIE, STATE_PATH));
    cleared
}

/// The access token this request's cookies carry, reassembled.
///
/// Chunks are read in order and stop at the first gap, so a browser holding
/// `access_token_0` and a stale `access_token_2` presents the first chunk alone
/// and is refused, rather than presenting a token with a hole in it.
pub fn access_token_from_cookies(headers: &HeaderMap) -> Option<String> {
    let cookies = present(headers);
    let mut token = String::new();

    for index in 0..MAX_CHUNKS {
        let name = format!("{ACCESS_PREFIX}{index}");

        let Some((_, value)) = cookies.iter().find(|(held, _)| *held == name) else {
            break;
        };

        token.push_str(value);
    }

    // TAK Server itself only ever writes the chunked names; the unnumbered one
    // is accepted because a proxy or an operator's own script may set it, and
    // refusing it would be a difference nobody could debug.
    if token.is_empty() {
        token = cookies
            .iter()
            .find(|(name, _)| name == "access_token")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
    }

    (!token.is_empty()).then_some(token)
}

/// One named cookie from a request.
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    present(headers)
        .into_iter()
        .find(|(held, _)| held == name)
        .map(|(_, value)| value)
}

/// Every cookie on the request, as name and value.
fn present(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .get_all(actix_web::http::header::COOKIE)
        .filter_map(|value| value.to_str().ok())
        .flat_map(|header| header.split(';'))
        .filter_map(|pair| Cookie::parse_encoded(pair.trim().to_string()).ok())
        .map(|cookie| (cookie.name().to_string(), cookie.value().to_string()))
        .collect()
}

/// A `Set-Cookie` value that removes a cookie.
fn expire(name: &str, path: &str) -> String {
    let mut cookie = Cookie::new(name.to_string(), String::new());

    cookie.set_http_only(true);
    cookie.set_secure(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path(path.to_string());
    cookie.set_max_age(CookieDuration::ZERO);
    cookie.make_removal();

    cookie.to_string()
}

/// `token`, in pieces no browser will drop.
fn chunks(token: &str) -> impl Iterator<Item = &str> {
    // Byte offsets rather than `chars`: a token is base64url and ASCII by
    // construction, and slicing on a character boundary that does not exist
    // would panic rather than producing a shorter chunk.
    (0..token.len())
        .step_by(CHUNK)
        .map(move |start| &token[start..(start + CHUNK).min(token.len())])
}

#[cfg(test)]
mod tests {
    use actix_web::http::header::{COOKIE, HeaderValue};

    use super::*;

    /// A request carrying `pairs` as its cookies.
    fn with_cookies(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let joined = pairs
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");

        headers.insert(COOKIE, HeaderValue::from_str(&joined).unwrap());
        headers
    }

    #[test]
    fn the_admin_api_is_never_authenticated_by_a_cookie() {
        // It reads the Authorization header and nothing else, so it has no
        // cross-site request forgery surface. This is the assertion that stops
        // somebody giving it one.
        for path in [
            "/api/v1/me",
            "/api/v1/users",
            "/api/v1/auth/logout",
            "/api/v1",
        ] {
            assert!(!cookies_allowed(path), "{path}");
        }
    }

    #[test]
    fn a_cookie_reaches_the_sign_in_endpoints_and_the_tak_surface() {
        for path in [
            "/login",
            "/login/auth",
            "/login/redirect",
            "/logout",
            "/token/access",
            "/oauth/authorize",
            "/Marti/api/version",
            "/Marti/api/missions",
            "/files/api/config",
        ] {
            assert!(cookies_allowed(path), "{path}");
        }
    }

    #[test]
    fn the_token_endpoint_reads_no_cookie() {
        // It takes a grant. A cookie there would be a credential the browser
        // attaches to a cross-site POST, which is the whole of what the
        // `/api/v1` rule exists to avoid.
        for path in ["/oauth/token", "/oauth/jwks", "/oauth/token_key", "/oauth"] {
            assert!(!cookies_allowed(path), "{path}");
        }
    }

    #[test]
    fn a_path_that_only_looks_like_an_allowed_one_is_not_allowed() {
        for path in [
            "/loginary",
            "/logoutsomewhere",
            "/Martian",
            "/files/apis/config",
            "/",
            "/oauth/authorized",
        ] {
            assert!(!cookies_allowed(path), "{path}");
        }
    }

    #[test]
    fn a_stored_token_carries_every_attribute_that_makes_it_safe_to_store() {
        let cookies = access_cookies("a-token");

        assert_eq!(cookies.len(), 1);
        let cookie = &cookies[0];

        assert!(cookie.starts_with("access_token_0=a-token"), "{cookie}");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("Secure"), "{cookie}");
        assert!(cookie.contains("SameSite=Lax"), "{cookie}");
        assert!(cookie.contains("Path=/"), "{cookie}");
    }

    #[test]
    fn the_state_cookie_is_scoped_to_the_sign_in_flow_alone() {
        let cookie = state_cookie("a-state");

        assert!(cookie.starts_with("state=a-state"), "{cookie}");
        assert!(cookie.contains("Path=/login"), "{cookie}");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("Secure"), "{cookie}");
        assert!(cookie.contains("SameSite=Lax"), "{cookie}");
    }

    #[test]
    fn a_long_token_is_split_the_way_a_tak_client_reassembles_it() {
        let token = "x".repeat(CHUNK * 2 + 17);
        let cookies = access_cookies(&token);

        assert_eq!(cookies.len(), 3);
        assert!(cookies[0].starts_with("access_token_0="));
        assert!(cookies[1].starts_with("access_token_1="));
        assert!(cookies[2].starts_with("access_token_2="));

        let headers = with_cookies(&[
            ("access_token_0", &"x".repeat(CHUNK)),
            ("access_token_1", &"x".repeat(CHUNK)),
            ("access_token_2", &"x".repeat(17)),
        ]);

        assert_eq!(access_token_from_cookies(&headers), Some(token));
    }

    #[test]
    fn a_token_with_a_hole_in_it_is_not_reassembled_into_one_without() {
        let headers = with_cookies(&[("access_token_0", "aaa"), ("access_token_2", "ccc")]);

        assert_eq!(
            access_token_from_cookies(&headers),
            Some("aaa".to_string()),
            "reading stops at the gap, and the truncated token is then simply refused",
        );
    }

    #[test]
    fn a_request_with_no_cookies_carries_no_token() {
        assert_eq!(access_token_from_cookies(&HeaderMap::new()), None);
        assert_eq!(
            access_token_from_cookies(&with_cookies(&[("other", "value")])),
            None
        );
    }

    #[test]
    fn the_unnumbered_name_is_read_when_it_is_the_only_one() {
        let headers = with_cookies(&[("access_token", "a-token")]);

        assert_eq!(
            access_token_from_cookies(&headers),
            Some("a-token".to_string())
        );
    }

    #[test]
    fn signing_out_names_every_chunk_the_browser_actually_sent() {
        let headers = with_cookies(&[
            ("access_token_0", "a"),
            ("access_token_1", "b"),
            ("state", "s"),
            ("unrelated", "keep-me"),
        ]);

        let cleared = clearing_cookies(&headers);
        let joined = cleared.join("\n");

        assert!(joined.contains("access_token_0="), "{joined}");
        assert!(joined.contains("access_token_1="), "{joined}");
        assert!(joined.contains("state="), "{joined}");
        assert!(!joined.contains("unrelated"), "{joined}");
        assert!(
            cleared.iter().all(|cookie| cookie.contains("Max-Age=0")),
            "{joined}",
        );
    }

    #[test]
    fn one_named_cookie_is_read_back_as_it_was_sent() {
        let headers = with_cookies(&[("state", "the-state"), ("other", "x")]);

        assert_eq!(
            cookie_value(&headers, "state"),
            Some("the-state".to_string())
        );
        assert_eq!(cookie_value(&headers, "missing"), None);
    }
}
