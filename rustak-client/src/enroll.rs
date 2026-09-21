//! Getting a sidecar its certificate, the way an ATAK device gets one.
//!
//! A service is a client like any other, so it enrols like one: it generates a
//! key, sends a signing request to `POST /Marti/api/tls/signClient/v2` under HTTP
//! Basic with a one-time enrolment token, and keeps what comes back. There is no
//! sidecar-shaped shortcut, which is the point — anything a plugin can do, a
//! well-behaved client could do.
//!
//! ```no_run
//! # async fn example() -> Result<(), human_errors::Error> {
//! use rustak_client::enroll::{Enrolment, enroll};
//! use rustak_core::identity::Secret;
//!
//! let enrolled = enroll(&Enrolment {
//!     marti: "https://tak.example.com:8443",
//!     username: "svc.weather",
//!     secret: &Secret::new(std::env::var("RUSTAK_ENROLLMENT_TOKEN").unwrap()),
//!     client_uid: "SERVICE-weather",
//!     truststore: None,
//!     control_truststore: None,
//!     credential: Default::default(),
//!     trust: rustak_client::http::Trust::Internal,
//! })
//! .await?;
//!
//! enrolled.write_to("/etc/rustak", "weather")?;
//! # Ok(())
//! # }
//! ```
//!
//! # The private key never leaves this process
//!
//! It is generated here, written here, and the server never sees it: what
//! crosses the wire is a signing request carrying the *public* key. That is why
//! there is no "download my certificate" call to re-run — a lost key is a
//! re-enrolment, not a recovery.
//!
//! # The token is spent
//!
//! An enrolment token is one-time by default, and the server consumes it only
//! once the certificate has been issued and recorded. A failed enrolment
//! therefore leaves the token usable, and a successful one leaves it spent — so
//! a sidecar enrols when it has no certificate and not on every start.

use std::path::{Path, PathBuf};

use rustak_core::prelude::*;
use rustak_core::service::ServiceIdentity;

use crate::http::{self, Trust};

/// What the sign response carries, once it is JSON.
#[derive(Debug, Deserialize)]
struct Signed {
    /// The issued certificate, as bare base64 with no PEM armour.
    #[serde(rename = "signedCert")]
    signed_cert: String,

    /// `ca0`, `ca1`, … — one per link of the chain, zero-indexed.
    #[serde(flatten)]
    chain: std::collections::BTreeMap<String, String>,
}

/// What to enrol as, and where.
#[derive(Debug, Clone)]
pub struct Enrolment<'a> {
    /// The Marti API, e.g. `https://tak.example.com:8443`.
    pub marti: &'a str,

    /// The account to enrol. The certificate's common name is this, and the
    /// server refuses a signing request that claims any other.
    pub username: &'a str,

    /// The enrolment token or client password, presented over HTTP Basic.
    pub secret: &'a Secret,

    /// The device identifier this certificate is recorded against, which for a
    /// service is `SERVICE-<name>`.
    pub client_uid: &'a str,

    /// `[service] truststore` — the deployment's own CA, when it has one.
    ///
    /// [`None`] uses the platform's roots, which is right for a server behind a
    /// public certificate and wrong for one behind its own CA — an installation
    /// with a private CA has to distribute it before a sidecar can enrol, exactly
    /// as it does for a device.
    ///
    /// Whether it *replaces* the platform's roots or *joins* them is `trust`'s
    /// business, not this field's: see [`Trust`].
    pub truststore: Option<&'a Path>,

    /// `[service] control_truststore` — the public listener's own roots.
    ///
    /// Only consulted when `trust` is [`Trust::Public`], and then it replaces
    /// everything else. [`None`] is the ordinary case, including every
    /// enrolment against `[server] marti`.
    pub control_truststore: Option<&'a Path>,

    /// How `secret` is presented.
    ///
    /// An enrolment token or a client password goes in an HTTP Basic header,
    /// because that is the only thing ATAK and CloudTAK can send. An
    /// orchestrator's workload identity is a **bearer** credential and goes in
    /// `Authorization: Bearer` — the server accepts it either way, and sending
    /// a 900-byte JWT as a password is a shape nothing but a compatibility
    /// client should be writing.
    pub credential: Presentation,

    /// Which roots the *server* is verified against for this call.
    ///
    /// [`Trust::Internal`] for `[server] marti`, the mTLS listener, which
    /// always presents the deployment's own CA. [`Trust::Public`] for
    /// `[server] control`, the public listener, which serves
    /// `/Marti/api/tls/*` beside the control API and may hold an ACME or
    /// operator-supplied certificate. There is no default: see [`Trust`].
    pub trust: Trust,
}

