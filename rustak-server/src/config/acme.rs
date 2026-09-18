//! `[acme]` — obtaining the public certificate from a certificate authority.
//!
//! Only `[web.public]` uses this. The Marti and stream listeners present
//! certificates from our own CA, because their clients are devices we enrolled
//! and handed a truststore to; the public listener is the one a browser reaches,
//! and a browser trusts what a public authority signed.
//!
//! # Challenges decide which port has to be open
//!
//! `tls-alpn-01` is answered on port **443** during the TLS handshake itself,
//! so it needs nothing beyond a listener already bound there. `http-01` is
//! answered over plaintext HTTP on port **80**, which means
//! [`plain_bind`](super::PublicWebConfig::plain_bind) or a port-80 listener.
//! The authority connects to those ports and no others, so
//! [`Config::validate`](super::Config::validate) refuses a combination that
//! could never complete rather than letting the first renewal discover it.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::ServerConfig;

/// Let's Encrypt's production directory.
const LETSENCRYPT_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// Let's Encrypt's staging directory, which issues untrusted certificates
/// against far more generous rate limits.
const LETSENCRYPT_STAGING_URL: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

/// The alias accepted in place of [`LETSENCRYPT_URL`].
const LETSENCRYPT_ALIAS: &str = "letsencrypt";

/// The alias accepted in place of [`LETSENCRYPT_STAGING_URL`].
const LETSENCRYPT_STAGING_ALIAS: &str = "letsencrypt-staging";

/// How long before expiry a certificate is renewed.
fn default_renew_before() -> chrono::Duration {
    chrono::Duration::days(30)
}

/// `[acme]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcmeConfig {
    /// Whether to order and renew a certificate.
    ///
    /// Must agree with `[web.public.tls] mode = "acme"`: the two are separate
    /// keys because one says where the certificate comes from and the other
    /// turns the ordering machinery on, and an installation that sets only one
    /// of them is told so rather than silently serving the wrong certificate.
    #[serde(default)]
    pub enabled: bool,

    /// The ACME directory to order from.
    #[serde(default)]
    pub directory: AcmeDirectory,

    /// The contact the authority uses to warn about expiring certificates.
    ///
    /// Written as an email address or as a `mailto:` URI; both are accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,

    /// Whether the authority's terms of service are accepted.
    ///
    /// Off by default and required before an order is placed: agreeing to
    /// somebody's terms on an operator's behalf is not ours to do.
    #[serde(default)]
    pub accept_tos: bool,

    /// The names to request. Defaults to `[server] domains`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,

    /// How the authority verifies that we control those names.
    #[serde(default)]
    pub challenge: AcmeChallenge,

    /// How long before expiry the certificate is renewed.
    #[serde(
        default = "default_renew_before",
        with = "rustak_core::config::duration::humane"
    )]
    pub renew_before: chrono::Duration,
}

impl Default for AcmeConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    fn default() -> Self {
        Self {
            enabled: false,
            directory: AcmeDirectory::default(),
            contact: None,
            accept_tos: false,
            domains: Vec::new(),
            challenge: AcmeChallenge::default(),
            renew_before: default_renew_before(),
        }
    }
}

impl AcmeConfig {
    /// The names to request, falling back to the server's own domains.
    pub fn domains<'a>(&'a self, server: &'a ServerConfig) -> &'a [String] {
        if self.domains.is_empty() {
            &server.domains
        } else {
            &self.domains
        }
    }

    /// The contact to register with the authority, as the `mailto:` URI the
    /// protocol expects.
    pub fn contacts(&self) -> Vec<String> {
        self.contact
            .iter()
            .map(|contact| {
                if contact.contains(':') {
                    contact.clone()
                } else {
                    format!("mailto:{contact}")
                }
            })
            .collect()
    }

    /// Why `name` could never be ordered from a public authority, if it could
    /// not.
    ///
    /// The three ways a configured name is hopeless: an address literal (ACME
    /// has an `ip` identifier, but no public authority offers it), a bare
    /// label with no domain at all, and anything under a suffix RFC 6761 or
    /// RFC 8375 reserves for private networks. A wildcard is judged by the
    /// name under it, because that is the name an authority validates.
    ///
    /// Returns a fragment that completes "cannot be ordered … : {reason}".
    pub fn unorderable(name: &str) -> Option<&'static str> {
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
        let name = name.strip_prefix("*.").unwrap_or(&name);

