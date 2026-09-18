//! The rules a configuration has to satisfy beyond parsing as TOML.
//!
//! Everything here is a combination that is individually well-formed and
//! jointly impossible: an ACME order that could never be validated because no
//! listener is bound on the port the authority connects to, a `files`
//! certificate source with no files named, a plaintext listener nobody asked
//! for twice. Catching those at load time is what makes `--check` worth
//! running in a deployment pipeline — the alternative is a server that starts,
//! reports itself healthy, and fails at the first renewal or the first
//! enrollment.
//!
//! The rules live beside the schema rather than inside it because they are
//! *cross-section*: whether `[acme]` is satisfiable depends on `[web.public]`
//! and `[server]`, and serde has no way to express that.

use human_errors::Error;

use super::{AcmeChallenge, Config, MAX_SHUTDOWN_TIMEOUT, TlsMode};

/// Advice for a combination the example file shows the right form of.
const ADVICE_EXAMPLE: &[&str] = &[
    "Compare the section against config.example.toml, which documents every key with its default.",
    "Run `rustak --config <file> --check` to validate a file without starting the server.",
];

/// Advice for a `tls-alpn-01` challenge with no listener on port 443.
const ADVICE_TLS_ALPN_01: &[&str] = &[
    "Add \":443\" to `[web.public] listen`: the challenge is answered inside the TLS handshake, by that listener, on that port.",
    "Or use `challenge = \"http-01\"` and make port 80 reach the public listener.",
    "Binding a port below 1024 needs CAP_NET_BIND_SERVICE, or a proxy that forwards it.",
];

/// Advice for an `http-01` challenge with no plaintext listener.
const ADVICE_HTTP_01: &[&str] = &[
    "Add \":80\" to `[web.public] listen`, or set `[web.public] plain_bind = \":80\"` and put a proxy in front that forwards it.",
    "Or use `challenge = \"tls-alpn-01\"` with \":443\" in `[web.public] listen`, which needs no plaintext port at all.",
    "Binding a port below 1024 needs CAP_NET_BIND_SERVICE, or a proxy that forwards it.",
];

/// Advice for a name a public authority could never issue for.
const ADVICE_PUBLIC_NAME: &[&str] = &[
    "List the fully qualified names this server answers to on the public internet, for example `domains = [\"tak.example.com\"]`.",
    "A private or made-up name cannot be validated by a public authority; use `[web.public.tls] mode = \"internal\"` for a LAN deployment.",
];

/// Checks every cross-section rule, in the order an operator meets them.
pub(super) fn validate(config: &Config) -> Result<(), Error> {
    shutdown(config)?;
    public_listener(config)?;
    certificate_source(config)?;
    acme(config)?;
    credentials(config)?;
    oauth_clients(config)?;
    pki(config)?;
    distinct_listeners(config)
}

/// The drain budget has to describe a wait an orchestrator would sit through.
///
/// Both ends matter. Zero would cut every connection off the moment a
/// `SIGTERM` arrived, including the upload somebody was halfway through; longer
/// than [`MAX_SHUTDOWN_TIMEOUT`] would outlast every default grace period there
/// is, so the process would be `SIGKILL`ed before the drain it asked for
/// finished — and the checkpoint that runs after the drain would never run at
/// all. That is the failure this whole setting exists to prevent, so a budget
/// which guarantees it is refused rather than warned about.
fn shutdown(config: &Config) -> Result<(), Error> {
    let budget = config.server.shutdown_timeout;

    positive(budget, "[server] shutdown_timeout")?;

    if budget > MAX_SHUTDOWN_TIMEOUT {
        return Err(human_errors::user(
            format!(
                "`[server] shutdown_timeout` is {}s, and a drain may be at most {}s.",
                budget.num_seconds(),
                MAX_SHUTDOWN_TIMEOUT.num_seconds(),
            ),
            &[
                "Set it to how long connections need to close, inside whatever your orchestrator allows before it sends SIGKILL.",
                "`docker stop` allows ten seconds in total and the checkpoint after the drain needs two of them, which is why the default is 8s.",
            ],
        ));
    }

    Ok(())
}

/// `[web.public]` is the listener everything else is reached through.
fn public_listener(config: &Config) -> Result<(), Error> {
    if config.web.public.listen.is_empty() {
        return Err(human_errors::user(
            "`[web.public] listen` is empty, so there is nothing to serve the admin UI, the API or enrollment on.",
            ADVICE_EXAMPLE,
        ));
    }

    Ok(())
}

