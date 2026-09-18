//! A software authenticator, so a passkey ceremony can be tested end to end.
//!
//! It does what a security key does: holds a key pair per credential, signs the
//! authenticator data and the hash of the client data, and counts. The server
//! under test does not know it is not a phone.
//!
//! # Why it is written out rather than depended on
//!
//! `webauthn-authenticator-rs` has exactly this, and it reaches it through
//! OpenSSL — a system library this workspace has deliberately avoided
//! everywhere else (rustls over `aws-lc-rs`, `protox` instead of `protoc`,
//! `rcgen` instead of a shelled-out toolchain), and one that would have to be
//! installed on every cross-compilation image for `cargo test` to run. The
//! ceremony it performs is about a hundred lines of CBOR and one signature,
//! and the crates for all three algorithms are already here.
//!
//! # Why it can misbehave on purpose
//!
//! Every builder on this type manufactures one specific lie a real attacker
//! would tell: a ceremony run from somewhere else ([`SoftAuthenticator::at_origin`]), a
//! credential asserted for a different relying party ([`SoftAuthenticator::at_rp_id`]), an
//! assertion nobody was present or verified for
//! ([`SoftAuthenticator::without_user_verification`], [`SoftAuthenticator::without_user_presence`]), a
//! signature that does not match what was signed
//! ([`SoftAuthenticator::with_tampered_signature`]), a credential cloned onto a second
//! device ([`SoftAuthenticator::clone_credential`]), and a key using an algorithm that was
//! never offered ([`Algorithm::Unsupported`]). Each one exists so that a
//! refusal can be asserted rather than assumed.

mod keys;

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine as _;
use sha2::{Digest as _, Sha256};

pub use keys::Algorithm;

/// Base64 as WebAuthn uses it on the wire.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// User present.
const FLAG_UP: u8 = 0x01;

/// User verified. Passkeys are registered and asserted with verification
/// required, so an authenticator that did not set this would be refused.
const FLAG_UV: u8 = 0x04;

/// Backup eligible.
const FLAG_BE: u8 = 0x08;

/// Backup state.
const FLAG_BS: u8 = 0x10;

/// Attested credential data is present. Registration only.
const FLAG_AT: u8 = 0x40;

/// A credential this authenticator holds.
#[derive(Clone)]
struct Stored {
    /// The account the relying party said it was for.
    user_handle: Vec<u8>,
    /// What the authenticator will report next time it signs.
    counter: u32,
    /// What it signs with.
    algorithm: Algorithm,
}

/// An authenticator a test can plug in.
pub struct SoftAuthenticator {
    rp_id: String,
    origin: String,
    algorithm: Algorithm,
    /// The flags every assertion sets, before the attested-data flag.
    flags: u8,
    /// Whether to corrupt the signature after producing it.
    tamper: bool,
    /// Whether to claim the ceremony happened in a nested browsing context.
    cross_origin: bool,
    /// Whether to write the other ceremony's name into the client data.
    swap_ceremony_type: bool,
    credentials: Mutex<HashMap<Vec<u8>, Stored>>,
}

impl SoftAuthenticator {
    /// An authenticator that will answer ceremonies from `origin`.
    ///
    /// # Panics
    ///
    /// If `origin` is not a URL with a host, which is not something a test can
    /// carry on past.
    pub fn new(origin: &str) -> Self {
        let parsed = url::Url::parse(origin).expect("an origin a browser could be at");
        let rp_id = parsed
            .host_str()
            .expect("an origin with a host")
            .to_string();

        Self {
            rp_id,
            origin: origin.trim_end_matches('/').to_string(),
            algorithm: Algorithm::default(),
            flags: FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS,
            tamper: false,
            cross_origin: false,
            swap_ceremony_type: false,
            credentials: Mutex::new(HashMap::new()),
        }
    }

    /// The same authenticator, signing with a different algorithm.
    pub fn signing_with(mut self, algorithm: Algorithm) -> Self {
        self.algorithm = algorithm;
        self
    }

    /// The same authenticator, answering as though the page were somewhere
    /// else.
    ///
    /// It holds the same credentials and signs with the same key; only the
    /// origin it writes into the client data differs. That is the phishing
    /// case in its purest form — everything about the assertion is genuine
    /// except where it was produced.
    pub fn at_origin(&self, origin: &str) -> Self {
        self.but(|clone| clone.origin = origin.trim_end_matches('/').to_string())
    }

    /// The same authenticator, signing over a different relying party.
    ///
    /// The origin in the client data is still ours, so the only thing wrong is
    /// the relying-party identifier hashed into the authenticator data — the
    /// half of the binding a browser does not police.
    pub fn at_rp_id(&self, rp_id: &str) -> Self {
        self.but(|clone| clone.rp_id = rp_id.to_string())
    }

    /// The same authenticator, asserting without verifying the user.
    pub fn without_user_verification(&self) -> Self {
        self.but(|clone| clone.flags &= !FLAG_UV)
    }

    /// The same authenticator, asserting without the user being there at all.
    pub fn without_user_presence(&self) -> Self {
        self.but(|clone| clone.flags &= !FLAG_UP)
    }