/// Which header a credential travels in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Presentation {
    /// `Authorization: Basic <base64(username:secret)>` — an enrolment token
    /// or a client password.
    #[default]
    Basic,

    /// `Authorization: Bearer <assertion>` — an orchestrator's workload
    /// identity.
    Bearer,
}

/// A freshly issued identity: the certificate, its key, and the CA chain.
#[derive(Debug, Clone)]
pub struct Enrolled {
    /// The issued certificate, PEM.
    pub certificate_pem: String,

    /// Its private key, PKCS#8 PEM. Generated locally and never sent.
    pub key_pem: String,

    /// The chain the server sent, PEM, leaf-issuer first. This is the
    /// truststore a sidecar verifies the server with from now on.
    pub truststore_pem: String,
}

impl Enrolled {
    /// Writes the three files and answers the paths.
    ///
    /// They are named `<name>.pem`, `<name>.key` and `truststore.pem` inside
    /// `directory`, which is what `[service] certificate`, `key` and `truststore`
    /// then point at.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the directory cannot be written.
    pub fn write_to(&self, directory: impl AsRef<Path>, name: &str) -> Result<Paths, Error> {
        let directory = directory.as_ref();
        let paths = Paths {
            certificate: directory.join(format!("{name}.pem")),
            key: directory.join(format!("{name}.key")),
            truststore: directory.join("truststore.pem"),
        };

        self.write_files(&paths)?;

        Ok(paths)
    }

    /// Writes the three files exactly where `paths` names them.
    ///
    /// This is [`write_to`](Self::write_to) for a configuration that already
    /// says where its certificate, key and truststore live: the harness writes
    /// what `[service] certificate`, `key` and `truststore` point at rather than
    /// inventing names beside them.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when a file or its directory cannot
    /// be written.
    pub fn write_files(&self, paths: &Paths) -> Result<(), Error> {
        self.write_identity(&paths.certificate, &paths.key)?;
        self.write_truststore(&paths.truststore)
    }

    /// Writes the certificate and its key, and nothing else.
    ///
    /// The key is created with mode `0600` on Unix — it is the only copy of
    /// this sidecar's identity, and a world-readable one would make enrolment
    /// worse than the shared file it replaces. The certificate keeps the
    /// process umask: it is public material, and an init container that writes
    /// it for a sidecar running as another user still has to be readable.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when either file or its directory
    /// cannot be written.
    pub fn write_identity(&self, certificate: &Path, key: &Path) -> Result<(), Error> {
        write_pem(certificate, &self.certificate_pem, false)?;
        write_pem(key, &self.key_pem, true)
    }

    /// Writes the CA chain the server answered with, as the truststore.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the file or its directory
    /// cannot be written.
    pub fn write_truststore(&self, path: &Path) -> Result<(), Error> {
        write_pem(path, &self.truststore_pem, false)
    }
}

/// Where [`Enrolled::write_to`] put the three files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub certificate: PathBuf,
    pub key: PathBuf,
    pub truststore: PathBuf,
}

impl Paths {
    /// Attaches these files to an identity, as `[service]` would have.
    #[must_use]
    pub fn attach(&self, identity: ServiceIdentity) -> ServiceIdentity {
        identity
            .with_client_cert(&self.certificate, &self.key)
            .with_truststore(&self.truststore)
    }
}

/// Enrols, and answers the certificate, the key and the chain.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the URL is not one we can call, the
/// key cannot be generated, the server refuses the credential (which is what a
/// spent one-time token looks like), or the answer is not one we can read.
#[instrument("client.enroll", skip_all, fields(username = %request.username), err(Display))]
pub async fn enroll(request: &Enrolment<'_>) -> Result<Enrolled, Error> {
    let mut identity = ServiceIdentity::new(ServiceName::parse("enrolment").map_err(|err| {
        human_errors::user(err.to_string(), &["Please report this issue via GitHub."])
    })?);

    // Attached to the same two slots `[service]` fills, so that the policy in
    // `http::roots_for` is the one policy — an enrolment verified by a
    // different rule from the calls that follow it is the bug this all came
    // from, one step earlier.
    if let Some(truststore) = request.truststore {
        identity = identity.with_truststore(truststore);
    }

    if let Some(truststore) = request.control_truststore {
        identity = identity.with_control_truststore(truststore);
    }

    let client = http::client(&identity, request.trust, http::DEFAULT_TIMEOUT)?;
    let base = http::base_url(request.marti, "marti")?;
    let (csr, key) = signing_request(request.username)?;

    let signing = client
        .post(http::endpoint(&base, "/Marti/api/tls/signClient/v2")?)
        .query(&[("clientUid", request.client_uid), ("version", "3")]);

    let response = match request.credential {
        Presentation::Basic => signing.basic_auth(request.username, Some(request.secret.expose())),
        Presentation::Bearer => signing.bearer_auth(request.secret.expose()),
    }
    .header("accept", "application/json")
    .body(csr)
    .send()
    .await
    .map_err(|err| http::transport(err, "ask the server to sign a certificate"))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(refused(status));
    }

    let signed: Signed = serde_json::from_str(&body).map_err(|err| {
        human_errors::user(
            format!("The server's enrolment answer was not one we can read ({err})."),
            &[
                "Check that [server] marti names the TAK API, which is usually port 8443.",
                "A proxy that rewrites the response body will break enrolment.",
            ],
        )
    })?;

    Ok(Enrolled {
        certificate_pem: armour(&signed.signed_cert),
        key_pem: key,
        truststore_pem: chain(&signed),
    })
}

