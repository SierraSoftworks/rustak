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
//! ceremony it performs is about a hundred lines of CBOR and one RSA signature,
//! and the crates for both are already here.
//!
//! # Why RSA rather than the usual P-256
//!
//! `RS256` is one of the two algorithms `webauthn-rs` accepts, and the `rsa`
//! crate is already a dependency because the certificate authority and the
//! token signing keys need it. Reaching for an elliptic-curve crate to sign
//! test assertions would add a dependency to save nothing.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use base64::Engine as _;
use rsa::RsaPrivateKey;
use rsa::traits::PublicKeyParts as _;
use sha2::{Digest as _, Sha256};

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

/// The COSE identifier for RS256.
const COSE_RS256: i64 = -257;

/// The COSE key type for RSA.
const COSE_KTY_RSA: u64 = 3;

/// One key per process, because RSA generation would otherwise dominate the
/// suite, and because which key signs is not what any of these tests are about.
static DEVICE_KEY: LazyLock<RsaPrivateKey> = LazyLock::new(|| {
    RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
        .expect("generate a key for the authenticator under test")
});

/// A credential this authenticator holds.
#[derive(Clone)]
struct Stored {
    /// The account the relying party said it was for.
    user_handle: Vec<u8>,
    /// What the authenticator will report next time it signs.
    counter: u32,
}

/// An authenticator a test can plug in.
pub struct SoftAuthenticator {
    rp_id: String,
    origin: String,
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
            credentials: Mutex::new(HashMap::new()),
        }
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
        let client_data = client_data("webauthn.create", &challenge, &self.origin);

        let mut attested = Vec::new();
        attested.extend_from_slice(&[0u8; 16]); // No attestation, so no AAGUID.
        attested.extend_from_slice(&(credential_id.len() as u16).to_be_bytes());
        attested.extend_from_slice(&credential_id);
        attested.extend_from_slice(&cose_key());

        let auth_data = self.auth_data(
            FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS | FLAG_AT,
            0,
            &attested,
        );

        self.credentials
            .lock()
            .expect("the credential store")
            .insert(
                credential_id.clone(),
                Stored {
                    user_handle,
                    counter: 0,
                },
            );

        serde_json::json!({
            "id": B64.encode(&credential_id),
            "rawId": B64.encode(&credential_id),
            "type": "public-key",
            "response": {
                "attestationObject": B64.encode(attestation_object(&auth_data)),
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

        let (user_handle, counter) = {
            let mut held = self.credentials.lock().expect("the credential store");
            let stored = held
                .get_mut(&credential_id)
                .expect("a credential this authenticator holds");

            // A real authenticator increments on every assertion, and the
            // server treats a counter that does not move as a cloned device.
            stored.counter += 1;

            (stored.user_handle.clone(), stored.counter)
        };

        let client_data = client_data("webauthn.get", &challenge, &self.origin);
        let auth_data = self.auth_data(FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS, counter, &[]);

        let mut signed = auth_data.clone();
        signed.extend_from_slice(&Sha256::digest(&client_data));

        serde_json::json!({
            "id": B64.encode(&credential_id),
            "rawId": B64.encode(&credential_id),
            "type": "public-key",
            "response": {
                "authenticatorData": B64.encode(&auth_data),
                "clientDataJSON": B64.encode(&client_data),
                "signature": B64.encode(sign(&signed)),
                "userHandle": B64.encode(&user_handle),
            },
            "extensions": {},
        })
    }

    /// The same authenticator, answering as though the page were somewhere
    /// else.
    ///
    /// It holds the same credentials and signs with the same key; only the
    /// origin it writes into the client data differs. That is the phishing
    /// case in its purest form — everything about the assertion is genuine
    /// except where it was produced.
    pub fn at_origin(&self, origin: &str) -> Self {
        let held = self
            .credentials
            .lock()
            .expect("the credential store")
            .clone();

        Self {
            rp_id: self.rp_id.clone(),
            origin: origin.trim_end_matches('/').to_string(),
            credentials: Mutex::new(held),
        }
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

/// The client data a browser would produce.
fn client_data(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "type": kind,
        "challenge": B64.encode(challenge),
        "origin": origin,
        "crossOrigin": false,
    }))
    .expect("render the client data")
}

/// The attestation object, in the `none` format.
fn attestation_object(auth_data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();

    map_header(&mut out, 3);
    text(&mut out, "fmt");
    text(&mut out, "none");
    text(&mut out, "attStmt");
    map_header(&mut out, 0);
    text(&mut out, "authData");
    bytes(&mut out, auth_data);

    out
}

/// The device's public key, as COSE.
fn cose_key() -> Vec<u8> {
    let public = DEVICE_KEY.to_public_key();
    let mut out = Vec::new();

    map_header(&mut out, 4);
    uint(&mut out, 0, 1); // 1: key type
    uint(&mut out, 0, COSE_KTY_RSA);
    uint(&mut out, 0, 3); // 3: algorithm
    negative(&mut out, COSE_RS256);
    negative(&mut out, -1); // -1: modulus
    bytes(&mut out, &public.n().to_bytes_be());
    negative(&mut out, -2); // -2: exponent
    bytes(&mut out, &public.e().to_bytes_be());

    out
}

/// Signs with the device key, as `RS256`.
///
/// `jsonwebtoken` already holds the one RSASSA-PKCS1-v1_5 implementation this
/// workspace depends on; it hands back base64, which is decoded here rather
/// than reaching for a second signing crate.
fn sign(message: &[u8]) -> Vec<u8> {
    use rsa::pkcs1::EncodeRsaPrivateKey as _;

    let der = DEVICE_KEY
        .to_pkcs1_der()
        .expect("encode the authenticator's key");
    let key = jsonwebtoken::EncodingKey::from_rsa_der(der.as_bytes());

    let encoded = jsonwebtoken::crypto::sign(message, &key, jsonwebtoken::Algorithm::RS256)
        .expect("sign the assertion");

    B64.decode(encoded).expect("a base64url signature")
}

/// A fresh credential identifier.
fn random_id() -> Vec<u8> {
    use rand::Rng as _;

    let mut id = [0u8; 32];
    rand::rng().fill_bytes(&mut id);

    id.to_vec()
}

/// A CBOR head: the major type and an unsigned argument.
fn uint(out: &mut Vec<u8>, major: u8, value: u64) {
    let major = major << 5;

    match value {
        0..=23 => out.push(major | value as u8),
        24..=0xff => out.extend_from_slice(&[major | 24, value as u8]),
        0x100..=0xffff => {
            out.push(major | 25);
            out.extend_from_slice(&(value as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(major | 26);
            out.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            out.push(major | 27);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}

/// A CBOR negative integer.
fn negative(out: &mut Vec<u8>, value: i64) {
    uint(out, 1, (-1 - value) as u64);
}

/// A CBOR byte string.
fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    uint(out, 2, value.len() as u64);
    out.extend_from_slice(value);
}

/// A CBOR text string.
fn text(out: &mut Vec<u8>, value: &str) {
    uint(out, 3, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}

/// A CBOR map header.
fn map_header(out: &mut Vec<u8>, entries: u64) {
    uint(out, 5, entries);
}
