//! The RSA key pair every access token is signed with, and how one is stored.
//!
//! Split out of [`super`] because it is a different job: that module decides
//! what a token says and whether one is acceptable, this one owns the key
//! material and the sealed form it rests in. Nothing here is public beyond the
//! crate's own token path — a key that could be handed out is a key that
//! eventually is.
//!
//! The private half is sealed with [`SecretContext::JwtSigningKey`] before it
//! reaches the database, and the [`std::fmt::Debug`] implementation prints only
//! the identifier, so a `{:?}` on anything holding one cannot spill it.

use base64::Engine as _;
use jsonwebtoken::{DecodingKey, EncodingKey};
use rsa::pkcs1::EncodeRsaPrivateKey as _;
use rsa::pkcs8::{DecodePrivateKey as _, EncodePrivateKey as _, EncodePublicKey as _};
use rsa::traits::PublicKeyParts as _;
use rustak_core::prelude::*;
use sha2::{Digest as _, Sha256};

use crate::crypto::{SecretContext, SecretStore};
use crate::db::{
    Database,
    repos::{KeyAlgorithm, KeyPurpose, NewOauthKey},
};

use super::{ADVICE_REPORT, B64, KEY_BITS, KID_BYTES};

/// One RSA key pair, in every form the token path needs it.
pub(super) struct SigningKey {
    pub(super) kid: String,
    pub(super) encoding: EncodingKey,
    pub(super) decoding: DecodingKey,
    /// The public half, SPKI PEM, as `/oauth/token_key` publishes it.
    pub(super) public_pem: String,
    /// The `n` and `e` a JSON Web Key carries, base64url.
    modulus: String,
    exponent: String,
}

impl SigningKey {
    /// Rebuilds a key from the PKCS#8 encoding we sealed.
    pub(super) fn from_pkcs8(der: &[u8]) -> Result<Self, Error> {
        let private = rsa::RsaPrivateKey::from_pkcs8_der(der).or_system_err(&[
            "A stored token signing key could not be read; it may have been written by a different version of rustak.",
        ])?;
        let public = rsa::RsaPublicKey::from(&private);

        let spki = public.to_public_key_der().or_system_err(ADVICE_REPORT)?;
        let pkcs1 = private.to_pkcs1_der().or_system_err(ADVICE_REPORT)?;

        Ok(Self {
            kid: hex::encode(&Sha256::digest(spki.as_bytes())[..KID_BYTES]),
            encoding: EncodingKey::from_rsa_der(pkcs1.as_bytes()),
            decoding: DecodingKey::from_rsa_raw_components(
                &public.n().to_bytes_be(),
                &public.e().to_bytes_be(),
            ),
            public_pem: public
                .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
                .or_system_err(ADVICE_REPORT)?,
            modulus: B64.encode(public.n().to_bytes_be()),
            exponent: B64.encode(public.e().to_bytes_be()),
        })
    }

    /// Generates a key and returns it beside its PKCS#8 encoding, for sealing.
    pub(super) fn generate() -> Result<(Self, Vec<u8>), Error> {
        let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, KEY_BITS)
            .or_system_err(ADVICE_REPORT)?;
        let pkcs8 = private.to_pkcs8_der().or_system_err(ADVICE_REPORT)?;

        Ok((
            Self::from_pkcs8(pkcs8.as_bytes())?,
            pkcs8.as_bytes().to_vec(),
        ))
    }

    /// The public half as a JSON Web Key.
    pub(super) fn jwk(&self) -> serde_json::Value {
        serde_json::json!({
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": self.kid,
            "n": self.modulus,
            "e": self.exponent,
        })
    }
}

impl std::fmt::Debug for SigningKey {
    /// Written out so that a `{:?}` on anything holding a key cannot print the
    /// private half, which `EncodingKey` would otherwise be asked for.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SigningKey({})", self.kid)
    }
}