/// A fresh key and the signing request that carries its public half.
fn signing_request(username: &str) -> Result<(String, String), Error> {
    let key = rcgen::KeyPair::generate().map_err(|err| {
        human_errors::user(
            format!("Could not generate a private key ({err})."),
            &["This usually means the platform's random number source is unavailable."],
        )
    })?;

    let mut params = rcgen::CertificateParams::default();
    params.distinguished_name = rcgen::DistinguishedName::new();
    // The common name is the only part of the subject the server reads, and it
    // has to be the account being enrolled: everything else is replaced with the
    // installation's own organisation on the certificate that comes back.
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, username);

    let csr = params.serialize_request(&key).and_then(|csr| csr.pem());

    match csr {
        Ok(csr) => Ok((csr, key.serialize_pem())),
        Err(err) => Err(human_errors::user(
            format!("Could not build a signing request for '{username}' ({err})."),
            &["Check that [service] name is a name a certificate can carry."],
        )),
    }
}

/// The CA chain, in the order the server numbered it.
///
/// `ca0`, `ca1`, … are the keys; a `BTreeMap` orders them lexically, which is
/// the same order for any chain a certificate authority will ever have.
fn chain(signed: &Signed) -> String {
    signed
        .chain
        .iter()
        .filter(|(key, _)| key.starts_with("ca"))
        .map(|(_, value)| armour(value))
        .collect()
}

/// Rebuilds the PEM armour around the bare base64 the server sends.
///
/// Both ATAK and CloudTAK do this themselves, which is why the server sends it
/// bare; a client that forgot would hand rustls something it cannot parse.
fn armour(bare: &str) -> String {
    let body: String = bare.split_whitespace().collect::<Vec<_>>().join("\n");

    format!("-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n")
}

/// What each way of being refused means for whoever is holding the token.
fn refused(status: reqwest::StatusCode) -> Error {
    let advice: &[&str] = match status.as_u16() {
        401 => &[
            "An enrolment token is one-time: mint a new one if this one has been used.",
            "Check the username — it is the account's, not the service's display name.",
        ],
        403 => &["The signing request names a different account than the credential does."],
        429 => &["Too many attempts. Wait for the lockout to pass and try again."],
        503 => &["This installation has no certificate authority yet."],
        _ => &["The status above is the server's own."],
    };

    human_errors::user(
        format!("The server refused the enrolment ({status})."),
        advice,
    )
}

/// Writes one PEM file, creating the directory above it.
///
/// `private` asks for mode `0600` on Unix, which is applied both when the file
/// is created and afterwards — `create` leaves an existing file's mode alone,
/// and a re-enrolment over a key somebody once made readable must not inherit
/// that. Other platforms have no equivalent to set, so the file is written with
/// whatever they give it.
#[cfg_attr(not(unix), allow(unused_variables))]
fn write_pem(path: &Path, contents: &str, private: bool) -> Result<(), Error> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|err| cannot_write(parent, err))?;
    }

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }

    let mut file = options.open(path).map_err(|err| cannot_write(path, err))?;

    std::io::Write::write_all(&mut file, contents.as_bytes())
        .map_err(|err| cannot_write(path, err))?;

    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|err| cannot_write(path, err))?;
    }

    Ok(())
}

