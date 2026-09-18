//! The manual configuration package: a zip somebody imports by hand to end up
//! with this server configured as a stream.
//!
//! Enrolment is the path we want people on. This is the other one — an
//! operator downloads a package, sends it out of band, and the recipient
//! imports it into ATAK, WinTAK or iTAK. Two layouts, because the importers
//! disagree:
//!
//! * **ATAK and WinTAK** want a Mission Package **inside** a Mission Package.
//!   The outer archive is what the import dialog accepts; the inner one is what
//!   carries the `.pref` and the keystores, and it sets `onReceiveDelete` so
//!   that it cleans itself up afterwards.
//! * **iTAK** wants a flat zip with no manifest at all.
//!
//! # Where the `.p12` files end up
//!
//! ATAK's certificate sorter copies every `.p12` it finds in a package into
//! `<atak root>/cert/`, wherever it was in the archive. That is why
//! `caLocation` says `cert/truststore.p12` while the archive entry is under a
//! different directory entirely, and why the directory name itself does not
//! matter — it is a fixed constant here so that two builds produce the same
//! layout.
//!
//! # The credentials that are not in here
//!
//! ATAK never reads `username`/`password` out of a `.pref`: stream credentials
//! come from its own credential store, keyed by host. The enrolment variant
//! therefore carries no secret at all — the person types their username and
//! client password into ATAK's own prompt — and the certificate variant carries
//! only the keystore an administrator already had issued.

use rustak_api::PrefClass;
use rustak_core::prelude::*;

use super::builder::write_zip;
use super::prefs::{COT_STREAMS, PrefGroup, UserSettings, enrollment_defaults, render};
use crate::files::package::{ContentEntry, MANIFEST_PATH, Manifest, write_manifest};

/// The directory both archives nest their payload under.
///
/// Any name works — ATAK re-homes the keystores and reads the `.pref` wherever
/// it is — so it is fixed rather than random, which keeps two builds of the
/// same package byte-identical apart from the minted manifest identifiers.
const FOLDER: &str = "5c5a2c1d9e4b4f0a8d3e7b61c2f09a4d";

/// Where a keystore resolves to once ATAK's sorter has re-homed it.
const CERT_DIR: &str = "cert";

/// The truststore's filename inside the package, and inside `cert/`.
const TRUSTSTORE: &str = "truststore.p12";

/// What to build.
#[derive(Clone)]
pub struct ConfigPackageInput {
    /// The host a client connects to, which is also what the Channels
    /// preference is scoped by.
    pub host: String,
    /// The streaming port.
    pub stream_port: u16,
    /// What the connection is called in the client's server list.
    pub description: String,
    /// The account the package configures.
    pub username: String,
    pub truststore_p12: Vec<u8>,
    pub truststore_password: String,
    /// The account's own keystore and its passphrase. [`None`] builds the
    /// enrolment variant, which asks the client to enrol for its own.
    pub client_p12: Option<(Vec<u8>, String)>,
    /// Which SharedPreferences store ordinary settings are imported into.
    pub app_prefs_group: String,
    /// What the person's profile says about them, if anything.
    pub user: UserSettings,
}

impl std::fmt::Debug for ConfigPackageInput {
    /// Written out so that neither passphrase can reach a log through a `{:?}`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigPackageInput")
            .field("host", &self.host)
            .field("stream_port", &self.stream_port)
            .field("username", &self.username)
            .field("with_client_certificate", &self.client_p12.is_some())
            .finish_non_exhaustive()
    }
}

impl ConfigPackageInput {
    /// `<host>:<port>:ssl`, the connect string ATAK stores.
    fn connect_string(&self) -> String {
        format!("{}:{}:ssl", self.host, self.stream_port)
    }

    /// Where the account's keystore resolves to after import.
    fn client_location(&self) -> String {
        format!("{CERT_DIR}/{}.p12", self.username)
    }

    /// Where the truststore resolves to after import.
    fn ca_location(&self) -> String {
        format!("{CERT_DIR}/{TRUSTSTORE}")
    }

    /// The `cot_streams` group, indexed as ATAK's connection loader reads it.
    ///
    /// Only the keys `PreferenceControl` actually looks at are emitted; the
    /// rest would be carried and ignored.
    fn cot_streams(&self) -> PrefGroup {
        let group = PrefGroup::new(COT_STREAMS)
            .typed("count", PrefClass::Integer, "1")
            .with("description0", &self.description)
            .typed("enabled0", PrefClass::Boolean, "true")
            .with("connectString0", self.connect_string())
            .with("caLocation0", self.ca_location())
            .with("caPassword0", &self.truststore_password);

        let Some((_, password)) = &self.client_p12 else {
            // The enrolment variant: no keystore, so the client enrols for one
            // and the person types their credentials into ATAK's own prompt.
            return group
                .typed("enrollForCertificateWithTrust0", PrefClass::Boolean, "true")
                .typed("useAuth0", PrefClass::Boolean, "true")
                .typed("cacheCreds0", PrefClass::String, "cache_creds_both");
        };

        group
            .with("certificateLocation0", self.client_location())
            .with("clientPassword0", password)
            .typed(
                "enrollForCertificateWithTrust0",
                PrefClass::Boolean,
                "false",
            )
            .typed("useAuth0", PrefClass::Boolean, "false")
    }

