//! How a sidecar gets its certificate on the start where it has none.
//!
//! `docs/plugins.md` has always said that "a sidecar enrols when it has no
//! certificate rather than on every start". This is the part of the harness
//! that makes that true: before the CoT stream is opened, a sidecar whose
//! `[service] certificate` and `key` are missing — unset, or naming files that
//! are not there — presents a credential to [`crate::enroll`], writes the three
//! PEMs, and carries on start-up with them.
//!
//! # Which credential, in what order
//!
//! | | | |
//! |---|---|---|
//! | 1 | the PEMs in `pki_dir` | already enrolled; nothing is spent and no credential is read |
//! | 2 | an orchestrator's **workload identity** | a Nomad or Kubernetes JWT the task already holds — a deployment with one needs no rustak secret at all |
//! | 3 | a one-time **enrolment token** | the M9-05 path, for a deployment with no orchestrator identity |
//! | 4 | nothing | the error M9-05 already had, now naming both of the two ways out |
//!
//! Workload identity beats the enrolment token deliberately: a deployment that
//! has both is one being migrated, and the credential that does not have to be
//! minted, handed over and spent is the one to prefer.
//!
//! # The private key is generated here and stays here
//!
//! Nothing writes a key into a deployment: it is generated inside this process,
//! written with mode `0600`, and what crosses the wire is a signing request
//! carrying the public half. That is why there is no way to re-download a
//! certificate, and why a lost key is a re-enrolment rather than a recovery.
//!
//! # One start, one token
//!
//! An enrolment token is one-time and is spent by the signing call it pays for.
//! So the token is supplied for the *first* start —
//! [`ENROLLMENT_TOKEN_ENV`](super::ENROLLMENT_TOKEN_ENV) in the environment, or
//! `[service] enrollment_token` in the file — and a
//! token that is still set once the files exist is ignored with a log line
//! rather than spent again. A leftover environment variable therefore never
//! re-enrols a running deployment.
//!
//! # Failing is fatal
//!
//! Every failure here is a [`human_errors::Kind::User`] error that ends
//! start-up. A sidecar that carried on would either run with no identity at all
//! or fall back to something it was not configured for, and an operator would
//! find out about it from an empty map rather than from a start-up message.

use std::path::{Path, PathBuf};

use rustak_core::prelude::*;

use super::config::{ServerConfig, ServiceConfig, SidecarConfig};
use super::workload::{self, Source};
use crate::enroll::{Enrolment, Paths, Presentation, enroll};
use crate::http::Trust;

/// Which credential a start is going to enrol with.
enum Credential {
    /// The identity this sidecar's orchestrator already gave it.
    Workload { source: Source, assertion: Secret },
    /// A one-time enrolment token, from the file or the environment.
    Token(Secret),
}

impl Credential {
    /// The words the start-up line and `--check` both use for it.
    fn describe(&self) -> String {
        match self {
            Self::Workload { source, .. } => format!("the workload identity from {source}"),
            Self::Token(_) => "an enrolment token".to_string(),
        }
    }

    /// The secret itself, and the header it travels in.
    fn present(&self) -> (&Secret, Presentation) {
        match self {
            Self::Workload { assertion, .. } => (assertion, Presentation::Bearer),
            Self::Token(token) => (token, Presentation::Basic),
        }
    }

    /// Who signed it, when that is something this end can read.
    fn issuer(&self) -> Option<String> {
        match self {
            Self::Workload { assertion, .. } => workload::unverified_issuer(assertion),
            Self::Token(_) => None,
        }
    }
}

/// The credential this start will enrol with, in the documented order.
///
/// Reads the workload identity from its source — which is the point: it is
/// re-read on every use, because both orchestrators rotate it.
fn credential(service: &ServiceConfig) -> Result<Option<Credential>, Error> {
    if let Some(source) = service.workload_source()? {
        // A source that was named in the file and holds nothing is a refusal
        // rather than a fall-through: naming one is a deliberate act.
        let assertion = source.read()?;

        return Ok(Some(Credential::Workload { source, assertion }));
    }

    Ok(service.enrollment_token()?.map(Credential::Token))
}