/// A file that could not be written, named.
fn cannot_write(path: &Path, err: std::io::Error) -> Error {
    human_errors::user(
        format!("Could not write '{}': {err}.", path.display()),
        &["Check that the directory exists and that this process may write to it."],
    )
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    /// The shape the server answers with: bare base64, no armour anywhere.
    fn signed_body() -> serde_json::Value {
        serde_json::json!({
            "signedCert": "TEVBRg==",
            "ca0": "SU5URVJNRURJQVRF",
            "ca1": "Uk9PVA==",
        })
    }

    async fn enrolled(server: &MockServer) -> Result<Enrolled, Error> {
        enroll(&Enrolment {
            marti: &server.uri(),
            username: "svc.weather",
            secret: &Secret::new("one-time-token"),
            client_uid: "SERVICE-weather",
            truststore: None,
            control_truststore: None,
            credential: Presentation::Basic,
            trust: Trust::Internal,
        })
        .await
    }

    #[tokio::test]
    async fn enrolling_sends_a_signing_request_and_keeps_the_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_matcher("/Marti/api/tls/signClient/v2"))
            .and(query_param("clientUid", "SERVICE-weather"))
            .and(query_param("version", "3"))
            .and(header("accept", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(signed_body()))
            .mount(&server)
            .await;

        let enrolled = enrolled(&server).await.unwrap();

        assert!(
            enrolled
                .certificate_pem
                .starts_with("-----BEGIN CERTIFICATE-----"),
            "{}",
            enrolled.certificate_pem
        );
        assert!(
            enrolled.key_pem.contains("PRIVATE KEY"),
            "the key is generated locally and is the only copy",
        );
        assert_eq!(
            enrolled.truststore_pem.matches("BEGIN CERTIFICATE").count(),
            2,
            "both chain links are kept, in the order the server numbered them",
        );
    }

    #[tokio::test]
    async fn a_spent_token_says_to_mint_another() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&server)
            .await;

        let err = enrolled(&server).await.unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains("one-time"), "{err}");
    }

    #[tokio::test]
    async fn the_three_files_land_where_the_configuration_expects_them() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(signed_body()))
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();

        let paths = enrolled(&server)
            .await
            .unwrap()
            .write_to(directory.path().join("pki"), "weather")
            .unwrap();

        assert!(paths.certificate.ends_with("weather.pem"));
        assert!(paths.key.ends_with("weather.key"));
        assert!(paths.truststore.ends_with("truststore.pem"));

        let identity = paths.attach(ServiceIdentity::new(ServiceName::parse("weather").unwrap()));
        assert!(identity.has_client_cert());
        assert_eq!(identity.truststore(), Some(paths.truststore.as_path()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_private_key_is_written_so_that_only_this_process_can_read_it() {
        // The key is the whole of a sidecar's identity and there is no second
        // copy anywhere: a mode that let the rest of the container read it would
        // make self-enrolment weaker than the shared file it replaces.
        use std::os::unix::fs::PermissionsExt as _;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(signed_body()))
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        // A key left behind by an earlier, more generous enrolment: writing over
        // it must tighten the mode rather than inherit it.
        let key = directory.path().join("pki").join("weather.key");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, "stale").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();

        let paths = enrolled(&server)
            .await
            .unwrap()
            .write_to(directory.path().join("pki"), "weather")
            .unwrap();

        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&paths.key), 0o600, "the key is ours alone");
        assert_eq!(
            mode(&paths.certificate) & 0o600,
            0o600,
            "the certificate is public material and is left to the umask",
        );
    }

    #[tokio::test]
    async fn the_files_can_be_written_where_the_configuration_names_them() {
        // What the harness does: `[service] certificate`/`key`/`truststore` are
        // paths an operator chose, not names to invent beside a directory.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(signed_body()))
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths {
            certificate: directory.path().join("nested/identity.crt"),
            key: directory.path().join("nested/identity.key"),
            truststore: directory.path().join("nested/ca-bundle.pem"),
        };

        enrolled(&server)
            .await
            .unwrap()
            .write_files(&paths)
            .unwrap();

        assert!(
            std::fs::read_to_string(&paths.certificate)
                .unwrap()
                .contains("BEGIN CERTIFICATE")
        );
        assert!(
            std::fs::read_to_string(&paths.key)
                .unwrap()
                .contains("PRIVATE KEY")
        );
        assert!(
            std::fs::read_to_string(&paths.truststore)
                .unwrap()
                .contains("BEGIN CERTIFICATE")
        );
    }

    #[test]
    fn a_signing_request_names_the_account_and_carries_no_private_key() {
        let (csr, key) = signing_request("svc.weather").unwrap();

        assert!(
            csr.starts_with("-----BEGIN CERTIFICATE REQUEST-----"),
            "{csr}"
        );
        assert!(
            !csr.contains("PRIVATE KEY"),
            "the request carries the public half and nothing else",
        );
        assert!(key.contains("PRIVATE KEY"));
    }
}
