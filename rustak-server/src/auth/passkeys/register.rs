//! Registering a passkey: the half of the contract that creates a credential.
//!
//! The options handed to the browser are [`PublicKeyCredentialCreationOptions::passkey`]'s,
//! which is the specification's passkey profile: a client-side discoverable
//! credential (`residentKey: "required"`, `requireResidentKey: true`), user
//! verification required, attestation `none`, and a five-minute timeout. Two
//! things are changed from its defaults, and both are written down where they
//! happen: the algorithm list, and the `credProtect` extension.

use rustak_api::PasskeyChallenge;
use rustak_core::prelude::*;
use webauthn_rp::bin::{Decode as _, Encode as _};
use webauthn_rp::request::PublicKeyCredentialDescriptor;
use webauthn_rp::request::register::{
    CoseAlgorithmIdentifier, CoseAlgorithmIdentifiers, CredProtect, Nickname,
    PublicKeyCredentialCreationOptions, PublicKeyCredentialUserEntity, RegistrationServerState,
    Username,
};
use webauthn_rp::response::CredentialId;
use webauthn_rp::response::register::ser_relaxed::RegistrationRelaxed;

use crate::auth::passkey_store::{self, CeremonyKind, USER_HANDLE_LEN};
use crate::db::{
    Database,
    repos::{PasskeyRow, UserRow},
};

use super::{Passkeys, ceremony_failed, malformed, wrong_ceremony};

impl Passkeys {
    /// Begins registering a passkey for an account.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the ceremony cannot be
    /// started or stored, and a [`human_errors::Kind::User`] error when the
    /// account's name is not one an authenticator can be asked to store.
    #[instrument("auth.passkeys.register.start", skip_all, fields(username = %user.username), err(Display))]
    pub async fn start_registration(
        &self,
        db: &Database,
        user: &UserRow,
        label: String,
        bootstrap: bool,
    ) -> Result<PasskeyChallenge, Error> {
        let held = db.passkeys().list_for_user(user.id).await?;
        let exclude = exclude_list(&held)?;

        let handle = passkey_store::user_handle(user.id);
        let mut options = PublicKeyCredentialCreationOptions::passkey(
            self.rp_id(),
            PublicKeyCredentialUserEntity {
                name: Username::try_from(user.username.as_str())
                    .map_err(|err| unusable_name(&err.to_string()))?,
                id: &handle,
                // The display name is whatever an administrator or an identity
                // provider typed, so it can be anything; it is shown by the
                // authenticator and used for nothing else. A name RFC 8266 will
                // not carry is dropped rather than allowed to fail a ceremony
                // that has no other problem.
                display_name: user
                    .display_name
                    .as_deref()
                    .and_then(|name| Nickname::try_from(name).ok()),
            },
            exclude,
        );

        // The contract is ES256, RS256 and EdDSA. `webauthn_rp` offers ES384 as
        // well; it is dropped so that the list a browser sees is the one the
        // API documents rather than a superset that happens to be supported.
        options.pub_key_cred_params =
            CoseAlgorithmIdentifiers::ALL.remove(CoseAlgorithmIdentifier::Es384);

        // `passkey()` asks for `credProtect: userVerificationRequired` and
        // permits the client to enforce it. An authenticator that does not
        // implement the extension then makes the browser fail the ceremony
        // outright, which would exclude platform authenticators that are
        // perfectly able to verify a user. `userVerification: "required"` is
        // already in the authenticator selection and is what actually binds
        // the credential to a verified user, so the extension buys nothing
        // here and costs compatibility.
        options.extensions.cred_protect = CredProtect::None;

        let (state, client) = options.start_ceremony().map_err(ceremony_failed)?;
        let encoded = state.encode().map_err(ceremony_failed)?;

        let challenge_id = passkey_store::begin(
            db,
            CeremonyKind::Register {
                user_id: user.id,
                label,
                bootstrap,
            },
            self.rp_id_name(),
            &encoded,
        )
        .await?;

        let mut options = serde_json::to_value(&client).or_system_err(&[
            "This is unexpected; please report it with the surrounding log entries.",
        ])?;
        self.name_the_installation(&mut options);

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
        // See `finish_login`: the relying party is pinned when the ceremony
        // starts (R-01 M10).
        passkey_store::require_rp_id(&ceremony, self.rp_id_name())?;

        let CeremonyKind::Register {
            user_id,
            label: registered_label,
            bootstrap,
        } = ceremony.kind.clone()
        else {
            return Err(wrong_ceremony());
        };

        let response = serde_json::from_value::<RegistrationRelaxed>(credential.clone())
            .map_err(malformed)?
            .0;

        let encoded = passkey_store::state_of(&ceremony)?;
        let state = RegistrationServerState::<USER_HANDLE_LEN>::decode(encoded.as_slice())
            .map_err(ceremony_failed)?;

        let origins = self.origins();
        let registered = state
            .verify(
                self.rp_id(),
                &response,
                &Self::registration_options(&origins),
            )
            .map_err(ceremony_failed)?;

        // A credential already registered to somebody else would let whoever
        // holds it sign in as either account.
        if db
            .passkeys()
            .get_by_credential_id(registered.id().as_ref())
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
            .create(passkey_store::to_row(user_id, &registered, label)?)
            .await?;

        info!(passkey = %row.id, "Registered a passkey.");

        Ok((user_id, row, bootstrap))
    }
}