/// The `[web.public.tls] mode` must be one this installation can carry out.
fn certificate_source(config: &Config) -> Result<(), Error> {
    let public = &config.web.public;

    match public.tls.mode {
        TlsMode::None if !public.allow_insecure_http => Err(human_errors::user(
            "`[web.public.tls] mode = \"none\"` would serve the admin UI, the API, enrollment and OAuth tokens over plaintext HTTP.",
            &[
                "Set `[web.public.tls] mode` to \"internal\", \"files\" or \"acme\" to serve TLS.",
                "If this really is a development instance, or TLS is terminated by a proxy in front of rustak, set `[web.public] allow_insecure_http = true` as well.",
            ],
        )),
        TlsMode::Files if public.tls.cert_file.is_none() || public.tls.key_file.is_none() => {
            Err(human_errors::user(
                "`[web.public.tls] mode = \"files\"` needs both `cert_file` and `key_file`.",
                &[
                    "Set `cert_file` to the full certificate chain in PEM form, and `key_file` to its private key.",
                    "Use `mode = \"acme\"` to obtain a certificate automatically, or \"internal\" to issue one from rustak's own CA.",
                ],
            ))
        }
        _ => Ok(()),
    }
}

/// An ACME order has to be one the authority could complete.
fn acme(config: &Config) -> Result<(), Error> {
    let acme = &config.acme;
    let ordering = config.web.public.tls.mode == TlsMode::Acme;

    if acme.enabled != ordering {
        return Err(human_errors::user(
            "`[acme] enabled` and `[web.public.tls] mode = \"acme\"` disagree, so the certificate rustak serves would not be the one it orders.",
            &[
                "Set both: `[web.public.tls] mode = \"acme\"` and `[acme] enabled = true`.",
                "Or set neither, and choose \"internal\" or \"files\" as the certificate source.",
            ],
        ));
    }

    if !ordering {
        return Ok(());
    }

    if acme.domains(&config.server).is_empty() {
        return Err(human_errors::user(
            "ACME has no names to request: set `[acme] domains`, or `[server] domains`.",
            &[
                "List the public host names this server answers to, for example `domains = [\"tak.example.com\"]`.",
                "Every name must resolve to this server from the public internet before an order can be validated.",
            ],
        ));
    }

    if !acme.accept_tos {
        return Err(human_errors::user(
            format!(
                "`[acme] accept_tos` is not set, and {} will not issue a certificate until its terms of service are accepted.",
                acme.directory
            ),
            &[
                "Read the authority's terms of service, then set `[acme] accept_tos = true`.",
                "rustak does not accept somebody else's terms on your behalf.",
            ],
        ));
    }

    public_names(config)?;
    challenge_is_reachable(config)
}

/// Every name in an order has to be one a public authority could issue for.
///
/// Not a style rule: an order for `tak.lan` or for an address literal is
/// refused by the authority *after* it has counted against the account's rate
/// limit, and Let's Encrypt's failed-validation limit is five an hour.
/// Catching it at `--check` costs nothing and saves an afternoon. The
/// classification itself lives beside the section it belongs to; see
/// [`AcmeConfig::unorderable`](super::AcmeConfig::unorderable).
fn public_names(config: &Config) -> Result<(), Error> {
    for name in config.acme.domains(&config.server) {
        if let Some(reason) = super::AcmeConfig::unorderable(name) {
            return Err(human_errors::user(
                format!("`{name}` cannot be ordered from a certificate authority: {reason}"),
                ADVICE_PUBLIC_NAME,
            ));
        }
    }

    Ok(())
}

/// The ACME authority connects to one specific port; something has to answer.
fn challenge_is_reachable(config: &Config) -> Result<(), Error> {
    let public = &config.web.public;

    match config.acme.challenge {
        AcmeChallenge::TlsAlpn01 if public.listens_on(443) => Ok(()),
        // A `plain_bind` on another port is accepted: a proxy forwarding port
        // 80 to it is a normal deployment, and only the operator knows whether
        // there is one.
        AcmeChallenge::Http01 if public.plain_bind.is_some() || public.listens_on(80) => Ok(()),
        AcmeChallenge::TlsAlpn01 => Err(human_errors::user(
            "The ACME `tls-alpn-01` challenge is answered on port 443, and nothing is bound there.",
            ADVICE_TLS_ALPN_01,
        )),
        AcmeChallenge::Http01 => Err(human_errors::user(
            "The ACME `http-01` challenge is answered on port 80, and nothing is bound there.",
            ADVICE_HTTP_01,
        )),
    }
}