    /// The same authenticator, producing a signature over something else.
    pub fn with_tampered_signature(&self) -> Self {
        self.but(|clone| clone.tamper = true)
    }

    /// The same authenticator, claiming the page was inside another site.
    ///
    /// A ceremony run in a frame somebody else owns is a ceremony the person
    /// may not have understood they were completing.
    pub fn in_a_frame(&self) -> Self {
        self.but(|clone| clone.cross_origin = true)
    }

    /// The same authenticator, labelling each ceremony as the other one.
    ///
    /// The `type` in the client data is what stops an assertion being replayed
    /// as a registration and the other way round.
    pub fn mislabelling_the_ceremony(&self) -> Self {
        self.but(|clone| clone.swap_ceremony_type = true)
    }

    /// Performs a registration, as `navigator.credentials.create` would.
    ///
    /// # Panics
    ///
    /// If the options are not the ones a relying party sends, which in a test
    /// means the server built them wrongly.
    pub fn create(&self, options: &serde_json::Value) -> serde_json::Value {
        let challenge = challenge_of(options);
        let user_handle = B64
            .decode(
                options["user"]["id"]
                    .as_str()
                    .expect("the options name the account"),
            )
            .expect("a base64url account handle");

        let credential_id = random_id();
        let client_data = self.client_data("webauthn.create", &challenge);

        let mut attested = Vec::new();
        attested.extend_from_slice(&[0u8; 16]); // No attestation, so no AAGUID.
        attested.extend_from_slice(&(credential_id.len() as u16).to_be_bytes());
        attested.extend_from_slice(&credential_id);
        attested.extend_from_slice(&self.algorithm.cose_key());

        let auth_data = self.auth_data(self.flags | FLAG_AT, 0, &attested);

        self.credentials
            .lock()
            .expect("the credential store")
            .insert(
                credential_id.clone(),
                Stored {
                    user_handle,
                    counter: 0,
                    algorithm: self.algorithm,
                },
            );

        serde_json::json!({
            "id": B64.encode(&credential_id),
            "rawId": B64.encode(&credential_id),
            "type": "public-key",
            "response": {
                "attestationObject": B64.encode(keys::attestation_object(&auth_data)),
                "clientDataJSON": B64.encode(&client_data),
                "transports": ["internal", "hybrid"],
            },
            "extensions": {},
        })
    }

    /// Performs an assertion, as `navigator.credentials.get` would.
    ///
    /// # Panics
    ///
    /// If the options name no credential this authenticator holds, which in a
    /// test means the server offered the wrong ones.
    pub fn get(&self, options: &serde_json::Value) -> serde_json::Value {
        let challenge = challenge_of(options);
        let credential_id = self.pick(options);

        let (user_handle, counter, algorithm) = {
            let mut held = self.credentials.lock().expect("the credential store");
            let stored = held
                .get_mut(&credential_id)
                .expect("a credential this authenticator holds");

            // A real authenticator increments on every assertion, and the
            // server treats a counter that does not move as a cloned device.
            stored.counter += 1;

            (stored.user_handle.clone(), stored.counter, stored.algorithm)
        };

        let client_data = self.client_data("webauthn.get", &challenge);
        let auth_data = self.auth_data(self.flags, counter, &[]);

        let mut signed = auth_data.clone();
        signed.extend_from_slice(&Sha256::digest(&client_data));

        if self.tamper {
            // Sign something else entirely: a signature that verifies against
            // data other than what was presented is the whole of the attack.
            signed[0] ^= 0xff;
        }

        serde_json::json!({
            "id": B64.encode(&credential_id),
            "rawId": B64.encode(&credential_id),
            "type": "public-key",
            "response": {
                "authenticatorData": B64.encode(&auth_data),
                "clientDataJSON": B64.encode(&client_data),
                "signature": B64.encode(algorithm.sign(&signed)),
                "userHandle": B64.encode(&user_handle),
            },
            "extensions": {},
        })
    }

    /// Winds a credential's counter back, as a cloned authenticator would.
    ///
    /// # Panics
    ///
    /// If nothing has been registered yet.
    pub fn clone_credential(&self) {
        let mut held = self.credentials.lock().expect("the credential store");
        let stored = held.values_mut().next().expect("a credential to clone");

        stored.counter = 0;
    }

    /// A copy of this authenticator with one thing changed.
    fn but(&self, change: impl FnOnce(&mut Self)) -> Self {
        let mut clone = Self {
            rp_id: self.rp_id.clone(),
            origin: self.origin.clone(),
            algorithm: self.algorithm,
            flags: self.flags,
            tamper: self.tamper,
            cross_origin: self.cross_origin,
            swap_ceremony_type: self.swap_ceremony_type,
            credentials: Mutex::new(
                self.credentials
                    .lock()
                    .expect("the credential store")
                    .clone(),
            ),
        };
        change(&mut clone);

        clone
    }