/// The credentials this account already holds, which the authenticator is
/// asked not to overwrite.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when a stored credential identifier
/// is not one WebAuthn allows, which means it was not written by this code.
fn exclude_list(held: &[PasskeyRow]) -> Result<Vec<PublicKeyCredentialDescriptor<Vec<u8>>>, Error> {
    held.iter()
        .map(|row| {
            Ok(PublicKeyCredentialDescriptor {
                id: CredentialId::try_from(row.credential_id.clone()).map_err(|_| {
                    human_errors::system(
                        "A stored passkey could not be read.",
                        &["It may have been written by a different version of rustak."],
                    )
                })?,
                transports: passkey_store::transports_of(row),
            })
        })
        .collect()
}

/// A name no authenticator can be asked to store.
fn unusable_name(why: &str) -> Error {
    human_errors::user(
        format!("That account's name cannot be used for a passkey: {why}."),
        &["Rename the account to something an authenticator will accept."],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ceremony itself is exercised end to end against a real software
    /// authenticator in [`crate::web::api::passkey`]. What is here is the shape
    /// of the options a browser is handed, which is the part of the contract
    /// the UI and the e2e suite depend on.
    fn options_for(username: &str) -> serde_json::Value {
        let passkeys = Passkeys::for_base_url("https://tak.example.com", "rustak").unwrap();
        let handle = passkey_store::user_handle(UserId::new(1));

        let mut options = PublicKeyCredentialCreationOptions::passkey(
            passkeys.rp_id(),
            PublicKeyCredentialUserEntity {
                name: Username::try_from(username).unwrap(),
                id: &handle,
                display_name: None,
            },
            Vec::new(),
        );
        options.pub_key_cred_params =
            CoseAlgorithmIdentifiers::ALL.remove(CoseAlgorithmIdentifier::Es384);
        options.extensions.cred_protect = CredProtect::None;

        let (_, client) = options.start_ceremony().unwrap();

        serde_json::to_value(&client).unwrap()
    }

    #[test]
    fn registration_asks_the_authenticator_to_store_the_credential() {
        // Sign-in is a discoverable ceremony, so a credential the authenticator
        // did not keep is a passkey that can never be used.
        let options = options_for("ada");
        let selection = &options["authenticatorSelection"];

        assert_eq!(selection["residentKey"], "required");
        assert_eq!(selection["requireResidentKey"], true);
        assert_eq!(
            selection["userVerification"], "required",
            "both sign-in ceremonies demand a verified user, so registration has to too",
        );
    }

    #[test]
    fn the_algorithms_offered_are_the_three_the_api_documents() {
        let options = options_for("ada");
        let mut offered: Vec<i64> = options["pubKeyCredParams"]
            .as_array()
            .expect("the options carry an algorithm list")
            .iter()
            .map(|entry| entry["alg"].as_i64().expect("a COSE identifier"))
            .collect();
        offered.sort_unstable();

        assert_eq!(
            offered,
            vec![-257, -8, -7],
            "ES256, RS256 and EdDSA, and nothing the API does not name",
        );
    }

    #[test]
    fn nothing_is_attested_and_the_challenge_travels_as_base64url() {
        let options = options_for("ada");

        assert_eq!(options["attestation"], "none");
        assert_eq!(options["rp"]["id"], "tak.example.com");
        assert_eq!(options["user"]["name"], "ada");

        let challenge = options["challenge"]
            .as_str()
            .expect("the options carry a challenge");
        assert!(
            challenge
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "the UI decodes this with a strict base64url decoder",
        );

        let user_id = options["user"]["id"].as_str().expect("an account handle");
        assert_eq!(
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, user_id)
                .unwrap()
                .len(),
            USER_HANDLE_LEN,
        );
    }

    #[test]
    fn a_credential_identifier_nothing_wrote_is_reported_rather_than_panicking() {
        let row = PasskeyRow {
            id: rustak_api::identity::PasskeyId::new(1),
            user_id: UserId::new(1),
            credential_id: Vec::new(),
            public_key: Vec::new(),
            sign_count: 0,
            transports: None,
            label: "Phone".to_string(),
            backup_eligible: false,
            backup_state: false,
            created_at: chrono::Utc::now(),
            last_used_at: None,
        };

        let refused = exclude_list(std::slice::from_ref(&row)).unwrap_err();

        assert!(refused.is(human_errors::Kind::System));
    }
}