/// Credential lifetimes and the rate limiter have to describe a window that
/// exists.
fn credentials(config: &Config) -> Result<(), Error> {
    let auth = &config.auth;

    positive(auth.access_token_ttl, "[auth] access_token_ttl")?;
    positive(auth.refresh_token_ttl, "[auth] refresh_token_ttl")?;
    positive(auth.enrollment_token_ttl, "[auth] enrollment_token_ttl")?;
    positive(auth.client_password_ttl, "[auth] client_password_ttl")?;
    positive(auth.rate_limit.window, "[auth.rate_limit] window")?;

    if auth.rate_limit.attempts == 0 {
        return Err(human_errors::user(
            "`[auth.rate_limit] attempts = 0` would lock out every credential on its first use.",
            &[
                "Set `attempts` to the number of failures allowed within `window`, for example 10.",
                "There is no way to disable the rate limiter: it is what stands between a client password and an offline guessing attack.",
            ],
        ));
    }

    Ok(())
}

/// Every registered OAuth2 client has to name somewhere a code may be sent.
///
/// These are checked at load time rather than at the first authorization
/// request because every one of them is a redirect target: a duplicate
/// identifier means whichever entry happens to be first wins, a client with no
/// redirect URI can never complete a sign-in, and a URI that is not an absolute
/// `https` address is either unusable or — with a fragment, or over plain HTTP
/// — a way to leak a code out of the browser.
fn oauth_clients(config: &Config) -> Result<(), Error> {
    let mut seen: Vec<&str> = Vec::new();

    for client in &config.auth.oauth.clients {
        if client.id.trim().is_empty() {
            return Err(oauth_refusal(
                "a client under `[auth.oauth] clients` has an empty `id`",
            ));
        }

        if seen.contains(&client.id.as_str()) {
            return Err(oauth_refusal(&format!(
                "`[auth.oauth] clients` registers `{}` twice",
                client.id
            )));
        }

        seen.push(&client.id);

        if client.redirect_uris.is_empty() {
            return Err(oauth_refusal(&format!(
                "the client `{}` has no `redirect_uris`, so a code could never be delivered to it",
                client.id
            )));
        }

        for uri in &client.redirect_uris {
            redirect_uri(&client.id, uri)?;
        }
    }

    Ok(())
}

/// One registered redirect URI has to be one a browser could be sent to safely.
fn redirect_uri(client: &str, uri: &str) -> Result<(), Error> {
    let Ok(parsed) = url::Url::parse(uri) else {
        return Err(oauth_refusal(&format!(
            "the client `{client}` lists `{uri}`, which is not an absolute URI"
        )));
    };

    if parsed.fragment().is_some() {
        return Err(oauth_refusal(&format!(
            "the client `{client}` lists `{uri}`, and a redirect URI may not carry a fragment"
        )));
    }

    let loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));

    // A native client is registered with a loopback address, which is why the
    // scheme rule has an exception rather than being absolute; anything else
    // carrying a code over plain HTTP puts it in a proxy log.
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
        return Err(oauth_refusal(&format!(
            "the client `{client}` lists `{uri}`, which would carry an authorization code over plaintext"
        )));
    }

    Ok(())
}

/// The one shape an `[auth.oauth]` refusal takes.
fn oauth_refusal(what: &str) -> Error {
    human_errors::user(
        format!("`[auth.oauth]` is not usable as written: {what}."),
        &[
            "Each client is `{ id = \"...\", redirect_uris = [\"https://...\"], public = true }`, with the URIs written out in full.",
            "A redirect URI is compared byte for byte, so it has to be exactly the one the client sends.",
        ],
    )
}