/// Advice for an enrolment that was asked for with nothing to pay for it.
const ADVICE_NO_TOKEN: &[&str] = &[
    "Under Nomad or Kubernetes, give the task a workload identity — see \"Workload identity\" in docs/deployment.md — and this needs no secret at all.",
    "Otherwise mint a one-time enrolment token for this service's account and pass it as RUSTAK_ENROLLMENT_TOKEN.",
    "Or write it into the file as [service] enrollment_token = \"${{ env.RUSTAK_ENROLLMENT_TOKEN }}\".",
    "A token is spent by the enrolment it pays for, so this is needed once rather than on every start.",
];

/// Advice for an enrolment that has nowhere to enrol at.
const ADVICE_NO_SERVER: &[&str] = &[
    "Set [server] control to the public listener, which is usually port 8446.",
    "Set [server] marti instead if enrolment is served somewhere else.",
];

/// Advice for a certificate that came back in a shape we could not read.
const ADVICE_UNREADABLE: &[&str] = &[
    "The certificate was still written; this only affects what the start-up line can say about it.",
    "Please report this issue via GitHub.",
];

/// Validates the identity half of a configuration without touching anything.
///
/// This is `--check` for a deployment that has not enrolled yet, which is the
/// ordinary state of a container image before its first run: the certificate,
/// key and truststore it names are files that do not exist, and a validation
/// that tried to read them would refuse the very file a pipeline is trying to
/// test. So the paths that are not there yet are taken off the configuration —
/// after saying where they will be written — and what is left is validated as
/// usual.
///
/// Nothing is written and **no request is made**, including none to read a
/// workload identity's issuer: which source a start would use is answered from
/// the environment and the filesystem alone. What *is* checked is that an
/// `${{ env.… }}` enrolment token resolves and that `workload_identity` names
/// one source rather than two — an expression whose variable was never set is
/// exactly the kind of thing `--check` exists to catch before a deployment
/// goes out.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the enrolment token still holds an
/// unresolved `${{ env.NAME }}` expression, or when `[service]
/// workload_identity` names both an `env` and a `file`.
pub(crate) fn check<S>(config: &mut SidecarConfig<S>, config_path: &Path) -> Result<(), Error> {
    let paths = resolve(&config.service, config_path);

    if paths.certificate.exists() && paths.key.exists() {
        return Ok(());
    }

    // The source is worked out without reading the token: `--check` says which
    // credential a start would use, and reading one would be work a validation
    // has no business doing.
    let source = config.service.workload_source()?;

    // An unresolved expression is refused by name here, exactly as a start
    // would refuse it — which is the point of validating a candidate file with
    // the binary that will read it. Skipped when a workload identity is going
    // to be used instead, because then the token is a leftover.
    let token = match &source {
        Some(_) => None,
        None => config.service.enrollment_token()?,
    };

    if source.is_none() && token.is_none() && config.service.pki_dir.is_none() {
        // Nothing says this sidecar means to enrol, so its missing certificate
        // is a missing certificate and start-up says so in its own words.
        return Ok(());
    }

    let with = match &source {
        Some(source) => format!(" with the workload identity from {source}"),
        None => String::new(),
    };

    tracing::info!(
        certificate = %paths.certificate.display(),
        key = %paths.key.display(),
        truststore = %paths.truststore.display(),
        "The configuration is valid; this identity will be enrolled into '{}'{with} on the first start.",
        directory(&paths.certificate).display(),
    );

    if !paths.certificate.exists() || !paths.key.exists() {
        config.service.certificate = None;
        config.service.key = None;
    }

    if !paths.truststore.exists() {
        config.service.truststore = None;
    }

    Ok(())
}

