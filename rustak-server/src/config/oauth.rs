//! `[auth.oauth]` — the clients rustak's own authorization server serves.
//!
//! Its own file rather than a corner of [`super::auth`] because the rules a
//! client has to satisfy are the *whole* of what makes this server an
//! authorization server rather than an open redirector, and they are
//! cross-field: a redirect URI has to be one a browser can be sent to safely, a
//! secret has to be present exactly when `public = false`, and an identifier
//! may not be registered twice. They are checked when the configuration loads —
//! `rustak --check` — rather than at the first authorization request, because
//! every one of them is a redirect target and none of them is something to
//! discover at three in the morning.
//!
//! # What `public` decides
//!
//! | | `public = true` | `public = false` |
//! |---|---|---|
//! | `secret` | refused | required |
//! | Client authentication at `/oauth/token` | none | `client_secret_post` or `client_secret_basic` |
//! | Proof key for code exchange | **mandatory**, `S256` only | optional, and `S256` only when sent |
//!
//! A public client holds nothing an interceptor does not, so the proof key is
//! the only thing standing between a code lifted from an address bar and a
//! session. A confidential client has a secret the interceptor does not have,
//! which is what lets the proof key become optional — and CloudTAK's relying
//! party sends no `code_challenge` at all (`compat/oauth.md` §6), so "optional"
//! is the difference between serving it and not.

use std::fmt;

use serde::{Deserialize, Serialize};

use human_errors::Error;

/// What a redacted secret renders as in a `Debug` dump.
const REDACTED: &str = "<redacted>";

fn default_true() -> bool {
    true
}

/// `[auth.oauth]` — the clients `GET /oauth/authorize` will issue codes to.
///
/// Empty by default, which means the authorization-code flow is switched off:
/// an authorization server with no registered client has nowhere legitimate to
/// send a code, and defaulting to a wildcard would turn this server into an
/// open redirector the moment somebody guessed a client identifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthServerConfig {
    /// The registered clients, by identifier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clients: Vec<OAuthClient>,

    /// The name of the group `GET /oauth/userinfo` reports for an
    /// administrator, beside the channels they hold.
    ///
    /// A relying party maps its own roles from group names, and "is this person
    /// an administrator" is the one fact that is not a channel — so it is
    /// released as a group rather than as a claim a standard library would
    /// ignore. Rename it when a channel of your own is already called `admin`.
    #[serde(default = "default_admin_group")]
    pub admin_group: String,
}

impl Default for OAuthServerConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            clients: Vec::new(),
            admin_group: default_admin_group(),
        }
    }
}

/// The marker group an administrator is reported in.
fn default_admin_group() -> String {
    "admin".to_string()
}

impl OAuthServerConfig {
    /// The registered client with this identifier, when there is one.
    ///
    /// The comparison is exact: client identifiers are chosen by the operator
    /// and written into a client's own configuration, so a case-insensitive
    /// match would only widen what counts as registered.
    pub fn client(&self, id: &str) -> Option<&OAuthClient> {
        self.clients.iter().find(|client| client.id == id)
    }
}

/// One registered client of our authorization server.
///
/// [`Debug`] is written out rather than derived: a configuration dump in a log
/// line or a bug report must not carry a client secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthClient {
    /// The `client_id` the client sends.
    pub id: String,

    /// Every URI a code may be sent to, in full.
    ///
    /// Compared byte for byte, never as a prefix: a prefix match on
    /// `https://app.example.com/` also matches
    /// `https://app.example.com/../../evil`, and an authorization server that
    /// hands a code to the wrong URI has handed away the session.
    pub redirect_uris: Vec<String>,

    /// Whether the client keeps no secret, which is every browser and mobile
    /// client.
    ///
    /// A public client **must** use proof key for code exchange, exactly as
    /// before. A confidential one (`public = false`) **must** carry a
    /// [`secret`](Self::secret) and authenticate with it at `/oauth/token`, and
    /// its proof key becomes optional — required only when it sent a
    /// `code_challenge`. The two are checked against each other when the
    /// configuration loads, so neither combination can be half-configured.
    #[serde(default = "default_true")]
    pub public: bool,

    /// The secret a confidential client authenticates with at `/oauth/token`.
    ///
    /// Required when `public = false` and refused when `public = true`. Write
    /// it as `"${{ env.RUSTAK_OAUTH_CLIENT_SECRET }}"` to keep it out of the
    /// file; it is redacted from every `Debug` rendering and never logged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,

    /// Every URI `/logout?post_logout_redirect_uri=` may send a browser to.
    ///
    /// Empty by default, which means a sign-out answers `204` and redirects
    /// nowhere. Compared byte for byte, for the same reason
    /// [`redirect_uris`](Self::redirect_uris) is: an unchecked one is an open
    /// redirector wearing a sign-out's clothes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_logout_redirect_uris: Vec<String>,
}

