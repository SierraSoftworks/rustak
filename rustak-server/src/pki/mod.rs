//! The internal certificate authority and the encodings it speaks.
//!
//! rustak issues the certificates its devices authenticate with, rather than
//! relying on an external PKI, because a TAK deployment's trust boundary *is*
//! the server: a device enrols against it, receives a certificate and a
//! truststore, and from then on the mutual-TLS handshake is the authentication.
//! There is nothing else to check the handshake against.
//!
//! # The shape of this module
//!
//! | File | What it answers |
//! |---|---|
//! | [`acme`] | a publicly trusted certificate for the browser-facing listener |
//! | [`keys`] | generating and reloading the private keys we own |
//! | [`ca`] | the root authority: creation, storage, reload, export |
//! | [`csr`] | reading a signing request in any form a client sends one |
//! | [`issue`] | signing a client certificate, with our subject |
//! | [`server_cert`] | the certificate our own listeners present |
//! | [`mod@revoke`] | taking one back, and the cache the handshake consults |
//! | [`serial`] | the one spelling of a serial number, shared by both ends |
//! | [`p12`] | PKCS#12 bundles in the shapes TAK clients read |
//! | [`pem`] | the textual encodings certificates travel in |
//! | [`tls`] | the rustls configurations the three listeners are built from |
//! | [`facade`] | [`Pki`], which ties enrolment and revocation together |
//!
//! [`acme`] stands apart from the rest: it is the one certificate this server
//! does not issue. Nothing else here depends on it, and it depends on the rest
//! only for [`keys`] and for [`tls`]'s resolver.
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

pub mod acme;
pub mod ca;
pub mod csr;
pub mod facade;
pub mod issue;
pub mod keys;
pub mod p12;
pub mod pem;
pub mod revoke;
pub mod serial;
pub mod server_cert;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tls;

pub use acme::{CertState, http01_routes};
pub use ca::{CaMaterial, ca_certificate_path, load_or_create_root_ca};
pub use csr::{CsrEncoding, CsrKey, CsrPolicy, ParsedCsr, parse_csr};
pub use facade::{Enrollment, IssuedVia, Pki, WORKLOAD_IDENTITY};
pub use issue::{IssueRequest, IssuedCert, issue_client_cert};
pub use keys::{KeyType, generate_key, key_pair_from_pkcs8, signature_algorithm};
pub use p12::{P12Options, client_keystore, legacy_signclient_v1, truststore};
pub use pem::{bare_base64_64col, parse_pem_chain, pem_certificate, sha256_fingerprint};
pub use revoke::{CertRejection, RevocationCache, RevokeReason, revoke, supersede_workload};
pub use serial::{SERIAL_BYTES, random_serial, serial_hex};
pub use server_cert::{ServerCertificate, load_or_issue as load_or_issue_server_cert};
pub use tls::{
    HotSwapCertResolver, ListenerKind, PeerCertificate, RustakClientVerifier, marti_server_config,
    stream_server_config,
};
