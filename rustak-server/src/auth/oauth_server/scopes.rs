//! The OpenID Connect scopes this server grants, and what each one releases.
//!
//! Four, and no more: `openid` asks for an ID token at all, and `profile`,
//! `email` and `groups` each name one group of claims. They are a **separate**
//! string from the rustak scope in [`crate::auth::tokens`] and never a
//! substitute for it — that one is the ceiling on what the access token may
//! *do*, and this one is a list of what may be *said* about the account. A
//! client that could widen the first by asking for the second would turn
//! "please tell me this person's email address" into administrative access.
//!
//! An unknown scope is dropped rather than refused. RFC 6749 §3.3 allows it,
//! every relying party sends a handful of scopes it hopes are supported, and
//! refusing the request would break a sign-in over a claim nobody needed.

/// Asks for an ID token. Without it, none is issued whatever else was asked.
pub const OPENID: &str = "openid";

/// `name`: the display name the account carries, when it has one.
pub const PROFILE: &str = "profile";

/// `email`: the account's email column, when it is set. Never invented.
pub const EMAIL: &str = "email";

/// `groups`: the channels the account holds, plus the administrator marker.
pub const GROUPS: &str = "groups";

/// Every scope this server grants, in the order it reports them.
pub const SUPPORTED: &[&str] = &[OPENID, PROFILE, EMAIL, GROUPS];

/// The scopes granted for a request, which is what it asked for narrowed to
/// [`SUPPORTED`].
///
/// [`None`] when nothing recognisable was asked for, which is how a client that
/// is not a relying party — the admin UI, a WebTAK page — is told apart from
/// one that asked for `openid` and got it. The order is [`SUPPORTED`]'s rather
/// than the request's, so the string a token response echoes is stable and a
/// duplicate in the request collapses.
pub fn granted(requested: Option<&str>) -> Option<String> {
    let requested = requested?;
    let asked: Vec<&str> = requested.split_whitespace().collect();
    let granted: Vec<&str> = SUPPORTED
        .iter()
        .copied()
        .filter(|supported| asked.contains(supported))
        .collect();

    match granted.is_empty() {
        true => None,
        false => Some(granted.join(" ")),
    }
}

/// Whether a granted string carries `scope`.
///
/// Exact, space-separated comparison rather than a substring search, for the
/// same reason [`crate::auth::tokens::grants_admin`] is: a scope named
/// `email_verified` must not satisfy `email`.
pub fn grants(granted: &str, scope: &str) -> bool {
    granted.split(' ').any(|held| held == scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relying_partys_default_request_is_granted_whole() {
        assert_eq!(
            granted(Some("openid profile email groups")).as_deref(),
            Some("openid profile email groups"),
        );
    }

    #[test]
    fn a_scope_this_server_does_not_serve_is_dropped_rather_than_refused() {
        // Every library sends a few scopes it hopes are supported; refusing the
        // request would break a sign-in over a claim nobody needed.
        assert_eq!(
            granted(Some("openid offline_access address phone")).as_deref(),
            Some("openid"),
        );
    }

    #[test]
    fn nothing_recognisable_is_no_grant_at_all() {
        // Which is how a client that is not a relying party is told apart from
        // one that asked for `openid` and got it.
        assert_eq!(granted(None), None);
        assert_eq!(granted(Some("")), None);
        assert_eq!(granted(Some("   ")), None);
        assert_eq!(granted(Some("api admin")), None);
    }

    #[test]
    fn the_order_is_ours_and_a_repeat_collapses() {
        // The response echoes this string, so it has to be stable whatever
        // order the client happened to write.
        assert_eq!(
            granted(Some("groups email email profile openid")).as_deref(),
            Some("openid profile email groups"),
        );
    }

    #[test]
    fn a_scope_that_merely_starts_with_another_is_not_that_other() {
        assert!(grants("openid email", EMAIL));
        assert!(!grants("openid email_verified", EMAIL));
        assert!(!grants("openid", PROFILE));
        assert!(!grants("", OPENID));
    }
}