/// Makes sure this sidecar has a certificate, enrolling for one if it has not.
///
/// The configuration is updated in place with the paths that were used, so that
/// the identity the rest of start-up builds is the enrolled one. Answers where
/// the three files are, or [`None`] for a sidecar that has no certificate, was
/// given no credential, and did not ask for one.
///
/// `required` is `--enroll`: it turns "there is nothing to do here" into a
/// failure, because a one-off enrolment task that quietly enrolled nothing is
/// an init container that reports success and leaves the deployment broken.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the credential is unresolved,
/// missing or refused, when there is no endpoint to enrol against, when the
/// server cannot be reached, or when the files cannot be written.
pub(crate) async fn ensure<S>(
    config: &mut SidecarConfig<S>,
    config_path: &Path,
    required: bool,
) -> Result<Option<Paths>, Error> {
    let paths = resolve(&config.service, config_path);

    if paths.certificate.exists() && paths.key.exists() {
        if config.service.has_enrollment_token() {
            tracing::info!(
                certificate = %paths.certificate.display(),
                "This sidecar already has a certificate, so its enrolment token was not used.",
            );
        }

        already_enrolled(&paths);
        attach(config, &paths);

        return Ok(Some(paths));
    }

    let Some(credential) = credential(&config.service)? else {
        if required {
            return Err(human_errors::user(
                format!(
                    "This sidecar has no certificate at '{}' and nothing to get one with.",
                    paths.certificate.display()
                ),
                ADVICE_NO_TOKEN,
            ));
        }

        return Ok(None);
    };

    // Refuses an endpoint that is still an unsubstituted `${{ env.… }}`
    // expression before one is dialled, by name, exactly as start-up would a
    // moment later.
    config.server.endpoints()?;

    let (server, trust) = target(&config.server)?;
    let account = config.service.account().to_string();
    let uid = config.service.name.uid();
    // An explicitly configured truststore that is already there is what the
    // *enrolment call itself* verifies the server with; without one, the
    // platform's roots are, which is right for the public listener behind a
    // publicly issued certificate.
    let verify_with = config
        .service
        .truststore
        .as_deref()
        .filter(|path| path.exists());
    // Not filtered on existence: an operator who pinned the public listener and
    // named a file that is not there must be told so, rather than quietly given
    // the platform's roots — which is the whole class of bug this setting
    // exists to close.
    let pin_control_with = match trust {
        Trust::Public => config.service.control_truststore.as_deref(),
        Trust::Internal => None,
    };

    let (secret, presentation) = credential.present();

    tracing::info!(
        account = %account,
        uid = %uid,
        %server,
        credential = %credential.describe(),
        "This sidecar has no certificate; enrolling for one.",
    );

    let enrolled = enroll(&Enrolment {
        marti: &server,
        username: &account,
        secret,
        client_uid: uid.as_str(),
        truststore: verify_with,
        control_truststore: pin_control_with,
        credential: presentation,
        trust,
    })
    .await
    .map_err(|err| {
        human_errors::wrap_user(
            err,
            format!("Could not enrol '{account}' at '{server}'."),
            &["The sidecar does not start without an identity, so this is fatal."],
        )
    })?;

    enrolled.write_identity(&paths.certificate, &paths.key)?;

    // The chain the server answered with is what this sidecar trusts from now
    // on — rustak's `internal` mode signs with its own CA, which no platform
    // root store has heard of. A truststore the operator named *and supplied*
    // is theirs, and is left alone.
    if verify_with.is_some() {
        tracing::info!(
            truststore = %paths.truststore.display(),
            "Keeping the configured truststore rather than the chain the server sent.",
        );
    } else {
        enrolled.write_truststore(&paths.truststore)?;
    }

    describe(&enrolled.certificate_pem, &paths);
    workload::report_identity(
        &credential.describe(),
        credential.issuer().as_deref(),
        &common_name(&enrolled.certificate_pem).unwrap_or(account),
    );
    attach(config, &paths);

    Ok(Some(paths))
}

/// The identity line for a start that had its certificate already.
///
/// The same line as every other start, because "which of the four credentials
/// did this process use" is the question, and "it already had one" is one of
/// the four answers.
fn already_enrolled(paths: &Paths) {
    let pem = std::fs::read_to_string(&paths.certificate).unwrap_or_default();

    workload::report_identity(
        &format!("the certificate at '{}'", paths.certificate.display()),
        None,
        &common_name(&pem).unwrap_or_else(|| "an unreadable certificate".to_string()),
    );
}

/// The common name of a PEM certificate, which is the account rustak issued it
/// to.
fn common_name(certificate_pem: &str) -> Option<String> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(certificate_pem.as_bytes()).ok()?;
    let certificate = pem.parse_x509().ok()?;
    let name = certificate
        .subject()
        .iter_common_name()
        .next()?
        .as_str()
        .ok()?
        .to_string();

    Some(name)
}

