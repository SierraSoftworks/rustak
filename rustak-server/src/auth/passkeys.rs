//! Passkeys: the only way to sign in to rustak without an identity provider.
//!
//! There are no local passwords, so this is it. The ceremonies are
//! `webauthn-rs`'s, and what is here is the four decisions around them: which
//! relying party we are, where the challenge state lives, which credentials a
//! challenge lists, and what a successful assertion means.
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
//! registration asks for `residentKey: "required"` — see `require_discoverable`
//! below, which is also where the reason it is a patch rather than a parameter
//! is written down. A username-assisted sign-in is
//! still offered as a fallback ([`Passkeys::start_login`] with an account), for
//! a key registered elsewhere or by an older version of this server.
//!
//! # Why a counter that goes backwards is fatal
//!
//! `webauthn-rs` compares the authenticator's signature counter against the one
//! we stored. A counter that has not moved forward is the signal that the
//! credential has been cloned, and it refuses the assertion. Recording the new
//! counter after every sign-in is therefore not bookkeeping, it is the check.

use rustak_api::{PasskeyChallenge, PasskeySummary};
use rustak_core::prelude::*;
use webauthn_rs::prelude::*;

use crate::db::{
    Database,
    repos::{PasskeyRow, UserRow},
};

use super::passkey_store::{self, Ceremony, CeremonyKind};

/// What a browser is told when a ceremony will not work.
const ADVICE_CEREMONY: &[&str] = &[
    "Try again, and complete the prompt your browser shows.",
    "A passkey only works on the host it was registered against.",
];

/// The relying party this server is, ready to run a ceremony.
pub struct Passkeys {
    webauthn: Webauthn,
}

