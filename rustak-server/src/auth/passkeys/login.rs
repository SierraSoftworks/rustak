//! Signing in with a passkey: the half of the contract that proves possession.
//!
//! Two ceremonies, and they are not interchangeable. The *discoverable* one
//! names no account and lists no credentials: the authenticator says which
//! account it holds, which is what lets the sign-in page have no username field
//! and therefore not be a way to ask which accounts exist. The
//! *username-assisted* one lists exactly that account's credentials, and is the
//! fallback for a key registered elsewhere or by an older version of this
//! server.
//!
//! A ceremony handle is bound to the kind it was started for, so an assertion
//! obtained from a discoverable challenge cannot be presented to the
//! username-assisted verifier or the other way round — the two carry different
//! requirements about the user handle, and letting them cross would be letting
//! the client pick which check runs.

use rustak_api::PasskeyChallenge;
use rustak_core::prelude::*;
use webauthn_rp::AuthenticatedCredential;
use webauthn_rp::bin::{Decode as _, Encode as _};
use webauthn_rp::request::auth::{
    AllowedCredentials, DiscoverableAuthenticationServerState,
    DiscoverableCredentialRequestOptions, NonDiscoverableAuthenticationServerState,
    NonDiscoverableCredentialRequestOptions,
};
use webauthn_rp::request::{
    Credentials as _, PublicKeyCredentialDescriptor, UserVerificationRequirement,
};
use webauthn_rp::response::CredentialId;
use webauthn_rp::response::auth::ser_relaxed::{
    DiscoverableAuthenticationRelaxed, NonDiscoverableAuthenticationRelaxed,
};

use crate::auth::passkey_store::{self, CeremonyKind, USER_HANDLE_LEN};
use crate::db::{
    Database,
    repos::{PasskeyRow, UserRow},
};

use super::{Passkeys, ceremony_failed, malformed, refused, wrong_ceremony};

impl Passkeys {
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
            let (state, client) = DiscoverableCredentialRequestOptions::passkey(self.rp_id())
                .start_ceremony()
                .map_err(ceremony_failed)?;