impl fmt::Debug for OAuthClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthClient")
            .field("id", &self.id)
            .field("redirect_uris", &self.redirect_uris)
            .field("public", &self.public)
            .field("secret", &self.secret.as_ref().map(|_| REDACTED))
            .field("post_logout_redirect_uris", &self.post_logout_redirect_uris)
            .finish()
    }
}

impl OAuthClient {
    /// Whether a code may be sent to this URI.
    pub fn allows(&self, redirect_uri: &str) -> bool {
        self.redirect_uris
            .iter()
            .any(|registered| registered == redirect_uri)
    }

    /// Whether a browser may be sent to this URI after signing out.
    pub fn allows_post_logout(&self, redirect_uri: &str) -> bool {
        self.post_logout_redirect_uris
            .iter()
            .any(|registered| registered == redirect_uri)
    }
}

impl OAuthServerConfig {
    /// Checks every rule a registered client has to satisfy.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error naming the client and the rule it
    /// broke.
    pub(super) fn validate(&self) -> Result<(), Error> {
        let mut seen: Vec<&str> = Vec::new();

        for client in &self.clients {
            if client.id.trim().is_empty() {
                return Err(refusal(
                    "a client under `[auth.oauth] clients` has an empty `id`",
                ));
            }

            if seen.contains(&client.id.as_str()) {
                return Err(refusal(&format!(
                    "`[auth.oauth] clients` registers `{}` twice",
                    client.id
                )));
            }

            seen.push(&client.id);
            client.validate()?;
        }

        Ok(())
    }
}

impl OAuthClient {
    /// Checks this one client. See [`OAuthServerConfig::validate`].
    fn validate(&self) -> Result<(), Error> {
        if self.redirect_uris.is_empty() {
            return Err(refusal(&format!(
                "the client `{}` has no `redirect_uris`, so a code could never be delivered to it",
                self.id
            )));
        }

        for uri in self
            .redirect_uris
            .iter()
            .chain(&self.post_logout_redirect_uris)
        {
            redirect_uri(&self.id, uri)?;
        }

        // A confidential client with no secret registers a flow it can start
        // and never finish; a public one carrying a secret has nowhere to keep
        // it, so the secret is in somebody's phone and the client is public
        // anyway. Both are configurations that look like they work.
        match (self.public, self.secret.as_deref().map(str::trim)) {
            (false, None | Some("")) => Err(refusal(&format!(
                "the client `{}` is `public = false` and carries no `secret`, so it could never authenticate at /oauth/token",
                self.id
            ))),
            (true, Some(_)) => Err(refusal(&format!(
                "the client `{}` is `public = true` and carries a `secret`, which a public client has nowhere to keep",
                self.id
            ))),
            _ => Ok(()),
        }
    }
}