    /// The unsuffixed default connection keys iTAK reads out of the app group.
    fn itak_defaults(&self, group: PrefGroup) -> PrefGroup {
        let group = group
            .with("caLocation", self.ca_location())
            .with("caPassword", &self.truststore_password);

        match &self.client_p12 {
            Some((_, password)) => group
                .with("certificateLocation", self.client_location())
                .with("clientPassword", password),
            None => group,
        }
    }

    /// The whole `.pref` document a client imports.
    fn document(&self, itak: bool) -> String {
        let mut app = enrollment_defaults(&self.host, Some(&self.user));
        app.name = self.app_prefs_group.clone();

        if itak {
            app = self.itak_defaults(app);
        }

        render(&[self.cot_streams(), app])
    }

    /// The files that travel beside the `.pref`.
    fn keystores(&self) -> Vec<(String, &[u8])> {
        let mut out = vec![(TRUSTSTORE.to_string(), self.truststore_p12.as_slice())];

        if let Some((bytes, _)) = &self.client_p12 {
            out.push((format!("{}.p12", self.username), bytes.as_slice()));
        }

        out
    }
}

/// Builds the double-wrapped package ATAK and WinTAK import.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when either archive cannot be
/// written.
pub fn build_wintak_atak(input: &ConfigPackageInput) -> Result<(String, Vec<u8>), Error> {
    let document = input.document(false);
    let pref_name = format!("{}.pref", input.username);

    let mut inner_manifest = Manifest::new(
        uuid::Uuid::new_v4().to_string(),
        format!("{}_CONFIG", input.username),
    )
    .parameter("onReceiveImport", "true")
    .parameter("onReceiveDelete", "true")
    .content(ContentEntry::new(format!("{FOLDER}/{pref_name}")));

    let mut inner: Vec<(String, Vec<u8>)> =
        vec![(format!("{FOLDER}/{pref_name}"), document.into_bytes())];

    for (name, bytes) in input.keystores() {
        let entry = format!("{FOLDER}/{name}");
        inner_manifest = inner_manifest.content(ContentEntry::new(entry.clone()));
        inner.push((entry, bytes.to_vec()));
    }

    let inner_zip = zip_with_manifest(&inner_manifest, &inner)?;

    let inner_name = format!("{}.zip", input.username);
    let outer_manifest = Manifest::new(
        uuid::Uuid::new_v4().to_string(),
        format!("{}_CONFIG", input.username),
    )
    .parameter("onReceiveImport", "true")
    .parameter("onReceiveDelete", "true")
    .content(ContentEntry::new(format!("{FOLDER}/{inner_name}")));

    let outer = zip_with_manifest(
        &outer_manifest,
        &[(format!("{FOLDER}/{inner_name}"), inner_zip)],
    )?;

    Ok((format!("{}_CONFIG.zip", input.username), outer))
}

/// Builds the flat package iTAK imports.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the archive cannot be written.
pub fn build_itak(input: &ConfigPackageInput) -> Result<(String, Vec<u8>), Error> {
    let document = input.document(true);
    let mut entries: Vec<(&str, &[u8])> = vec![("config.pref", document.as_bytes())];

    let keystores = input.keystores();
    for (name, bytes) in &keystores {
        entries.push((name.as_str(), bytes));
    }

    Ok((
        format!("{}_CONFIG_iTAK.zip", input.username),
        write_zip(&entries)?,
    ))
}