        if name.parse::<std::net::IpAddr>().is_ok() {
            return Some(
                "it is an IP address, and a public certificate authority will not issue for one",
            );
        }

        let Some((_, suffix)) = name.rsplit_once('.') else {
            return Some("it is not a fully qualified name, so no authority could issue for it");
        };

        let reserved = matches!(
            suffix,
            "local" | "localhost" | "internal" | "lan" | "home" | "invalid" | "test"
        ) || name.ends_with(".home.arpa");

        reserved.then_some(
            "it is under a suffix reserved for private networks, which no public authority can validate",
        )
    }
}

/// The ACME directory to order from.
///
/// Accepts the two aliases every deployment wants — `letsencrypt` and
/// `letsencrypt-staging` — or the URL of any other authority's directory. The
/// alias is kept rather than resolved on the way in, so that a configuration we
/// write back out says what the operator wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AcmeDirectory {
    /// Let's Encrypt production.
    #[default]
    LetsEncrypt,

    /// Let's Encrypt staging: untrusted certificates, generous rate limits.
    /// Where a deployment should be tested before it is pointed at production.
    LetsEncryptStaging,

    /// Another authority's directory URL.
    Url(String),
}

impl AcmeDirectory {
    /// The directory URL to order from.
    pub fn url(&self) -> &str {
        match self {
            Self::LetsEncrypt => LETSENCRYPT_URL,
            Self::LetsEncryptStaging => LETSENCRYPT_STAGING_URL,
            Self::Url(url) => url,
        }
    }
}

impl fmt::Display for AcmeDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LetsEncrypt => formatter.write_str(LETSENCRYPT_ALIAS),
            Self::LetsEncryptStaging => formatter.write_str(LETSENCRYPT_STAGING_ALIAS),
            Self::Url(url) => formatter.write_str(url),
        }
    }
}

impl FromStr for AcmeDirectory {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim() {
            LETSENCRYPT_ALIAS => Ok(Self::LetsEncrypt),
            LETSENCRYPT_STAGING_ALIAS => Ok(Self::LetsEncryptStaging),
            // An ACME account key is established over this URL and every order
            // is authenticated with it; a plaintext directory would put the
            // whole exchange, and the certificate it produces, on the wire.
            url if url.starts_with("https://") => Ok(Self::Url(url.to_string())),
            other => Err(format!(
                "'{other}' is not an ACME directory; write \"{LETSENCRYPT_ALIAS}\", \"{LETSENCRYPT_STAGING_ALIAS}\", or an https:// directory URL"
            )),
        }
    }
}

impl Serialize for AcmeDirectory {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AcmeDirectory {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// How an authority verifies that we control a name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcmeChallenge {
    /// Answered during the TLS handshake on port 443. The default: it needs no
    /// plaintext port, and the listener answering it is one we already bind.
    #[default]
    #[serde(rename = "tls-alpn-01")]
    TlsAlpn01,

    /// Answered over plaintext HTTP on port 80.
    #[serde(rename = "http-01")]
    Http01,
}

impl AcmeChallenge {
    /// The port the authority connects to for this challenge.
    pub fn port(&self) -> u16 {
        match self {
            Self::TlsAlpn01 => 443,
            Self::Http01 => 80,
        }
    }

    /// The challenge as the configuration file spells it, so that an error
    /// message quotes something an operator can search their file for.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TlsAlpn01 => "tls-alpn-01",
            Self::Http01 => "http-01",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_section_is_the_written_out_default() {
        let parsed: AcmeConfig = toml::from_str("").unwrap();

        assert_eq!(parsed, AcmeConfig::default());
        assert!(!parsed.enabled);
        assert!(!parsed.accept_tos);
        assert_eq!(parsed.directory, AcmeDirectory::LetsEncrypt);
        assert_eq!(parsed.challenge, AcmeChallenge::TlsAlpn01);
        assert_eq!(parsed.renew_before, chrono::Duration::days(30));
    }

    #[test]
    fn both_aliases_and_a_url_are_accepted() {
        for (written, expected, url) in [
            ("letsencrypt", AcmeDirectory::LetsEncrypt, LETSENCRYPT_URL),
            (
                "letsencrypt-staging",
                AcmeDirectory::LetsEncryptStaging,
                LETSENCRYPT_STAGING_URL,
            ),
            (
                "https://ca.example.com/acme/directory",
                AcmeDirectory::Url("https://ca.example.com/acme/directory".to_string()),
                "https://ca.example.com/acme/directory",
            ),
        ] {
            let parsed: AcmeConfig = toml::from_str(&format!("directory = \"{written}\"")).unwrap();

            assert_eq!(parsed.directory, expected, "{written}");
            assert_eq!(parsed.directory.url(), url, "{written}");
            // What we read is what we would write back.
            assert_eq!(parsed.directory.to_string(), written, "{written}");
        }
    }