/// The certificate authority has to be able to issue what it is asked for.
fn pki(config: &Config) -> Result<(), Error> {
    let pki = &config.pki;

    positive(pki.ca_validity, "[pki] ca_validity")?;
    positive(pki.client_cert_validity, "[pki] client_cert_validity")?;
    positive(pki.server_cert_validity, "[pki] server_cert_validity")?;

    if pki.csr_min_rsa_bits < 2048 {
        return Err(human_errors::user(
            format!(
                "`[pki] csr_min_rsa_bits = {}` would accept RSA keys that are no longer considered safe.",
                pki.csr_min_rsa_bits
            ),
            &[
                "Leave `csr_min_rsa_bits` at 2048, or raise it.",
                "A client certificate issued from a weak key stays valid for `client_cert_validity`, so this is not a setting to relax temporarily.",
            ],
        ));
    }

    if pki.server_cert_renew_before >= pki.server_cert_validity {
        return Err(human_errors::user(
            "`[pki] server_cert_renew_before` is not shorter than `server_cert_validity`, so every certificate would be due for renewal the moment it is issued.",
            ADVICE_EXAMPLE,
        ));
    }

    if let Some(entry) = pki.malformed_name_entry() {
        return Err(human_errors::user(
            format!(
                "`[pki] name_entries` contains {entry}, which is not a subject component rustak can issue."
            ),
            &[
                "Write each entry as a [\"type\", \"value\"] pair, for example [\"OU\", \"EUD\"].",
                "Neither half may be blank: an EUD builds its signing request from these entries, and OpenSSL refuses a zero-length subject component.",
                "\"CN\" cannot be set here: the common name of an issued certificate is the username it identifies.",
            ],
        ));
    }

    Ok(())
}

/// Two listeners on one address is a start-up failure with a confusing message;
/// caught here it is a configuration error with a clear one.
fn distinct_listeners(config: &Config) -> Result<(), Error> {
    let mut bound: Vec<(String, &str)> = Vec::new();

    for address in &config.web.public.listen {
        bound.push((address.to_string(), "[web.public] listen"));
    }

    if let Some(address) = &config.web.public.plain_bind {
        bound.push((address.to_string(), "[web.public] plain_bind"));
    }

    if config.web.marti.enabled {
        bound.push((config.web.marti.listen.to_string(), "[web.marti] listen"));
    }

    if config.stream.tls.enabled {
        bound.push((config.stream.tls.listen.to_string(), "[stream.tls] listen"));
    }

    for index in 1..bound.len() {
        let (address, section) = &bound[index];

        // Port 0 asks the operating system for an ephemeral port, so several
        // of them are not the same socket; the test harnesses rely on that.
        if address.ends_with(":0") {
            continue;
        }

        if let Some((_, first)) = bound[..index].iter().find(|(other, _)| other == address) {
            return Err(human_errors::user(
                format!(
                    "`{section}` and `{first}` both bind {address}; only one of them could start."
                ),
                ADVICE_EXAMPLE,
            ));
        }
    }

    Ok(())
}