            return self
                .challenge(
                    db,
                    CeremonyKind::Discover,
                    &state.encode().map_err(ceremony_failed)?,
                    &client,
                )
                .await;
        };

        let rows = db.passkeys().list_for_user(user.id).await?;
        if rows.is_empty() {
            return Err(human_errors::user(
                "That account has no passkey registered.",
                &["Sign in with your identity provider, or ask an administrator to add one."],
            ));
        }

        let mut allowed = AllowedCredentials::with_capacity(rows.len());
        for row in &rows {
            let descriptor = PublicKeyCredentialDescriptor {
                id: CredentialId::try_from(row.credential_id.clone()).map_err(|_| unreadable())?,
                transports: passkey_store::transports_of(row),
            };

            if !allowed.push(descriptor.into()) {
                return Err(unreadable());
            }
        }

        let mut options =
            NonDiscoverableCredentialRequestOptions::second_factor(self.rp_id(), allowed)
                .map_err(ceremony_failed)?;

        // `second_factor()` builds the ceremony for a credential that is one
        // factor among several, so it does not demand user verification. Here
        // the passkey is the *whole* of the sign-in, and the credential was
        // registered with verification required, so an unverified assertion
        // would be a weaker proof than the one the credential promises.
        options.options().user_verification = UserVerificationRequirement::Required;

        let (state, client) = options.start_ceremony().map_err(ceremony_failed)?;

        self.challenge(
            db,
            CeremonyKind::Login { user_id: user.id },
            &state.encode().map_err(ceremony_failed)?,
            &client,
        )
        .await
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
        let encoded = passkey_store::state_of(&ceremony)?;

        let raw_id = raw_id_of(credential)?;
        let row = db
            .passkeys()
            .get_by_credential_id(&raw_id)
            .await?
            .ok_or_else(refused)?;

        let sign_count = {
            let handle = passkey_store::user_handle(row.user_id);
            let mut held = AuthenticatedCredential::new(
                passkey_store::credential_id_of(&row)?,
                &handle,
                passkey_store::static_state_of(&row)?,
                passkey_store::dynamic_state_of(&row),
            )
            .map_err(ceremony_failed)?;

            let origins = self.origins();
            let options = Self::authentication_options(&origins);

            match ceremony.kind {
                CeremonyKind::Login { user_id } if user_id == row.user_id => {
                    let response = serde_json::from_value::<
                        NonDiscoverableAuthenticationRelaxed<USER_HANDLE_LEN>,
                    >(credential.clone())
                    .map_err(malformed)?
                    .0;

                    NonDiscoverableAuthenticationServerState::decode(encoded.as_slice())
                        .map_err(ceremony_failed)?
                        .verify(self.rp_id(), &response, &mut held, &options)
                        .map_err(ceremony_failed)?;
                }
                // A handle started for one account cannot be spent on another's
                // credential, whatever the assertion itself says.
                CeremonyKind::Login { .. } => return Err(refused()),
                CeremonyKind::Discover => {
                    let response = serde_json::from_value::<
                        DiscoverableAuthenticationRelaxed<USER_HANDLE_LEN>,
                    >(credential.clone())
                    .map_err(malformed)?
                    .0;

                    DiscoverableAuthenticationServerState::decode(encoded.as_slice())
                        .map_err(ceremony_failed)?
                        .verify(self.rp_id(), &response, &mut held, &options)
                        .map_err(ceremony_failed)?;
                }
                CeremonyKind::Register { .. } => return Err(wrong_ceremony()),
            }

            held.dynamic_state().sign_count
        };

        // Not bookkeeping: the counter is how a cloned authenticator is caught,
        // and it only works if the stored value moves with every sign-in.
        db.passkeys().record_use(row.id, sign_count).await?;

        info!(passkey = %row.id, "A passkey signed somebody in.");

        Ok(row)
    }

    /// Stores a ceremony and returns what the browser is handed.
    async fn challenge<T: Serialize>(
        &self,
        db: &Database,
        kind: CeremonyKind,
        state: &[u8],
        client: &T,
    ) -> Result<PasskeyChallenge, Error> {
        let options = serde_json::to_value(client).or_system_err(&[
            "This is unexpected; please report it with the surrounding log entries.",
        ])?;

        Ok(PasskeyChallenge {
            challenge_id: passkey_store::begin(db, kind, state).await?,
            options,
        })
    }
}

/// The credential identifier an assertion names.
///
/// Read out of the JSON rather than out of a parsed assertion because the row
/// it selects is what the parsed assertion then has to be verified *against*,
/// and the two parsers disagree about whether a user handle is required.
fn raw_id_of(credential: &serde_json::Value) -> Result<Vec<u8>, Error> {
    use base64::Engine as _;

    credential
        .get("rawId")
        .or_else(|| credential.get("id"))
        .and_then(serde_json::Value::as_str)
        .and_then(|id| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(id)
                .ok()
        })
        .ok_or_else(refused)
}

/// What a caller is told when something we wrote cannot be read back.
fn unreadable() -> Error {
    human_errors::system(
        "A stored passkey could not be read.",
        &["It may have been written by a different version of rustak."],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_assertion_names_the_credential_that_signed_it() {
        let credential = serde_json::json!({
            "id": "AQID",
            "rawId": "AQID",
            "type": "public-key",
        });

        assert_eq!(raw_id_of(&credential).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn an_assertion_that_names_nothing_is_an_ordinary_refusal() {
        // Never a distinguishable failure: which check an assertion failed is
        // not something whoever presented it should learn.
        for credential in [
            serde_json::json!({}),
            serde_json::json!({ "rawId": 7 }),
            // Not base64url — standard base64 with padding, which a strict
            // decoder must refuse rather than quietly accept.
            serde_json::json!({ "rawId": "AQ==" }),
            serde_json::json!({ "rawId": "not base64!" }),
        ] {
            let error = raw_id_of(&credential).unwrap_err();

            assert!(error.is(human_errors::Kind::User));
            assert_eq!(error.description(), refused().description());
        }
    }
}