/// Where the three files belong, given what the file says and where it is.
///
/// `[service] certificate`, `key` and `truststore` name their own paths when
/// they are set. What they do not name goes in `[service] pki_dir`, and what
/// that does not name goes beside the configuration file — which is the
/// directory a container image already mounts.
fn resolve(service: &ServiceConfig, config_path: &Path) -> Paths {
    let directory = service
        .pki_dir
        .clone()
        .unwrap_or_else(|| beside(config_path));
    let name = service.name.as_str();

    Paths {
        certificate: service
            .certificate
            .clone()
            .unwrap_or_else(|| directory.join(format!("{name}.pem"))),
        key: service
            .key
            .clone()
            .unwrap_or_else(|| directory.join(format!("{name}.key"))),
        truststore: service
            .truststore
            .clone()
            .unwrap_or_else(|| directory.join("truststore.pem")),
    }
}

/// The directory a file will be written into, for the line that names it.
fn directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// The directory a configuration file is in, as a path that can be joined onto.
fn beside(config_path: &Path) -> PathBuf {
    match config_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        // `--config plugin.toml`, which is the harness's own default.
        _ => PathBuf::from("."),
    }
}

/// Points the configuration at the files, so start-up carries on with them.
///
/// The truststore is attached only when there is one: a sidecar enrolled
/// against a publicly issued certificate that was told to keep the platform's
/// roots must keep them.
fn attach<S>(config: &mut SidecarConfig<S>, paths: &Paths) {
    config.service.certificate = Some(paths.certificate.clone());
    config.service.key = Some(paths.key.clone());

    if config.service.truststore.is_none() && paths.truststore.exists() {
        config.service.truststore = Some(paths.truststore.clone());
    }
}

/// The endpoint the enrolment surface is served on, and how it is verified.
///
/// `[server] marti` when it is set, because an installation that names it has
/// said where its Marti API is; `[server] control` otherwise, because the
/// public listener serves `/Marti/api/tls/*` beside the control API and that is
/// the one every sidecar is configured with.
///
/// The policy follows the endpoint rather than the call: the mTLS listener is
/// always the deployment's own CA ([`Trust::Internal`]), and the public one may
/// be anything ([`Trust::Public`]). Enrolment is the *first* call a deployment
/// makes, so getting this wrong is a deployment that never starts.
fn target(server: &ServerConfig) -> Result<(String, Trust), Error> {
    if let Some(marti) = server.marti.clone() {
        return Ok((marti, Trust::Internal));
    }

    server
        .control
        .clone()
        .map(|control| (control, Trust::Public))
        .ok_or_else(|| {
            human_errors::user(
                "This sidecar has no certificate and no [server] endpoint to enrol against.",
                ADVICE_NO_SERVER,
            )
        })
}

/// Logs what was issued and where it landed.
///
/// The subject and the expiry are the two things an operator needs from a
/// start-up line: that the certificate names the account they expected, and
/// when they will have to think about it again.
fn describe(certificate_pem: &str, paths: &Paths) {
    match summarise(certificate_pem) {
        Ok((subject, expires)) => tracing::info!(
            %subject,
            %expires,
            certificate = %paths.certificate.display(),
            key = %paths.key.display(),
            truststore = %paths.truststore.display(),
            "Enrolled. The private key was generated here and has not left this process.",
        ),
        // The certificate is written either way: an issuer whose encoding we
        // cannot read is still an identity the TLS stack may well accept, and
        // refusing to start over a log line would be the worse failure.
        Err(err) => tracing::warn!(
            error = %err,
            certificate = %paths.certificate.display(),
            "Enrolled, but the certificate could not be read back to describe it.",
        ),
    }
}