/// Writes a Mission Package holding `entries` plus its manifest.
fn zip_with_manifest(manifest: &Manifest, entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>, Error> {
    let rendered = write_manifest(manifest);
    let mut all: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, data)| (name.as_str(), data.as_slice()))
        .collect();

    all.push((MANIFEST_PATH, rendered.as_bytes()));

    write_zip(&all)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read as _};

    use super::*;
    use crate::files::package::read_manifest;
    use crate::profiles::prefs::APP_PREFS;

    fn input(with_cert: bool) -> ConfigPackageInput {
        ConfigPackageInput {
            host: "tak.example.com".to_string(),
            stream_port: 8089,
            description: "rustak".to_string(),
            username: "ada".to_string(),
            truststore_p12: b"truststore".to_vec(),
            truststore_password: "atakatak".to_string(),
            client_p12: with_cert.then(|| (b"client".to_vec(), "atakatak".to_string())),
            app_prefs_group: APP_PREFS.to_string(),
            user: UserSettings::default(),
        }
    }

    fn entry(zip: &[u8], name: &str) -> Vec<u8> {
        let mut archive = zip::ZipArchive::new(Cursor::new(zip.to_vec())).unwrap();
        let mut found = Vec::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_end(&mut found)
            .unwrap();
        found
    }

    fn names(zip: &[u8]) -> Vec<String> {
        zip::ZipArchive::new(Cursor::new(zip.to_vec()))
            .unwrap()
            .file_names()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_atak_package_wraps_a_package_inside_a_package() {
        let (filename, outer) = build_wintak_atak(&input(true)).unwrap();

        assert_eq!(filename, "ada_CONFIG.zip");

        let listed = names(&outer);
        assert!(listed.contains(&format!("{FOLDER}/ada.zip")), "{listed:?}");
        assert!(listed.contains(&MANIFEST_PATH.to_string()));

        let inner = entry(&outer, &format!("{FOLDER}/ada.zip"));
        let listed = names(&inner);

        assert!(listed.contains(&format!("{FOLDER}/ada.pref")), "{listed:?}");
        assert!(listed.contains(&format!("{FOLDER}/truststore.p12")));
        assert!(listed.contains(&format!("{FOLDER}/ada.p12")));

        let mut archive = zip::ZipArchive::new(Cursor::new(inner.clone())).unwrap();
        let (manifest, _) = read_manifest(&mut archive).unwrap();

        assert_eq!(manifest.name(), Some("ada_CONFIG"));
        assert_eq!(manifest.config("onReceiveDelete"), Some("true"));
        assert_eq!(manifest.contents.len(), 3);
    }

    #[test]
    fn the_certificate_variant_names_the_keystore_the_sorter_will_have_moved() {
        let (_, outer) = build_wintak_atak(&input(true)).unwrap();
        let inner = entry(&outer, &format!("{FOLDER}/ada.zip"));
        let document = String::from_utf8(entry(&inner, &format!("{FOLDER}/ada.pref"))).unwrap();

        assert!(document.contains(
            r#"key="connectString0" class="class java.lang.String">tak.example.com:8089:ssl<"#
        ));
        assert!(document.contains(r#">cert/truststore.p12<"#));
        assert!(document.contains(r#">cert/ada.p12<"#));
        assert!(document.contains(
            r#"<entry key="enrollForCertificateWithTrust0" class="class java.lang.Boolean">false</entry>"#
        ));
        assert!(
            document
                .contains(r#"<entry key="useAuth0" class="class java.lang.Boolean">false</entry>"#)
        );
    }

    #[test]
    fn the_enrolment_variant_carries_no_keystore_and_asks_the_client_to_enrol() {
        let (_, outer) = build_wintak_atak(&input(false)).unwrap();
        let inner = entry(&outer, &format!("{FOLDER}/ada.zip"));

        assert!(!names(&inner).contains(&format!("{FOLDER}/ada.p12")));

        let document = String::from_utf8(entry(&inner, &format!("{FOLDER}/ada.pref"))).unwrap();

        assert!(!document.contains("certificateLocation"));
        assert!(!document.contains("clientPassword"));
        assert!(document.contains(
            r#"<entry key="enrollForCertificateWithTrust0" class="class java.lang.Boolean">true</entry>"#
        ));
        assert!(
            document
                .contains(r#"<entry key="useAuth0" class="class java.lang.Boolean">true</entry>"#)
        );
        assert!(
            !document.contains("key=\"password") && !document.contains("key=\"username"),
            "ATAK never reads those keys, so putting a secret there would only leak it",
        );
    }

    #[test]
    fn the_itak_package_is_flat_with_no_manifest() {
        let (filename, built) = build_itak(&input(true)).unwrap();

        assert_eq!(filename, "ada_CONFIG_iTAK.zip");

        let listed = names(&built);
        assert_eq!(
            listed,
            vec!["config.pref", "truststore.p12", "ada.p12"],
            "iTAK reads a flat archive",
        );

        let document = String::from_utf8(entry(&built, "config.pref")).unwrap();

        assert!(
            document.contains(r#"<entry key="caLocation" class="class java.lang.String">"#),
            "iTAK reads the unsuffixed defaults out of the app group",
        );
        assert!(
            document
                .contains(r#"<entry key="certificateLocation" class="class java.lang.String">"#)
        );
        assert!(document.contains("cot_streams"), "and the indexed ones too");
    }

    #[test]
    fn every_document_carries_the_group_that_turns_connect_profiles_on() {
        for input in [input(true), input(false)] {
            for document in [input.document(false), input.document(true)] {
                assert!(document.starts_with("<?xml version='1.0' standalone='yes'?>"));
                assert!(document.contains("deviceProfileEnableOnConnect"));
                assert!(document.contains("prefs_enable_channels_host-tak.example.com"));
            }
        }
    }

    #[test]
    fn a_passphrase_cannot_reach_a_log_through_a_debug_rendering() {
        let rendered = format!("{:?}", input(true));

        assert!(!rendered.contains("atakatak"));
        assert!(rendered.contains("with_client_certificate: true"));
    }
}