    #[test]
    fn a_plaintext_directory_is_refused() {
        // The account key is established over this URL and every order is
        // authenticated with it.
        let Err(err) =
            toml::from_str::<AcmeConfig>(r#"directory = "http://ca.example.com/directory""#)
        else {
            panic!("a plaintext directory should be refused");
        };

        assert!(err.to_string().contains("https://"), "{err}");
    }

    #[test]
    fn a_misspelled_alias_says_what_is_accepted() {
        let Err(err) = toml::from_str::<AcmeConfig>(r#"directory = "lets-encrypt""#) else {
            panic!("an unknown alias should be refused");
        };

        assert!(err.to_string().contains("letsencrypt"), "{err}");
    }

    #[test]
    fn the_names_fall_back_to_the_servers_own_domains() {
        let server: ServerConfig = toml::from_str(r#"domains = ["tak.example.com"]"#).unwrap();

        let inherited: AcmeConfig = toml::from_str("").unwrap();
        assert_eq!(inherited.domains(&server), ["tak.example.com"]);

        let overridden: AcmeConfig = toml::from_str(r#"domains = ["public.example.com"]"#).unwrap();
        assert_eq!(overridden.domains(&server), ["public.example.com"]);
    }

    #[test]
    fn a_bare_email_address_becomes_a_mailto_uri() {
        let bare: AcmeConfig = toml::from_str(r#"contact = "ops@example.com""#).unwrap();
        assert_eq!(bare.contacts(), vec!["mailto:ops@example.com"]);

        let written: AcmeConfig = toml::from_str(r#"contact = "mailto:ops@example.com""#).unwrap();
        assert_eq!(written.contacts(), vec!["mailto:ops@example.com"]);

        assert!(AcmeConfig::default().contacts().is_empty());
    }

    #[test]
    fn each_challenge_names_the_port_it_needs() {
        assert_eq!(AcmeChallenge::TlsAlpn01.port(), 443);
        assert_eq!(AcmeChallenge::Http01.port(), 80);
    }

    #[test]
    fn a_challenge_is_reported_the_way_the_file_spells_it() {
        // Validation quotes this back at the operator, who has to be able to
        // find it in their own file.
        for challenge in [AcmeChallenge::TlsAlpn01, AcmeChallenge::Http01] {
            let parsed: AcmeConfig =
                toml::from_str(&format!("challenge = \"{}\"", challenge.as_str())).unwrap();

            assert_eq!(parsed.challenge, challenge);
        }
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let Err(err) = toml::from_str::<AcmeConfig>(r#"contact_email = "ops@example.com""#) else {
            panic!("an unknown key should be refused");
        };

        assert!(err.to_string().contains("contact_email"), "{err}");
    }

    #[test]
    fn a_name_a_public_authority_could_issue_for_is_accepted() {
        for name in [
            "tak.example.com",
            "TAK.example.com.",
            "*.example.com",
            "a.b.c.example.co.uk",
        ] {
            assert_eq!(AcmeConfig::unorderable(name), None, "{name}");
        }
    }

    #[test]
    fn a_name_that_could_never_be_validated_says_why() {
        // Each of these is refused by the authority only *after* it has spent
        // a failed-validation slot, so the reason has to come from us.
        for (name, fragment) in [
            ("192.168.1.10", "IP address"),
            ("2001:db8::1", "IP address"),
            ("*.10.0.0.1", "IP address"),
            ("localhost", "fully qualified"),
            ("rustak", "fully qualified"),
            ("tak.lan", "private networks"),
            ("tak.local", "private networks"),
            ("tak.internal", "private networks"),
            ("printer.home.arpa", "private networks"),
            ("*.tak.lan", "private networks"),
            ("TAK.LAN.", "private networks"),
        ] {
            let reason = AcmeConfig::unorderable(name)
                .unwrap_or_else(|| panic!("{name} should not be orderable"));

            assert!(reason.contains(fragment), "{name}: {reason}");
        }
    }
}
