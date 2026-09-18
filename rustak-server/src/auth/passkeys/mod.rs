//! Passkeys: the only way to sign in to rustak without an identity provider.
//!
//! There are no local passwords, so this is it. The ceremonies are
//! [`webauthn_rp`]'s, and what is here is the four decisions around them: which
//! relying party we are, where the challenge state lives, which credentials a
//! challenge lists, and what a successful assertion means.
//!
//! # Why not `webauthn-rs`
//!
//! `webauthn-rs` 0.5 reaches WebAuthn through the `openssl` crate. That breaks
//! every cross-build in CI (`openssl-sys`' build script needs a system libssl on
//! the build image) and would link libssl into a binary whose every other
//! cryptographic dependency is rustls or RustCrypto. `webauthn_rp` verifies with
//! `p256`, `p384`, `rsa` and `ed25519-dalek`, so `cargo tree -i openssl-sys`
//! is now empty. `.claude/plan/status/M0-20-pure-rust-webauthn.md` records the
//! trade-offs.
//!
//! # The relying party is the host the console is served from
//!
//! WebAuthn binds a credential to a relying-party identifier, and the browser
//! refuses a ceremony whose identifier is not a suffix of the page's own host.
//! So the identifier is derived from this server's base URL, and a credential
//! registered at one host will not work at another — which is the property that
//! makes a passkey unphishable, and also the reason a development console
//! served by `trunk` on a different port has to be told about the same host.
//!
//! The one relaxation is that a loopback origin may differ in port, because
//! `trunk serve` proxies to the server from `127.0.0.1:8081` while the server
//! itself is on another port. The relying-party identifier is unchanged by it,
//! and it applies to loopback only.
//!
//! # Every passkey we register is discoverable
//!
//! The sign-in prompt has no username field, so signing in is a *discoverable*
//! ceremony: an empty `allowCredentials`, with the authenticator naming the
//! account. A credential the authenticator did not store cannot answer one, so
//! registration asks for `residentKey: "required"` and `userVerification:
//! "required"` — which is what `webauthn_rp`'s `PublicKeyCredentialCreationOptions::passkey`
//! builds, so unlike under `webauthn-rs` it no longer has to be patched onto the
//! serialised options afterwards. A username-assisted sign-in is still offered
//! as a fallback ([`Passkeys::start_login`] with an account), for a key
//! registered elsewhere or by an older version of this server.
//!
//! # Why a counter that goes backwards is fatal
//!
//! `SignatureCounterEnforcement::Fail` refuses an assertion whose counter did
//! not move past the stored one — but only when the stored one is non-zero,
//! because an authenticator that does not implement the counter reports zero
//! for ever and refusing those would lock out every such device. A counter that
//! *had* been moving and then stopped or went backwards is the signal that the
//! credential has been cloned. Recording the new counter after every sign-in is
//! therefore not bookkeeping, it is the check.

mod login;
mod register;

use rustak_api::PasskeySummary;
use rustak_core::prelude::*;
use webauthn_rp::request::auth::AuthenticationVerificationOptions;
use webauthn_rp::request::register::RegistrationVerificationOptions;
use webauthn_rp::request::{AsciiDomain, DomainOrigin, Port, RpId, Scheme};

use crate::db::repos::PasskeyRow;

/// What a browser is told when a ceremony will not work.
const ADVICE_CEREMONY: &[&str] = &[
    "Try again, and complete the prompt your browser shows.",
    "A passkey only works on the host it was registered against.",
];

/// The relying party this server is, ready to run a ceremony.
pub struct Passkeys {
    /// The identifier every credential is bound to.
    rp_id: RpId,
    /// What a browser calls this server in its prompt.
    rp_name: String,
    /// The scheme of the origin the console is served from.
    scheme: String,
    /// The host, which is also the relying-party identifier.
    host: String,
    /// The port the origin must be on, or [`Port::Any`] for loopback.
    port: Port,
}

impl std::fmt::Debug for Passkeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Passkeys")
            .field("origin", &self.origin_description())
            .finish()
    }
}

impl Passkeys {
    /// Builds the relying party from this server's own base URL.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the base URL is not something
    /// a relying party can be derived from — no host, or a host the browser
    /// would refuse.
    pub fn for_base_url(base_url: &str, rp_name: &str) -> Result<Self, Error> {
        let origin =
            url::Url::parse(base_url).map_err(|err| unusable_origin(base_url, &err.to_string()))?;

        // WebAuthn identifies a relying party by domain, so an address is not
        // one — a console reached at `https://192.0.2.10` cannot register a
        // passkey at all, whatever else is configured.
        let host = origin
            .domain()
            .ok_or_else(|| {
                unusable_origin(base_url, "a passkey is bound to a name, not an address")
            })?
            .to_string();

        let rp_id = AsciiDomain::try_from(host.clone())
            .map(RpId::Domain)
            .map_err(|err| unusable_origin(base_url, &err.to_string()))?;

        // `trunk serve` proxies to the API from its own port during
        // development, so a loopback console may be reached on any port. The
        // relying-party identifier is unchanged by that, and the relaxation
        // applies here and nowhere else.
        let port = if is_loopback(&host) {
            Port::Any
        } else {
            origin.port().map_or(Port::None, Port::Val)
        };

        Ok(Self {
            rp_id,
            rp_name: rp_name.to_string(),
            scheme: origin.scheme().to_string(),
            host,
            port,
        })
    }