impl std::fmt::Debug for Passkeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Passkeys")
            .field("origins", &self.webauthn.get_allowed_origins())
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
            Url::parse(base_url).map_err(|err| unusable_origin(base_url, &err.to_string()))?;

        // WebAuthn identifies a relying party by domain, so an address is not
        // one — a console reached at `https://192.0.2.10` cannot register a
        // passkey at all, whatever else is configured.
        let rp_id = origin
            .domain()
            .ok_or_else(|| {
                unusable_origin(base_url, "a passkey is bound to a name, not an address")
            })?
            .to_string();

        let webauthn = WebauthnBuilder::new(&rp_id, &origin)
            .and_then(|builder| {
                builder
                    .rp_name(rp_name)
                    .allow_any_port(is_loopback(&rp_id))
                    .build()
            })
            .map_err(|err| unusable_origin(base_url, &err.to_string()))?;

        Ok(Self { webauthn })
    }

    /// Begins registering a passkey for an account.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the ceremony cannot be
    /// started or stored.
    #[instrument("auth.passkeys.register.start", skip_all, fields(username = %user.username), err(Display))]
    pub async fn start_registration(
        &self,
        db: &Database,
        user: &UserRow,
        label: String,
        bootstrap: bool,
    ) -> Result<PasskeyChallenge, Error> {
        let existing = db.passkeys().list_for_user(user.id).await?;
        let exclude: Vec<CredentialID> = existing
            .iter()
            .map(|row| CredentialID::from(row.credential_id.clone()))
            .collect();

        let (options, state) = self
            .webauthn
            .start_passkey_registration(
                passkey_store::user_handle(user.id),
                user.username.as_str(),
                user.display_name
                    .as_deref()
                    .unwrap_or(user.username.as_str()),
                Some(exclude).filter(|list| !list.is_empty()),
            )
            .map_err(ceremony_failed)?;

        let challenge_id = passkey_store::begin(
            db,
            CeremonyKind::Register {
                user_id: user.id,
                label,
                bootstrap,
            },
            &state,
        )
        .await?;

        let mut options = public_key_of(&options)?;
        require_discoverable(&mut options);

        Ok(PasskeyChallenge {
            challenge_id,
            options,
        })
    }

    /// Completes a registration, storing the credential.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the ceremony has expired, the
    /// browser's response does not verify, or the credential is already
    /// registered — to this account or to anybody else.
    #[instrument("auth.passkeys.register.finish", skip_all, err(Display))]
    pub async fn finish_registration(
        &self,
        db: &Database,
        challenge_id: &str,
        credential: &serde_json::Value,
        label: Option<&str>,
    ) -> Result<(UserId, PasskeyRow, bool), Error> {
        let ceremony = passkey_store::claim(db, challenge_id).await?;

        let CeremonyKind::Register {
            user_id,
            label: registered_label,
            bootstrap,
        } = ceremony.kind.clone()
        else {
            return Err(wrong_ceremony());
        };

        let response: RegisterPublicKeyCredential =
            serde_json::from_value(credential.clone()).map_err(malformed)?;
        let state: PasskeyRegistration = state_of(&ceremony)?;

        let passkey = self
            .webauthn
            .finish_passkey_registration(&response, &state)
            .map_err(ceremony_failed)?;

        // A credential already registered to somebody else would let whoever
        // holds it sign in as either account.
        if db
            .passkeys()
            .get_by_credential_id(passkey.cred_id().as_ref())
            .await?
            .is_some()
        {
            return Err(human_errors::user(
                "That passkey is already registered.",
                &["Use it to sign in, or register a different one."],
            ));
        }

        let label = label
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map_or(registered_label, str::to_string);

        let row = db
            .passkeys()
            .create(passkey_store::to_row(user_id, &passkey, label)?)
            .await?;

        info!(passkey = %row.id, "Registered a passkey.");

        Ok((user_id, row, bootstrap))
    }

    /// Begins a sign-in.
    ///
    /// With no account named the challenge lists no credentials and the
    /// authenticator says which one it holds, which is the better flow: naming
    /// an account before proving anything is what would let somebody ask this
    /// endpoint which accounts exist.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when a named account has no
    /// passkeys, and a [`human_errors::Kind::System`] error when the ceremony
    /// cannot be started or stored.
    #[instrument("auth.passkeys.login.start", skip_all, err(Display))]
    pub async fn start_login(
        &self,
        db: &Database,
        user: Option<&UserRow>,
    ) -> Result<PasskeyChallenge, Error> {
        let Some(user) = user else {
            let (options, state) = self
                .webauthn
                .start_discoverable_authentication()
                .map_err(ceremony_failed)?;

            let challenge_id = passkey_store::begin(db, CeremonyKind::Discover, &state).await?;

            return Ok(PasskeyChallenge {
                challenge_id,
                options: public_key_of(&options)?,
            });
        };

        let rows = db.passkeys().list_for_user(user.id).await?;
        let credentials = rows
            .iter()
            .map(passkey_store::to_passkey)
            .collect::<Result<Vec<_>, _>>()?;

        if credentials.is_empty() {
            return Err(human_errors::user(
                "That account has no passkey registered.",
                &["Sign in with your identity provider, or ask an administrator to add one."],
            ));
        }

        let (options, state) = self
            .webauthn
            .start_passkey_authentication(&credentials)
            .map_err(ceremony_failed)?;

        let challenge_id =
            passkey_store::begin(db, CeremonyKind::Login { user_id: user.id }, &state).await?;

        Ok(PasskeyChallenge {
            challenge_id,
            options: public_key_of(&options)?,
        })
    }

    /// Completes a sign-in, returning whose passkey signed the challenge.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the ceremony has expired, the
    /// assertion does not verify, or the credential is not one we hold.
    #[instrument("auth.passkeys.login.finish", skip_all, err(Display))]
    pub async fn finish_login(
        &self,
        db: &Database,
        challenge_id: &str,
        credential: &serde_json::Value,
    ) -> Result<PasskeyRow, Error> {
        let ceremony = passkey_store::claim(db, challenge_id).await?;
        let response: PublicKeyCredential =
            serde_json::from_value(credential.clone()).map_err(malformed)?;

        let row = db
            .passkeys()
            .get_by_credential_id(response.raw_id.as_ref())
            .await?
            .ok_or_else(refused)?;

        let result = match ceremony.kind {
            CeremonyKind::Login { user_id } if user_id == row.user_id => self
                .webauthn
                .finish_passkey_authentication(&response, &state_of(&ceremony)?)
                .map_err(ceremony_failed)?,
            CeremonyKind::Login { .. } => return Err(refused()),
            CeremonyKind::Discover => self
                .webauthn
                .finish_discoverable_authentication(
                    &response,
                    state_of(&ceremony)?,
                    &[DiscoverableKey::from(&passkey_store::to_passkey(&row)?)],
                )
                .map_err(ceremony_failed)?,
            CeremonyKind::Register { .. } => return Err(wrong_ceremony()),
        };

        // Not bookkeeping: the counter is how a cloned authenticator is caught,
        // and it only works if the stored value moves with every sign-in.
        db.passkeys().record_use(row.id, result.counter()).await?;

        info!(passkey = %row.id, "A passkey signed somebody in.");

        Ok(row)
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

/// Asks the authenticator to store the credential where it can be found again.
///
/// # Why this is patched rather than asked for
///
/// The sign-in prompt has no username field, and deliberately so: naming an
/// account before proving anything is how somebody finds out which accounts
/// exist. That makes sign-in a *discoverable* ceremony — an empty
/// `allowCredentials`, with the authenticator saying which account it holds —
/// and a credential that is not discoverable cannot answer one.
///
/// `webauthn-rs` 0.5's [`Webauthn::start_passkey_registration`] hard-codes
/// `require_resident_key(false)`, which becomes `residentKey: "discouraged"` on
/// the wire, and the high-level API exposes no way to say otherwise. So the
/// emitted options are corrected here. It is safe to do on the serialised form
/// alone: the stored ceremony state carries the same flag, but nothing at
/// verification time reads it (`webauthn-rs-core` destructures it as `_`), so
/// the two cannot disagree about anything that matters.
///
/// `required` rather than `preferred` because the two are identical in every
/// major browser, and `required` is the one that fails at creation rather than
/// producing a credential that cannot sign in. `userVerification` is left at
/// `webauthn-rs`'s `required`: both sign-in ceremonies demand a verified user,
/// so a credential registered without verification would register happily and
/// then be refused at every attempt to use it.
fn require_discoverable(options: &mut serde_json::Value) {
    let Some(selection) = options
        .get_mut("authenticatorSelection")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    selection.insert("residentKey".to_string(), "required".into());
    selection.insert("requireResidentKey".to_string(), true.into());
}

/// The `publicKey` member the browser is handed.
fn public_key_of<T: Serialize>(options: &T) -> Result<serde_json::Value, Error> {
    let mut rendered = serde_json::to_value(options).or_system_err(&[
        "This is unexpected; please report it with the surrounding log entries.",
    ])?;

    Ok(rendered
        .get_mut("publicKey")
        .map(serde_json::Value::take)
        .unwrap_or(rendered))
}

/// The `webauthn-rs` state a ceremony was stored with.
fn state_of<T: DeserializeOwned>(ceremony: &Ceremony) -> Result<T, Error> {
    serde_json::from_value(ceremony.state.clone()).or_system_err(&[
        "A stored ceremony could not be read; it may have been written by a different version of rustak.",
    ])
}

/// What a browser is told when its response does not verify.
///
/// Deliberately the same for every cause: which check an assertion failed is
/// not something whoever presented it should learn.
fn ceremony_failed(err: WebauthnError) -> Error {
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

/// Whether a host is one a development console is served from.
///
/// `trunk serve` proxies to the API from its own port, so the origin differs
/// from the server's by port alone. The relying party is unchanged by that, and
/// the relaxation applies here and nowhere else.
fn is_loopback(rp_id: &str) -> bool {
    rp_id == "localhost" || rp_id.ends_with(".localhost")
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
    }

    #[test]
    fn a_loopback_console_may_be_served_on_another_port() {
        // `trunk serve` proxies to the API from its own port during
        // development; the relying party is unchanged by that.
        let passkeys = Passkeys::for_base_url("http://localhost:8446", "rustak").unwrap();

        assert!(format!("{passkeys:?}").contains("localhost"));
        assert!(is_loopback("localhost"));
        assert!(is_loopback("console.localhost"));
        assert!(
            !is_loopback("notlocalhost"),
            "the relaxation must not extend to a name that merely ends in one",
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
    fn registration_asks_the_authenticator_to_store_the_credential() {
        // Sign-in is a discoverable ceremony, so a credential the authenticator
        // did not keep is a passkey that can never be used. `webauthn-rs` asks
        // for `discouraged` and gives us no way to say otherwise, so the
        // emitted options are corrected on the way out.
        let mut options = serde_json::json!({
            "challenge": "c2FsdA",
            "authenticatorSelection": {
                "requireResidentKey": false,
                "residentKey": "discouraged",
                "userVerification": "required",
            },
        });

        require_discoverable(&mut options);

        let selection = &options["authenticatorSelection"];
        assert_eq!(selection["residentKey"], "required");
        assert_eq!(selection["requireResidentKey"], true);
        assert_eq!(
            selection["userVerification"], "required",
            "both sign-in ceremonies demand a verified user, so registration has to too",
        );
    }

    #[test]
    fn options_without_an_authenticator_selection_are_left_alone() {
        // Nothing here should be able to turn a ceremony `webauthn-rs` shaped
        // into one it did not, so a shape we do not recognise is passed through
        // rather than invented.
        let mut options = serde_json::json!({ "challenge": "c2FsdA" });
        let original = options.clone();

        require_discoverable(&mut options);

        assert_eq!(options, original);
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