/// The certificate's subject and expiry, for the line above.
fn summarise(certificate_pem: &str) -> Result<(String, String), Error> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(certificate_pem.as_bytes())
        .or_system_err(ADVICE_UNREADABLE)?;
    let certificate = pem.parse_x509().or_system_err(ADVICE_UNREADABLE)?;
    let not_after = certificate.validity().not_after;
    let expires = chrono::DateTime::from_timestamp(not_after.timestamp(), 0)
        .map_or_else(|| not_after.to_string(), |at| at.to_rfc3339());

    Ok((certificate.subject().to_string(), expires))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar::NoSettings;

    /// The configuration a test starts from, plus the directory it lives in.
    fn config(body: &str) -> SidecarConfig<NoSettings> {
        rustak_core::config::load_str(body).expect("the test configuration should load")
    }

    #[test]
    fn unnamed_files_land_beside_the_configuration_file() {
        let paths = resolve(
            &config("[service]\nname = \"example\"\n").service,
            Path::new("/data/plugin.toml"),
        );

        assert_eq!(paths.certificate, PathBuf::from("/data/example.pem"));
        assert_eq!(paths.key, PathBuf::from("/data/example.key"));
        assert_eq!(paths.truststore, PathBuf::from("/data/truststore.pem"));
    }

    #[test]
    fn a_pki_dir_takes_the_files_the_service_section_does_not_name() {
        let paths = resolve(
            &config("[service]\nname = \"example\"\npki_dir = \"/var/lib/rustak\"\n").service,
            Path::new("/data/plugin.toml"),
        );

        assert_eq!(
            paths.certificate,
            PathBuf::from("/var/lib/rustak/example.pem")
        );
        assert_eq!(
            paths.truststore,
            PathBuf::from("/var/lib/rustak/truststore.pem")
        );
    }

    #[test]
    fn configured_paths_are_written_where_the_operator_put_them() {
        // The deployment case: the file names all three, and enrolment fills
        // them in rather than inventing names of its own beside them.
        let paths = resolve(
            &config(
                "[service]\nname = \"example\"\ncertificate = \"/etc/rustak/a.pem\"\nkey = \"/etc/rustak/a.key\"\ntruststore = \"/etc/rustak/ca.pem\"\n",
            )
            .service,
            Path::new("/data/plugin.toml"),
        );

        assert_eq!(paths.certificate, PathBuf::from("/etc/rustak/a.pem"));
        assert_eq!(paths.key, PathBuf::from("/etc/rustak/a.key"));
        assert_eq!(paths.truststore, PathBuf::from("/etc/rustak/ca.pem"));
    }

    #[test]
    fn a_relative_configuration_file_keeps_its_files_in_the_working_directory() {
        let paths = resolve(
            &config("[service]\nname = \"example\"\n").service,
            Path::new("config.toml"),
        );

        assert_eq!(paths.certificate, PathBuf::from("./example.pem"));
    }

    #[test]
    fn marti_takes_precedence_over_control_as_the_enrolment_surface() {
        // And each endpoint brings its own trust policy: the mTLS listener is
        // always the deployment's own CA, the public one may be anything.
        let both = config(
            "[service]\nname = \"example\"\n\n[server]\nmarti = \"https://tak:8443\"\ncontrol = \"https://tak:8446\"\n",
        );
        assert_eq!(
            target(&both.server).unwrap(),
            ("https://tak:8443".to_string(), Trust::Internal),
        );

        let control =
            config("[service]\nname = \"example\"\n\n[server]\ncontrol = \"https://tak:8446\"\n");
        assert_eq!(
            target(&control.server).unwrap(),
            ("https://tak:8446".to_string(), Trust::Public),
        );

        let neither = config("[service]\nname = \"example\"\n");
        let err = target(&neither.server).unwrap_err();
        assert!(err.is(human_errors::Kind::User), "{err}");
    }

    #[test]
    fn the_account_defaults_to_the_service_name_and_is_overridden_by_name() {
        assert_eq!(
            config("[service]\nname = \"example\"\n").service.account(),
            "example"
        );
        assert_eq!(
            config("[service]\nname = \"example\"\naccount = \"svc.example\"\n")
                .service
                .account(),
            "svc.example",
        );
    }

    #[tokio::test]
    async fn an_existing_certificate_is_used_and_the_token_is_left_alone() {
        // The second start: the files are there, so nothing is spent and the
        // configuration comes out pointing at them — including the truststore
        // the first start wrote, which the file never named.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("plugin.toml");
        for (name, contents) in [
            ("example.pem", "certificate"),
            ("example.key", "key"),
            ("truststore.pem", "chain"),
        ] {
            std::fs::write(directory.path().join(name), contents).unwrap();
        }
        let mut config = config("[service]\nname = \"example\"\n");

        let paths = ensure(&mut config, &path, true).await.unwrap().unwrap();

        assert_eq!(
            config.service.certificate.as_deref(),
            Some(paths.certificate.as_path())
        );
        assert_eq!(config.service.key.as_deref(), Some(paths.key.as_path()));
        assert_eq!(
            config.service.truststore.as_deref(),
            Some(paths.truststore.as_path())
        );
    }

    #[tokio::test]
    async fn a_start_with_no_certificate_and_no_token_is_left_to_the_ordinary_start_up() {
        // Not this module's failure to report: the identity or the stream will
        // name the file that is missing, in the words it already uses for one.
        let directory = tempfile::tempdir().unwrap();
        let mut config = config("[service]\nname = \"example\"\n");

        let outcome = ensure(&mut config, &directory.path().join("plugin.toml"), false)
            .await
            .unwrap();

        assert!(outcome.is_none());
        assert!(config.service.certificate.is_none());
    }

    #[tokio::test]
    async fn enrolling_on_purpose_without_a_token_says_so() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = config("[service]\nname = \"example\"\n");

        let err = ensure(&mut config, &directory.path().join("plugin.toml"), true)
            .await
            .unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains("enrolment token"), "{err}");
    }

    #[tokio::test]
    async fn a_token_with_nowhere_to_enrol_is_refused_before_it_is_sent() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = config(&format!(
            "[service]\nname = \"example\"\nenrollment_token = \"one-time\"\npki_dir = \"{}\"\n",
            directory.path().display(),
        ));

        let err = ensure(&mut config, &directory.path().join("plugin.toml"), false)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("[server]"), "{err}");
    }

    #[tokio::test]
    async fn an_unresolved_token_is_refused_by_the_name_of_its_variable() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = config(
            "[service]\nname = \"example\"\nenrollment_token = \"${{ env.RUSTAK_ENROLMENT_NOT_SET }}\"\n",
        );

        let err = ensure(&mut config, &directory.path().join("plugin.toml"), false)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("RUSTAK_ENROLMENT_NOT_SET"),
            "{err}"
        );
        assert!(
            err.to_string().contains("service.enrollment_token"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_sidecar_enrols_writes_the_three_files_and_starts_up_with_them() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/Marti/api/tls/signClient/v2"))
            .and(wiremock::matchers::query_param(
                "clientUid",
                "SERVICE-example",
            ))
            // The signing request goes out as the *account*, not the service.
            .and(wiremock::matchers::basic_auth("svc.example", "one-time"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({ "signedCert": "TEVBRg==", "ca0": "Uk9PVA==" }),
                ),
            )
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("plugin.toml");
        let mut config = config(&format!(
            "[service]\nname = \"example\"\naccount = \"svc.example\"\nenrollment_token = \"one-time\"\n\n[server]\ncontrol = \"{}\"\n",
            server.uri(),
        ));

        let paths = ensure(&mut config, &path, false).await.unwrap().unwrap();

        assert!(paths.certificate.exists() && paths.key.exists() && paths.truststore.exists());
        assert!(
            std::fs::read_to_string(&paths.key)
                .unwrap()
                .contains("PRIVATE KEY")
        );
        // Start-up carries on with what was just written.
        assert!(config.identity().unwrap().has_client_cert());
        assert_eq!(
            config.service.truststore.as_deref(),
            Some(paths.truststore.as_path())
        );
        // The `basic_auth` matcher above is what asserts the account; a request
        // that reached the mock at all is a request that carried it.
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_pki_dir_holds_the_three_files_and_is_read_back_without_a_token() {
        // The deployment shape: `pki_dir = "/data"` and nothing else about the
        // identity. The first start writes all three files there; the next one
        // finds them, uses them, and makes no request at all.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({ "signedCert": "TEVBRg==", "ca0": "Uk9PVA==" }),
                ),
            )
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let pki = directory.path().join("data");
        let file = |token: &str| {
            format!(
                "[service]\nname = \"example\"\npki_dir = \"{}\"\n{token}\n\n[server]\ncontrol = \"{}\"\n",
                pki.display(),
                server.uri(),
            )
        };

        let mut first = config(&file("enrollment_token = \"one-time\""));
        let paths = ensure(&mut first, &directory.path().join("plugin.toml"), false)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(paths.certificate, pki.join("example.pem"));
        assert_eq!(paths.key, pki.join("example.key"));
        assert_eq!(paths.truststore, pki.join("truststore.pem"));

        // The next start, with the token gone from both the file and the
        // environment: the files are found and nothing is spent.
        let mut second = config(&file(""));
        let found = ensure(&mut second, &directory.path().join("plugin.toml"), false)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(found, paths);
        assert_eq!(
            second.service.certificate.as_deref(),
            Some(paths.certificate.as_path())
        );
        assert_eq!(second.service.key.as_deref(), Some(paths.key.as_path()));
        assert_eq!(
            second.service.truststore.as_deref(),
            Some(paths.truststore.as_path())
        );
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "the second start enrolled nothing",
        );
    }

    #[test]
    fn check_takes_the_files_that_are_not_there_yet_off_the_configuration() {
        // What lets `--check` validate a pre-enrolment file: the paths that
        // enrolment is going to write are not paths to read now.
        let directory = tempfile::tempdir().unwrap();
        let mut config = config(&format!(
            "[service]\nname = \"example\"\npki_dir = \"{}\"\ncertificate = \"/data/example.pem\"\nkey = \"/data/example.key\"\ntruststore = \"/data/truststore.pem\"\n",
            directory.path().display(),
        ));

        check(&mut config, &directory.path().join("plugin.toml")).unwrap();

        assert!(config.service.certificate.is_none());
        assert!(config.service.key.is_none());
        assert!(config.service.truststore.is_none());
        assert!(
            !directory.path().join("example.pem").exists(),
            "nothing was written"
        );
    }

    #[test]
    fn check_leaves_a_sidecar_that_never_meant_to_enrol_alone() {
        // No token, no pki_dir: the missing certificate is a missing
        // certificate, and start-up reports it in the words it already has.
        let directory = tempfile::tempdir().unwrap();
        let mut config = config(
            "[service]\nname = \"example\"\ncertificate = \"/etc/rustak/a.pem\"\nkey = \"/etc/rustak/a.key\"\n",
        );

        check(&mut config, &directory.path().join("plugin.toml")).unwrap();

        assert!(config.service.certificate.is_some());
    }

    #[tokio::test]
    async fn a_refused_token_stops_start_up_with_the_reason() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let mut config = config(&format!(
            "[service]\nname = \"example\"\nenrollment_token = \"spent\"\n\n[server]\ncontrol = \"{}\"\n",
            server.uri(),
        ));

        let err = ensure(&mut config, &directory.path().join("plugin.toml"), false)
            .await
            .unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains("Could not enrol"), "{err}");
        // The cause, and the advice that goes with it, survive the wrapping.
        assert!(err.to_string().contains("one-time"), "{err}");
        assert!(!directory.path().join("example.key").exists());
    }

    /// A server that signs anything, answering the shape `signClient/v2` does.
    async fn signing_server() -> wiremock::MockServer {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/Marti/api/tls/signClient/v2"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({ "signedCert": "TEVBRg==", "ca0": "Uk9PVA==" }),
                ),
            )
            .mount(&server)
            .await;

        server
    }

    /// A token file, and the configuration that points at it.
    fn workload_deployment(
        directory: &Path,
        server: &str,
        extra: &str,
    ) -> (PathBuf, SidecarConfig<NoSettings>) {
        let token = directory.join("nomad_rustak.jwt");
        std::fs::write(&token, "header.payload.signature").unwrap();

        let config = config(&format!(
            "[service]\nname = \"example\"\naccount = \"svc.example\"\nworkload_identity = {{ file = \"{}\" }}\n{extra}\n\n[server]\ncontrol = \"{server}\"\n",
            token.display(),
        ));

        (token, config)
    }

    #[tokio::test]
    async fn a_workload_identity_enrols_as_a_bearer_credential_rather_than_a_password() {
        // A 900-byte JWT in a password field is a shape only a compatibility
        // client should be writing; the sidecar sends the header the credential
        // actually is.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/Marti/api/tls/signClient/v2"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer header.payload.signature",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({ "signedCert": "TEVBRg==", "ca0": "Uk9PVA==" }),
                ),
            )
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let (_, mut config) = workload_deployment(directory.path(), &server.uri(), "");

        let paths = ensure(&mut config, &directory.path().join("plugin.toml"), false)
            .await
            .unwrap()
            .unwrap();

        assert!(paths.certificate.exists() && paths.key.exists());
        // The `header` matcher above is what asserts it; a request that reached
        // the mock at all is a request that carried a Bearer assertion.
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_workload_identity_is_preferred_over_a_leftover_enrolment_token() {
        // A deployment being migrated has both. The credential that does not
        // have to be minted, handed over and spent is the one to use.
        let server = signing_server().await;
        let directory = tempfile::tempdir().unwrap();
        let (_, mut config) = workload_deployment(
            directory.path(),
            &server.uri(),
            "enrollment_token = \"a-token-nobody-should-spend\"",
        );

        ensure(&mut config, &directory.path().join("plugin.toml"), false)
            .await
            .unwrap()
            .unwrap();

        let sent = server.received_requests().await.unwrap();
        let header = sent[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();

        assert!(
            header.starts_with("Bearer "),
            "the workload identity is what was spent: {header}",
        );
        assert!(
            !header.starts_with("Basic "),
            "the enrolment token was left alone: {header}",
        );
    }

    #[tokio::test]
    async fn the_token_is_re_read_from_its_file_on_every_start() {
        // Both orchestrators rotate it. A sidecar that cached the one it saw at
        // start-up would enrol with a token that expired an hour ago.
        let server = signing_server().await;
        let directory = tempfile::tempdir().unwrap();
        let (token, mut config) = workload_deployment(directory.path(), &server.uri(), "");

        std::fs::write(&token, "rotated.payload.signature").unwrap();

        ensure(&mut config, &directory.path().join("plugin.toml"), false)
            .await
            .unwrap()
            .unwrap();

        let sent = server.received_requests().await.unwrap();

        assert_eq!(
            sent[0]
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer rotated.payload.signature"),
        );
    }

    #[tokio::test]
    async fn a_workload_identity_that_is_not_there_is_refused_rather_than_fallen_back_from() {
        // Naming a source is a deliberate act. Quietly falling back to an
        // enrolment token would hide the one thing the operator got wrong.
        let directory = tempfile::tempdir().unwrap();
        let mut config = config(&format!(
            "[service]\nname = \"example\"\nworkload_identity = {{ file = \"{}\" }}\nenrollment_token = \"a-token\"\n",
            directory.path().join("never-mounted.jwt").display(),
        ));

        let err = ensure(&mut config, &directory.path().join("plugin.toml"), false)
            .await
            .unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains("never-mounted.jwt"), "{err}");
    }

    #[test]
    fn check_says_which_credential_a_first_start_would_use() {
        // The line a deployment pipeline reads. It must name the source without
        // reading the token and without touching the network.
        let directory = tempfile::tempdir().unwrap();
        let token = directory.path().join("nomad_rustak.jwt");
        std::fs::write(&token, "a.b.c").unwrap();

        let mut config = config(&format!(
            "[service]\nname = \"example\"\npki_dir = \"/data\"\nworkload_identity = {{ file = \"{}\" }}\n",
            token.display(),
        ));

        check(&mut config, &directory.path().join("plugin.toml"))
            .expect("a file that will enrol with a workload identity is a valid file");

        assert!(config.service.certificate.is_none());
        assert!(config.service.key.is_none());
    }

    #[test]
    fn check_refuses_a_workload_identity_that_names_two_sources() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = config(
            "[service]\nname = \"example\"\nworkload_identity = { env = \"NOMAD_TOKEN_rustak\", file = \"/var/run/secrets/tokens/rustak\" }\n",
        );

        let err = check(&mut config, &directory.path().join("plugin.toml")).unwrap_err();

        assert!(err.to_string().contains("exactly one"), "{err}");
    }

    #[test]
    fn the_common_name_is_read_out_of_the_certificate_that_was_issued() {
        let mut params = rcgen::CertificateParams::default();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "svc.example");
        let key = rcgen::KeyPair::generate().unwrap();
        let certificate = params.self_signed(&key).unwrap();

        assert_eq!(
            common_name(&certificate.pem()).as_deref(),
            Some("svc.example"),
        );
        assert_eq!(common_name("not a certificate"), None);
    }

    #[test]
    fn a_certificate_is_described_by_its_subject_and_its_expiry() {
        let mut params = rcgen::CertificateParams::default();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "svc.example");
        let key = rcgen::KeyPair::generate().unwrap();
        let certificate = params.self_signed(&key).unwrap();

        let (subject, expires) = summarise(&certificate.pem()).unwrap();

        assert!(subject.contains("svc.example"), "{subject}");
        assert!(expires.contains('T'), "{expires}");
    }
}