    /// The client data a browser would produce.
    fn client_data(&self, kind: &str, challenge: &[u8]) -> Vec<u8> {
        let kind = match (self.swap_ceremony_type, kind) {
            (false, kind) => kind,
            (true, "webauthn.create") => "webauthn.get",
            (true, _) => "webauthn.create",
        };

        serde_json::to_vec(&serde_json::json!({
            "type": kind,
            "challenge": B64.encode(challenge),
            "origin": self.origin,
            "crossOrigin": self.cross_origin,
        }))
        .expect("render the client data")
    }

    /// The authenticator data for one ceremony.
    fn auth_data(&self, flags: u8, counter: u32, attested: &[u8]) -> Vec<u8> {
        let mut data = Vec::with_capacity(37 + attested.len());

        data.extend_from_slice(&Sha256::digest(self.rp_id.as_bytes()));
        data.push(flags);
        data.extend_from_slice(&counter.to_be_bytes());
        data.extend_from_slice(attested);

        data
    }

    /// Which credential to assert with.
    fn pick(&self, options: &serde_json::Value) -> Vec<u8> {
        let held = self.credentials.lock().expect("the credential store");

        if let Some(allowed) = options
            .get("allowCredentials")
            .and_then(serde_json::Value::as_array)
            .filter(|list| !list.is_empty())
        {
            for entry in allowed {
                let id = B64
                    .decode(entry["id"].as_str().expect("a credential descriptor"))
                    .expect("a base64url credential id");

                if held.contains_key(&id) {
                    return id;
                }
            }

            panic!("the relying party offered no credential this authenticator holds");
        }

        held.keys()
            .next()
            .cloned()
            .expect("a credential to sign in with")
    }
}

/// The challenge the relying party sent.
fn challenge_of(options: &serde_json::Value) -> Vec<u8> {
    B64.decode(
        options["challenge"]
            .as_str()
            .expect("the options carry a challenge"),
    )
    .expect("a base64url challenge")
}

/// A fresh credential identifier.
fn random_id() -> Vec<u8> {
    use rand::Rng as _;

    let mut id = [0u8; 32];
    rand::rng().fill_bytes(&mut id);

    id.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "https://tak.example.com";

    fn registration_options() -> serde_json::Value {
        serde_json::json!({
            "challenge": B64.encode([7u8; 16]),
            "user": { "id": B64.encode([1u8; 16]) },
        })
    }

    #[test]
    fn a_registration_carries_the_relying_party_it_was_run_for() {
        let authenticator = SoftAuthenticator::new(ORIGIN);
        let credential = authenticator.create(&registration_options());

        let attestation = B64
            .decode(
                credential["response"]["attestationObject"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        let value: ciborium::Value =
            ciborium::from_reader(attestation.as_slice()).expect("valid CBOR");
        let ciborium::Value::Map(entries) = value else {
            panic!("an attestation object is a map");
        };

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0.as_text(), Some("fmt"));
        assert_eq!(entries[0].1.as_text(), Some("none"));

        let auth_data = entries[2].1.as_bytes().expect("authData is a byte string");
        assert_eq!(
            &auth_data[..32],
            Sha256::digest(b"tak.example.com").as_slice()
        );
        assert_eq!(auth_data[32] & FLAG_AT, FLAG_AT);
        assert_eq!(auth_data[32] & FLAG_UV, FLAG_UV);
    }

    #[test]
    fn every_lie_this_authenticator_can_tell_changes_exactly_one_thing() {
        let honest = SoftAuthenticator::new(ORIGIN);
        honest.create(&registration_options());

        assert_eq!(
            honest.at_origin("https://elsewhere.example").origin,
            "https://elsewhere.example"
        );
        assert_eq!(
            honest.at_rp_id("elsewhere.example").rp_id,
            "elsewhere.example"
        );
        assert_eq!(honest.without_user_verification().flags & FLAG_UV, 0);
        assert_eq!(honest.without_user_presence().flags & FLAG_UP, 0);
        assert!(honest.with_tampered_signature().tamper);

        // …and every one of them keeps the credential, so the refusal under
        // test is the lie rather than a missing key.
        for liar in [
            honest.at_origin("https://elsewhere.example"),
            honest.at_rp_id("elsewhere.example"),
            honest.without_user_verification(),
            honest.with_tampered_signature(),
        ] {
            assert_eq!(liar.credentials.lock().unwrap().len(), 1);
        }
    }

    #[test]
    fn the_counter_moves_with_every_assertion_and_a_clone_winds_it_back() {
        let authenticator = SoftAuthenticator::new(ORIGIN);
        authenticator.create(&registration_options());

        let options = serde_json::json!({ "challenge": B64.encode([9u8; 16]) });
        authenticator.get(&options);
        authenticator.get(&options);

        assert_eq!(
            authenticator
                .credentials
                .lock()
                .unwrap()
                .values()
                .next()
                .unwrap()
                .counter,
            2,
        );

        authenticator.clone_credential();

        assert_eq!(
            authenticator
                .credentials
                .lock()
                .unwrap()
                .values()
                .next()
                .unwrap()
                .counter,
            0,
        );
    }
}