/// One registered URI has to be one a browser could be sent to safely.
///
/// The same rule for a redirect URI and a post-logout one: both are places this
/// server will send a browser on somebody else's say-so, and the difference
/// between them is only what is in the query string when it arrives.
fn redirect_uri(client: &str, uri: &str) -> Result<(), Error> {
    let Ok(parsed) = url::Url::parse(uri) else {
        return Err(refusal(&format!(
            "the client `{client}` lists `{uri}`, which is not an absolute URI"
        )));
    };

    if parsed.fragment().is_some() {
        return Err(refusal(&format!(
            "the client `{client}` lists `{uri}`, and a redirect URI may not carry a fragment"
        )));
    }

    let loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));

    // A native client is registered with a loopback address, which is why the
    // scheme rule has an exception rather than being absolute; anything else
    // carrying a code over plain HTTP puts it in a proxy log.
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
        return Err(refusal(&format!(
            "the client `{client}` lists `{uri}`, which would carry an authorization code over plaintext"
        )));
    }

    Ok(())
}

/// The one shape an `[auth.oauth]` refusal takes.
fn refusal(what: &str) -> Error {
    human_errors::user(
        format!("`[auth.oauth]` is not usable as written: {what}."),
        &[
            "A public client is `{ id = \"...\", redirect_uris = [\"https://...\"] }` and must use proof key for code exchange.",
            "A confidential one adds `public = false` and `secret = \"${{ env.RUSTAK_OAUTH_CLIENT_SECRET }}\"`.",
            "Every URI is compared byte for byte, so it has to be exactly the one the client sends.",
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client registered as `body` says, parsed as the example file writes it.
    fn parse(body: &str) -> OAuthServerConfig {
        toml::from_str(body).expect("the section under test parses")
    }

    /// The message `--check` prints for a section it refuses.
    fn refused(body: &str) -> String {
        let Err(err) = parse(body).validate() else {
            panic!("`{body}` should have been refused");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");

        err.to_string()
    }

    /// One client with `extra` appended to its inline table.
    fn client(extra: &str) -> String {
        format!(
            "clients = [{{ id = \"app\", redirect_uris = [\"https://app.example.com/cb\"]{extra} }}]"
        )
    }

    #[test]
    fn no_client_is_registered_until_an_operator_registers_one() {
        // An authorization server with no registered client has nowhere
        // legitimate to send a code, so the flow is simply off.
        let parsed = parse("");

        assert!(parsed.clients.is_empty());
        assert_eq!(parsed.client("anything"), None);
        assert_eq!(parsed.admin_group, "admin");
        assert_eq!(parsed, OAuthServerConfig::default());
    }

    #[test]
    fn a_client_reads_back_as_the_example_file_writes_it() {
        let parsed = parse(
            r#"clients = [
              { id = "cloudtak", redirect_uris = ["https://map.example.com/callback"] },
            ]"#,
        );

        let client = parsed.client("cloudtak").expect("the one registered");

        assert!(client.public, "a client is public unless it says otherwise");
        assert!(client.secret.is_none());
        assert!(client.post_logout_redirect_uris.is_empty());
        assert!(client.allows("https://map.example.com/callback"));
        parsed
            .validate()
            .expect("a public client with a URI is usable");
    }

    #[test]
    fn a_confidential_client_reads_back_with_its_secret_and_sign_out_uris() {
        let parsed = parse(&client(
            r#", public = false, secret = "s3cret", post_logout_redirect_uris = ["https://app.example.com/bye"]"#,
        ));
        let client = parsed.client("app").expect("the one registered");

        assert!(!client.public);
        assert_eq!(client.secret.as_deref(), Some("s3cret"));
        assert!(client.allows_post_logout("https://app.example.com/bye"));
        assert!(!client.allows_post_logout("https://app.example.com/bye?x=1"));
        assert!(!client.allows_post_logout("https://evil.example.com/bye"));
        parsed
            .validate()
            .expect("a confidential client with a secret is usable");
    }

    #[test]
    fn a_confidential_client_with_no_secret_is_refused_at_load_time() {
        // It could start a flow and never finish one, which is a configuration
        // that looks like it works until somebody tries to sign in.
        for extra in [", public = false", r#", public = false, secret = "  ""#] {
            let message = refused(&client(extra));

            assert!(message.contains("public = false"), "{message}");
            assert!(message.contains("`app`"), "{message}");
        }
    }

    #[test]
    fn a_public_client_carrying_a_secret_is_refused_at_load_time() {
        // A public client has nowhere to keep one, so the "secret" is in
        // somebody's phone and the client is public anyway.
        let message = refused(&client(r#", secret = "s3cret""#));

        assert!(message.contains("public = true"), "{message}");
    }

    #[test]
    fn a_registered_client_needs_somewhere_to_send_a_code() {
        let message = refused(r#"clients = [{ id = "app", redirect_uris = [] }]"#);

        assert!(message.contains("redirect_uris"), "{message}");
    }

    #[test]
    fn a_client_identifier_cannot_be_registered_twice() {
        let message = refused(
            r#"clients = [
              { id = "app", redirect_uris = ["https://a.example.com/cb"] },
              { id = "app", redirect_uris = ["https://b.example.com/cb"] },
            ]"#,
        );

        assert!(message.contains("twice"), "{message}");
    }

    #[test]
    fn a_client_with_no_identifier_is_refused() {
        let message = refused(r#"clients = [{ id = " ", redirect_uris = ["https://a/cb"] }]"#);

        assert!(message.contains("empty `id`"), "{message}");
    }

    #[test]
    fn a_redirect_uri_that_would_leak_a_code_is_refused_at_load_time() {
        for uris in [
            r#"["http://app.example.com/cb"]"#,
            r#"["https://app.example.com/cb#fragment"]"#,
            r#"["/cb"]"#,
            r#"["not a uri"]"#,
        ] {
            let message = refused(&format!(
                r#"clients = [{{ id = "app", redirect_uris = {uris} }}]"#
            ));

            assert!(message.contains("[auth.oauth]"), "{uris}: {message}");
        }
    }

    #[test]
    fn a_post_logout_uri_is_held_to_the_same_rule_as_a_redirect_uri() {
        // It is a place this server will send a browser on somebody else's say
        // so, which is the whole of what makes an open redirector.
        let message = refused(&client(
            r#", post_logout_redirect_uris = ["http://app.example.com/bye"]"#,
        ));

        assert!(message.contains("plaintext"), "{message}");
    }

    #[test]
    fn a_loopback_client_may_use_plain_http_because_nothing_leaves_the_machine() {
        let parsed =
            parse(r#"clients = [{ id = "cli", redirect_uris = ["http://127.0.0.1:8080/cb"] }]"#);

        parsed.validate().expect("a loopback client is usable");
    }

    #[test]
    fn a_misspelled_client_key_is_refused_rather_than_ignored() {
        let Err(err) = toml::from_str::<OAuthServerConfig>(
            r#"clients = [{ id = "a", redirect_uri = ["https://a/cb"] }]"#,
        ) else {
            panic!("an unknown key should be refused");
        };

        assert!(!err.to_string().is_empty(), "{err}");
    }

    #[test]
    fn a_secret_never_appears_in_a_debug_dump() {
        // Configuration gets printed into logs and bug reports.
        let parsed = parse(&client(r#", public = false, secret = "s3cret-value""#));
        let rendered = format!("{parsed:?}");

        assert!(!rendered.contains("s3cret-value"), "{rendered}");
        assert!(rendered.contains(REDACTED), "{rendered}");
    }

    #[test]
    fn a_redirect_uri_is_matched_whole_rather_than_as_a_prefix() {
        // A prefix match on `https://app.example.com/` also matches
        // `https://app.example.com/../../evil`, and a code sent to the wrong
        // URI is the session given away.
        let parsed = parse(
            r#"clients = [{ id = "app", redirect_uris = ["https://app.example.com/callback"] }]"#,
        );
        let client = parsed.client("app").expect("the one registered");

        assert!(client.allows("https://app.example.com/callback"));

        for uri in [
            "https://app.example.com/callback/evil",
            "https://app.example.com/callback?next=1",
            "https://app.example.com/CALLBACK",
            "https://evil.example.com/callback",
            "",
        ] {
            assert!(!client.allows(uri), "{uri}");
        }
    }
}