/// Refuses a duration that names no window at all.
fn positive(value: chrono::Duration, key: &str) -> Result<(), Error> {
    if value <= chrono::Duration::zero() {
        return Err(human_errors::user(
            format!("`{key}` must be longer than zero."),
            ADVICE_EXAMPLE,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loads a configuration *without* validating it, so that a rule can be
    /// tested on a file that parses.
    fn parse(text: &str) -> Config {
        toml::from_str(text).expect("the fragment should parse")
    }

    fn refusal(text: &str) -> String {
        let Err(err) = parse(text).validate() else {
            panic!("this configuration should not validate:\n{text}");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
        err.to_string()
    }

    #[test]
    fn the_defaults_are_a_configuration_that_starts() {
        // An installation that writes an empty file gets an internal CA, a TLS
        // listener on 8446, Marti on 8443 and the stream on 8089.
        Config::default().validate().unwrap();
    }

    #[test]
    fn plaintext_needs_saying_twice() {
        let message = refusal("[web.public.tls]\nmode = \"none\"\n");
        assert!(message.contains("plaintext"), "{message}");

        parse("[web.public]\nallow_insecure_http = true\n[web.public.tls]\nmode = \"none\"\n")
            .validate()
            .unwrap();
    }

    #[test]
    fn a_file_based_certificate_needs_both_files() {
        let message = refusal(
            "[web.public.tls]\nmode = \"files\"\ncert_file = \"/etc/rustak/fullchain.pem\"\n",
        );
        assert!(message.contains("key_file"), "{message}");

        parse(
            r#"
            [web.public.tls]
            mode = "files"
            cert_file = "/etc/rustak/fullchain.pem"
            key_file = "/etc/rustak/privkey.pem"
            "#,
        )
        .validate()
        .unwrap();
    }

    #[test]
    fn the_two_acme_switches_have_to_agree() {
        // Otherwise rustak orders a certificate it never serves, or serves one
        // it never ordered.
        let message = refusal("[acme]\nenabled = true\n");
        assert!(message.contains("disagree"), "{message}");

        let message = refusal("[web.public.tls]\nmode = \"acme\"\n");
        assert!(message.contains("disagree"), "{message}");
    }

    /// An otherwise complete ACME configuration, missing only the listener.
    const ACME: &str = r#"
        [server]
        domains = ["tak.example.com"]

        [web.public.tls]
        mode = "acme"

        [acme]
        enabled = true
        accept_tos = true
    "#;

    #[test]
    fn an_acme_order_needs_names_to_request() {
        let message = refusal(
            r#"
            [web.public]
            listen = [":8446", ":443"]
            [web.public.tls]
            mode = "acme"
            [acme]
            enabled = true
            accept_tos = true
            "#,
        );

        assert!(message.contains("no names"), "{message}");
    }

    #[test]
    fn an_acme_order_needs_the_terms_accepted() {
        let message = refusal(
            r#"
            [server]
            domains = ["tak.example.com"]
            [web.public]
            listen = [":443"]
            [web.public.tls]
            mode = "acme"
            [acme]
            enabled = true
            "#,
        );

        assert!(message.contains("terms of service"), "{message}");
    }

    #[test]
    fn a_name_no_public_authority_could_issue_for_is_refused_before_the_order() {
        // Each of these costs a failed-validation slot against Let's Encrypt's
        // five-an-hour limit if it is discovered at run time instead.
        for name in [
            "tak.lan",
            "tak.local",
            "rustak.internal",
            "tak.home.arpa",
            "localhost",
            "192.168.1.10",
            "*.tak.lan",
        ] {
            let message = refusal(&format!(
                r#"
                [server]
                domains = ["{name}"]
                [web.public]
                listen = [":443"]
                [web.public.tls]
                mode = "acme"
                [acme]
                enabled = true
                accept_tos = true
                "#,
            ));

            assert!(
                message.contains(name) || message.contains(&name.to_ascii_lowercase()),
                "the refusal has to name the name: {message}",
            );
        }
    }

    #[test]
    fn an_ordinary_public_name_and_a_wildcard_over_one_are_accepted() {
        for name in ["tak.example.com", "TAK.example.com.", "*.example.com"] {
            parse(&format!(
                r#"
                [server]
                domains = ["{name}"]
                [web.public]
                listen = [":443"]
                [web.public.tls]
                mode = "acme"
                [acme]
                enabled = true
                accept_tos = true
                "#,
            ))
            .validate()
            .unwrap_or_else(|err| panic!("{name} should be orderable: {err}"));
        }
    }

    #[test]
    fn tls_alpn_needs_port_443_bound() {
        // The authority connects to 443 and completes the challenge in the
        // handshake; an installation on 8446 alone can never be validated.
        let message = refusal(ACME);
        assert!(message.contains("443"), "{message}");

        parse(&format!(
            "{ACME}\n[web.public]\nlisten = [\":8446\", \":443\"]"
        ))
        .validate()
        .unwrap();
    }

    #[test]
    fn http_01_needs_a_plaintext_port() {
        let message = refusal(
            r#"
            [server]
            domains = ["tak.example.com"]
            [web.public.tls]
            mode = "acme"
            [acme]
            enabled = true
            accept_tos = true
            challenge = "http-01"
            "#,
        );
        assert!(message.contains("80"), "{message}");

        parse(
            r#"
            [server]
            domains = ["tak.example.com"]
            [web.public]
            plain_bind = ":80"
            [web.public.tls]
            mode = "acme"
            [acme]
            enabled = true
            accept_tos = true
            challenge = "http-01"
            "#,
        )
        .validate()
        .unwrap();
    }

    #[test]
    fn a_drain_nobody_would_wait_out_is_refused() {
        // The setting exists to keep the shutdown inside an orchestrator's
        // grace period, so a value that cannot be is the one thing it must not
        // accept.
        let message = refusal("[server]\nshutdown_timeout = \"5m\"\n");
        assert!(message.contains("shutdown_timeout"), "{message}");
        assert!(message.contains("60s"), "{message}");

        let message = refusal("[server]\nshutdown_timeout = \"0s\"\n");
        assert!(message.contains("shutdown_timeout"), "{message}");

        parse("[server]\nshutdown_timeout = \"60s\"\n")
            .validate()
            .unwrap();
    }

    #[test]
    fn a_rate_limit_that_allows_nothing_is_refused() {
        let message = refusal("[auth.rate_limit]\nattempts = 0\n");
        assert!(message.contains("lock out"), "{message}");
    }

    #[test]
    fn a_credential_lifetime_of_zero_is_refused() {
        let message = refusal("[auth]\nenrollment_token_ttl = \"0s\"\n");
        assert!(message.contains("enrollment_token_ttl"), "{message}");
    }

    /// `[auth.oauth]` with one client whose redirect URIs are `uris`.
    fn with_client(id: &str, uris: &str) -> String {
        format!("[auth.oauth]\nclients = [{{ id = \"{id}\", redirect_uris = {uris} }}]\n")
    }

    #[test]
    fn a_registered_client_needs_somewhere_to_send_a_code() {
        let message = refusal(&with_client("app", "[]"));
        assert!(message.contains("redirect_uris"), "{message}");
    }

    #[test]
    fn a_client_identifier_cannot_be_registered_twice() {
        // Otherwise whichever entry is first silently wins, and the redirect
        // URIs of the other one are never honoured.
        let message = refusal(
            "[auth.oauth]\nclients = [\
             { id = \"app\", redirect_uris = [\"https://a.example.com/cb\"] },\
             { id = \"app\", redirect_uris = [\"https://b.example.com/cb\"] }]\n",
        );

        assert!(message.contains("twice"), "{message}");
    }

    #[test]
    fn a_redirect_uri_that_would_leak_a_code_is_refused_at_load_time() {
        for uris in [
            r#"["http://app.example.com/cb"]"#,
            r#"["/cb"]"#,
            r#"["https://app.example.com/cb#fragment"]"#,
        ] {
            let message = refusal(&with_client("app", uris));
            assert!(message.contains("[auth.oauth]"), "{uris}: {message}");
        }
    }

    #[test]
    fn a_loopback_client_may_use_plain_http_because_nothing_leaves_the_machine() {
        let config = parse(&with_client("native", r#"["http://127.0.0.1:1234/cb"]"#));

        assert_eq!(config.auth.oauth.clients.len(), 1);
    }

    #[test]
    fn a_weak_csr_key_size_cannot_be_configured() {
        // A certificate issued from a 1024-bit key outlives the change that
        // allowed it, which is why this is a refusal rather than a warning.
        let message = refusal("[pki]\ncsr_min_rsa_bits = 1024\n");
        assert!(message.contains("1024"), "{message}");
    }

    #[test]
    fn a_certificate_cannot_be_due_for_renewal_when_it_is_issued() {
        let message =
            refusal("[pki]\nserver_cert_validity = \"30d\"\nserver_cert_renew_before = \"30d\"\n");
        assert!(message.contains("renew_before"), "{message}");
    }

    #[test]
    fn a_common_name_cannot_be_configured_into_the_subject() {
        let message = refusal("[pki]\nname_entries = [[\"CN\", \"somebody\"]]\n");
        assert!(message.contains("CN"), "{message}");
    }

    #[test]
    fn two_listeners_on_one_socket_are_caught_before_start_up() {
        let message = refusal("[web.marti]\nlisten = \":8446\"\n");
        assert!(message.contains("8446"), "{message}");
        assert!(message.contains("[web.marti] listen"), "{message}");
    }

    #[test]
    fn a_disabled_listener_does_not_collide() {
        parse("[web.marti]\nenabled = false\nlisten = \":8446\"\n")
            .validate()
            .unwrap();
    }

    #[test]
    fn several_ephemeral_ports_are_not_a_collision() {
        // What the in-process test harness binds: every listener on port 0, so
        // that concurrent suites do not race for a fixed number.
        parse(
            r#"
            [web.public]
            listen = [":0"]
            allow_insecure_http = true
            [web.public.tls]
            mode = "none"
            [web.marti]
            listen = ":0"
            [stream.tls]
            listen = ":0"
            "#,
        )
        .validate()
        .unwrap();
    }
}