    /// The origins an assertion produced by a browser may claim.
    ///
    /// A one-element slice rather than a list: the relying party is exactly one
    /// host, and the only variation is the port a loopback console is served on.
    fn origins(&self) -> [DomainOrigin<'_, '_>; 1] {
        [DomainOrigin {
            scheme: Scheme::Other(self.scheme.as_str()),
            host: self.host.as_str(),
            port: self.port,
        }]
    }

    /// How a registration's client data is checked.
    ///
    /// `error_on_unsolicited_extensions` is off because browsers return
    /// extension outputs we did not ask for (`credProps` most often) and
    /// refusing those would be a sign-in failure with no security benefit —
    /// nothing here reads an extension we did not request. Everything that does
    /// bear on whether the credential is ours stays on: the origin, the
    /// relying-party identifier hash, the challenge, and user verification.
    fn registration_options<'a>(
        origins: &'a [DomainOrigin<'a, 'a>; 1],
    ) -> RegistrationVerificationOptions<'a, 'a, DomainOrigin<'a, 'a>, DomainOrigin<'a, 'a>> {
        RegistrationVerificationOptions {
            allowed_origins: origins,
            error_on_unsolicited_extensions: false,
            ..RegistrationVerificationOptions::default()
        }
    }

    /// How an assertion's client data is checked.
    ///
    /// `SignatureCounterEnforcement::Fail` is the default and is what we
    /// want; it is named in the module documentation above because it is a
    /// security decision rather than a default worth inheriting silently.
    fn authentication_options<'a>(
        origins: &'a [DomainOrigin<'a, 'a>; 1],
    ) -> AuthenticationVerificationOptions<'a, 'a, DomainOrigin<'a, 'a>, DomainOrigin<'a, 'a>> {
        AuthenticationVerificationOptions {
            allowed_origins: origins,
            error_on_unsolicited_extensions: false,
            // We do not store which attachment a credential was registered
            // with, so there is nothing to compare a fresh one against. The
            // flag carries no authentication weight; the signature does.
            auth_attachment_enforcement:
                webauthn_rp::request::auth::AuthenticatorAttachmentEnforcement::Ignore(false),
            ..AuthenticationVerificationOptions::default()
        }
    }

    /// The relying-party identifier, for the ceremonies.
    fn rp_id(&self) -> &RpId {
        &self.rp_id
    }

    /// Names this server in the options a browser is about to show somebody.
    ///
    /// `webauthn_rp` serialises `rp.name` as the relying-party *identifier*,
    /// which is correct but is a host name where the prompt has room for a
    /// sentence. The installation's own name is what was there before this
    /// library, it is what an operator configured, and it is read by nothing
    /// but the prompt — it never comes back, and no check looks at it — so it
    /// is put back on the way out rather than the operator losing it.
    fn name_the_installation(&self, options: &mut serde_json::Value) {
        if let Some(rp) = options
            .get_mut("rp")
            .and_then(serde_json::Value::as_object_mut)
        {
            rp.insert("name".to_string(), self.rp_name.as_str().into());
        }
    }

    /// What `Debug` prints, and what the tests assert on.
    fn origin_description(&self) -> String {
        match self.port {
            Port::Any => format!("{}://{} (any port)", self.scheme, self.host),
            Port::Val(port) => format!("{}://{}:{port}", self.scheme, self.host),
            _ => format!("{}://{}", self.scheme, self.host),
        }
    }
}

/// A stored passkey as the UI lists it.
pub fn to_summary(row: &PasskeyRow) -> PasskeySummary {
    PasskeySummary {
        id: row.id,
        label: row.label.clone(),
        created_at: row.created_at,
        last_used_at: row.last_used_at,
    }
}

/// Whether a host is one a development console is served from.
///
/// `trunk serve` proxies to the API from its own port, so the origin differs
/// from the server's by port alone. The relying party is unchanged by that, and
/// the relaxation applies here and nowhere else.
fn is_loopback(rp_id: &str) -> bool {
    rp_id == "localhost" || rp_id.ends_with(".localhost")
}

/// What a browser is told when its response does not verify.
///
/// Deliberately the same for every cause: which check an assertion failed is
/// not something whoever presented it should learn.
fn ceremony_failed<E: std::fmt::Display>(err: E) -> Error {
    debug!(error = %err, "A passkey ceremony did not verify.");

    human_errors::user("That passkey could not be used.", ADVICE_CEREMONY)
}