/// Unseals a stored key.
pub(super) fn open_key(
    secrets: &SecretStore,
    kid: &str,
    sealed: &str,
) -> Result<SigningKey, Error> {
    let sealed = serde_json::from_str(sealed).or_system_err(&[
        "A stored token signing key is corrupt; removing it will make rustak mint a new one.",
    ])?;

    SigningKey::from_pkcs8(&secrets.open(&sealed, SecretContext::JwtSigningKey { kid })?)
}

/// Generates a key, seals it and records it as the one to sign with.
pub(super) async fn create_key(db: &Database, secrets: &SecretStore) -> Result<SigningKey, Error> {
    // RSA generation is bignum arithmetic measured in hundreds of
    // milliseconds, which would otherwise stall this worker thread.
    let (key, pkcs8) = tokio::task::spawn_blocking(SigningKey::generate)
        .await
        .or_system_err(ADVICE_REPORT)??;

    store_key(db, secrets, key, &pkcs8).await
}

/// Seals a key that already exists and records it as the one to sign with.
pub(super) async fn store_key(
    db: &Database,
    secrets: &SecretStore,
    key: SigningKey,
    pkcs8: &[u8],
) -> Result<SigningKey, Error> {
    let sealed = secrets.seal(pkcs8, SecretContext::JwtSigningKey { kid: &key.kid })?;

    db.oauth_keys()
        .create(NewOauthKey {
            kid: key.kid.clone(),
            alg: KeyAlgorithm::Rs256,
            purpose: KeyPurpose::AccessToken,
            public_jwk: Some(key.jwk().to_string()),
            private_sealed: serde_json::to_string(&sealed).or_system_err(ADVICE_REPORT)?,
        })
        .await?;

    info!(kid = %key.kid, "Created a token signing key.");

    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared process-wide key, so no test here generates one.
    fn key() -> SigningKey {
        SigningKey::from_pkcs8(&crate::testing::keys::JWT_SIGNING_KEY)
            .expect("the shared test key is one we can sign with")
    }

    #[test]
    fn the_jwk_is_the_shape_rfc_7517_describes() {
        let jwk = key().jwk();

        assert_eq!(jwk["kty"], "RSA");
        assert_eq!(jwk["use"], "sig");
        assert_eq!(jwk["alg"], "RS256");
        assert!(jwk["kid"].as_str().is_some_and(|kid| !kid.is_empty()));
    }

    #[test]
    fn the_modulus_and_exponent_are_base64url_with_no_padding() {
        // RFC 7518 §6.3.1: base64url, and `=` padding is not part of it. A
        // padded value is one a relying-party library decodes to the wrong
        // bytes, and the signature then fails for a reason nobody can see.
        let jwk = key().jwk();

        for name in ["n", "e"] {
            let value = jwk[name].as_str().expect(name);

            assert!(!value.is_empty(), "{name}");
            assert!(!value.contains('='), "{name} is padded: {value}");
            assert!(
                !value.contains('+') && !value.contains('/'),
                "{name} is standard base64 rather than base64url: {value}",
            );
            assert!(
                B64.decode(value).is_ok(),
                "{name} does not decode as base64url: {value}",
            );
        }
    }

    #[test]
    fn the_exponent_is_the_one_the_key_actually_uses() {
        // Derived from the key rather than written down, so the published set
        // cannot drift from what the tokens are signed with.
        let jwk = key().jwk();

        assert_eq!(
            B64.decode(jwk["e"].as_str().unwrap()).unwrap(),
            vec![0x01, 0x00, 0x01],
            "65537, big-endian and minimally encoded",
        );
    }

    #[test]
    fn two_keys_are_named_differently_and_one_key_is_named_the_same_way_twice() {
        // The `kid` is a digest of the public half, which is what lets a
        // restart publish the same name for the same key.
        let (other, _) = SigningKey::generate().expect("a second key");

        assert_eq!(key().kid, key().kid);
        assert_ne!(key().kid, other.kid);
    }

    #[test]
    fn a_debug_dump_of_a_key_is_only_its_name() {
        // `EncodingKey` would otherwise be asked to render the private half.
        let key = key();

        assert_eq!(format!("{key:?}"), format!("SigningKey({})", key.kid));
    }
}
