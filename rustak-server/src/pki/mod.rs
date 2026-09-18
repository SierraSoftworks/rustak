//! The internal certificate authority and the encodings it speaks.
//!
//! rustak issues the certificates its devices authenticate with, rather than
//! relying on an external PKI, because a TAK deployment's trust boundary *is*
//! the server: a device enrols against it, receives a certificate and a
//! truststore, and from then on the mutual-TLS handshake is the authentication.
//! There is nothing else to check the handshake against.
//!
//! # What is here in M0
//!
//! Only what the first-run wizard needs: generating a key ([`keys`]), creating
//! or reloading the root authority ([`ca`]), and the textual encodings
//! certificates travel in ([`pem`]). Signing requests, client and server
//! certificate issuance, revocation, PKCS#12 bundles, the rustls configuration
//! builders and ACME arrive with M2, on top of these.
//!
//! # We choose the subject, the client chooses the key
//!
//! A device generates its own key and sends a certificate signing request. We
//! take the public key out of it and discard everything else — the subject, the
//! subject alternative names, any extension it asked for — and issue
//! `CN=<username>` plus the attributes configured under `[pki]`. Honouring what
//! the request asked for would let anybody who can enrol mint a certificate
//! naming somebody else, and the common name is exactly what the stream and
//! Marti listeners resolve to a user.

pub mod ca;
pub mod keys;
pub mod pem;
pub mod server_cert;

pub use ca::{CaMaterial, ca_certificate_path, load_or_create_root_ca};
pub use keys::{KeyType, generate_key, key_pair_from_pkcs8, signature_algorithm};
pub use pem::{bare_base64_64col, parse_pem_chain, pem_certificate, sha256_fingerprint};
pub use server_cert::{ServerCertificate, load_or_issue as load_or_issue_server_cert};