/// What a browser is told when its response is not a credential at all.
fn malformed(err: serde_json::Error) -> Error {
    debug!(error = %err, "A passkey response was not in the shape we expect.");

    human_errors::user("That passkey could not be used.", ADVICE_CEREMONY)
}

/// A credential we do not hold, reported as an ordinary refusal.
fn refused() -> Error {
    human_errors::user("That passkey could not be used.", ADVICE_CEREMONY)
}

/// A ceremony handle presented to the wrong endpoint.
fn wrong_ceremony() -> Error {
    human_errors::user(
        "That request did not match the ceremony it was started for.",
        &["Start again from the sign-in page."],
    )
}

/// A base URL a relying party cannot be derived from.
fn unusable_origin(base_url: &str, why: &str) -> Error {
    human_errors::user(
        format!("We cannot run a passkey ceremony for '{base_url}': {why}."),
        &[
            "Set [server] base_url or [server] domains to the host the console is served from.",
            "A passkey is bound to that host, so it has to be the one browsers actually use.",
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ceremonies themselves are exercised end to end against a real
    /// software authenticator in [`crate::web::api::passkey`], because the only
    /// way to know a ceremony works is to perform one. What is here is the part
    /// that has no HTTP surface: which relying party we decide we are.
    #[test]
    fn the_relying_party_is_the_host_the_console_is_served_from() {
        let passkeys = Passkeys::for_base_url("https://tak.example.com:8446", "rustak").unwrap();

        assert!(format!("{passkeys:?}").contains("tak.example.com"));
        assert!(format!("{passkeys:?}").contains("8446"));
    }

    #[test]
    fn a_loopback_console_may_be_served_on_another_port() {
        // `trunk serve` proxies to the API from its own port during
        // development; the relying party is unchanged by that.
        let passkeys = Passkeys::for_base_url("http://localhost:8446", "rustak").unwrap();

        assert!(format!("{passkeys:?}").contains("any port"));
        assert!(is_loopback("localhost"));
        assert!(is_loopback("console.localhost"));
        assert!(
            !is_loopback("notlocalhost"),
            "the relaxation must not extend to a name that merely ends in one",
        );
    }

    #[test]
    fn a_console_on_a_named_host_is_pinned_to_the_port_it_is_served_on() {
        // The port relaxation is loopback's alone. Everywhere else an origin
        // that differs by port is a different origin, and the check has to say
        // so.
        let passkeys = Passkeys::for_base_url("https://tak.example.com:8446", "rustak").unwrap();
        let origins = passkeys.origins();

        assert!(matches!(origins[0].port, Port::Val(8446)));
        assert!(
            origins[0]
                == webauthn_rp::response::Origin(std::borrow::Cow::Borrowed(
                    "https://tak.example.com:8446"
                )),
        );
        assert!(
            !(origins[0]
                == webauthn_rp::response::Origin(std::borrow::Cow::Borrowed(
                    "https://tak.example.com:9999"
                ))),
            "a different port is a different origin everywhere but loopback",
        );
        assert!(
            !(origins[0]
                == webauthn_rp::response::Origin(std::borrow::Cow::Borrowed(
                    "http://tak.example.com:8446"
                ))),
            "and so is a different scheme",
        );
    }

    #[test]
    fn a_console_reached_at_an_address_cannot_register_a_passkey() {
        // WebAuthn identifies a relying party by domain. Saying so at start-up
        // is far kinder than a browser refusing every prompt without saying why.
        let refused = Passkeys::for_base_url("https://192.0.2.10:8446", "rustak")
            .expect_err("an address is not a relying party");

        assert!(refused.is(human_errors::Kind::User));
        assert!(refused.description().contains("not an address"));
    }

    #[test]
    fn a_base_url_that_names_no_host_says_what_to_set() {
        for base_url in ["", "not-a-url", "file:///data"] {
            let refused = Passkeys::for_base_url(base_url, "rustak")
                .err()
                .unwrap_or_else(|| panic!("'{base_url}' should not be a relying party"));

            assert!(refused.is(human_errors::Kind::User));
            assert!(
                refused
                    .advice()
                    .iter()
                    .any(|line| line.contains("base_url"))
            );
        }
    }

    #[test]
    fn a_stored_passkey_is_listed_by_what_its_owner_called_it() {
        let row = crate::db::repos::PasskeyRow {
            id: rustak_api::identity::PasskeyId::new(4),
            user_id: UserId::new(1),
            credential_id: vec![1, 2, 3],
            public_key: Vec::new(),
            sign_count: 7,
            transports: None,
            label: "Security key".to_string(),
            backup_eligible: false,
            backup_state: false,
            created_at: chrono::Utc::now(),
            last_used_at: None,
        };

        let summary = to_summary(&row);

        assert_eq!(summary.id, row.id);
        assert_eq!(summary.label, "Security key");
        assert_eq!(summary.last_used_at, None);
    }
}
