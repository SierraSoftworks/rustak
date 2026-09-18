//! `host:port` as a configuration value.
//!
//! automate parsed its one listen address inline with `split_once(':')` and an
//! "empty host means `0.0.0.0`" rule. rustak has five listeners, one of which
//! ([`[web.public]`](https://github.com/SierraSoftworks/rustak)) takes a *list*
//! of addresses, and every one of them is a socket we bind before we can serve
//! anything — so the parse happens once, here, where the rules can be written
//! down and tested rather than repeated at each call site.
//!
//! # What is accepted
//!
//! | Written | Binds |
//! |---|---|
//! | `":8446"` | every IPv4 interface, port 8446 |
//! | `"0.0.0.0:8446"` | the same, said explicitly |
//! | `"127.0.0.1:8446"` | loopback only |
//! | `"[::]:8446"` | every IPv6 interface (and IPv4 too, on a dual-stack host) |
//! | `"[::1]:8446"` | IPv6 loopback only |
//! | `"tak.example.com:8446"` | whatever the name resolves to at bind time |
//!
//! An IPv6 literal must be bracketed. `::1:8446` is genuinely ambiguous — it is
//! also a valid IPv6 address in its own right — so we refuse it and say so
//! rather than guess which one the operator meant.
//!
//! Port `0` is accepted: it asks the operating system for an ephemeral port,
//! which is how the integration tests bind a listener without racing each other
//! for a fixed number.
//!
//! # Resolution happens at bind time, not at parse time
//!
//! A [`ListenAddr`] holds the host *as written*. A name that does not resolve
//! yet — a container hostname, a dual-stack interface still coming up — is not
//! a configuration error, and failing to load the file over it would make the
//! server's start-up depend on DNS. [`ListenAddr::to_socket_addrs`] is where
//! that lookup happens, and where its failure is reported.

use std::fmt;
use std::net::{SocketAddr, ToSocketAddrs};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The host substituted for an omitted one, as in `":8446"`.
const WILDCARD_HOST: &str = "0.0.0.0";

/// Advice attached to every parse failure in this module.
const ADVICE_LISTEN_FORMAT: &[&str] = &[
    "Write a listen address as \"host:port\", for example \"0.0.0.0:8446\".",
    "Omit the host (\":8446\") to listen on every IPv4 interface.",
    "Bracket an IPv6 literal, for example \"[::]:8446\" or \"[::1]:8446\".",
];

/// An address a rustak listener binds: a host as written, plus a port.
///
/// ```
/// # use rustak_core::config::listen::ListenAddr;
/// let every_interface: ListenAddr = ":8446".parse().unwrap();
/// assert_eq!(every_interface.host(), "0.0.0.0");
/// assert_eq!(every_interface.port(), 8446);
///
/// // An IPv6 literal keeps its brackets when written back out, so a
/// // configuration file we rewrite still parses.
/// let dual_stack: ListenAddr = "[::]:8089".parse().unwrap();
/// assert_eq!(dual_stack.to_string(), "[::]:8089");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ListenAddr {
    host: String,
    port: u16,
}

impl ListenAddr {
    /// Builds an address from a host and port without going through the text
    /// form. An empty host means every IPv4 interface, as `":8446"` does.
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        let host = host.into();
        let host = if host.is_empty() {
            WILDCARD_HOST.to_string()
        } else {
            host
        };

        Self { host, port }
    }

    /// The host as written, with any IPv6 brackets removed.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port to bind.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Reports whether this address binds every interface of its family, which
    /// is what start-up logging and the ACME validation check need to know.
    pub fn is_wildcard(&self) -> bool {
        self.host == WILDCARD_HOST || self.host == "::"
    }

    /// Resolves the address to every socket it names.
    ///
    /// A host name may resolve to several addresses (an A record and a AAAA
    /// record, say); all of them are returned so that the caller can bind each
    /// one, which is what "listen on `tak.example.com`" means on a dual-stack
    /// host.
    ///
    /// # Errors
    ///
    /// Returns a [`human_errors::Kind::User`] error when the host does not
    /// resolve, or resolves to nothing.
    pub fn to_socket_addrs(&self) -> Result<Vec<SocketAddr>, human_errors::Error> {
        let resolved: Vec<SocketAddr> = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|err| {
                human_errors::user(
                    format!("We could not resolve the listen address '{self}': {err}"),
                    &[
                        "Check that the host name is spelled correctly and resolves on this machine.",
                        "Use an IP address, or omit the host to listen on every interface.",
                    ],
                )
            })?
            .collect();

        if resolved.is_empty() {
            return Err(human_errors::user(
                format!("The listen address '{self}' did not resolve to any address."),
                &[
                    "Check that the host name resolves on this machine.",
                    "Use an IP address, or omit the host to listen on every interface.",
                ],
            ));
        }

        Ok(resolved)
    }
}

impl fmt::Display for ListenAddr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // An IPv6 literal has to go back out bracketed or the value we write
        // would not parse as the value we read.
        if self.host.contains(':') {
            write!(formatter, "[{}]:{}", self.host, self.port)
        } else {
            write!(formatter, "{}:{}", self.host, self.port)
        }
    }
}

impl FromStr for ListenAddr {
    type Err = human_errors::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();

        let (host, port) = if let Some(rest) = text.strip_prefix('[') {
            // A bracketed IPv6 literal: "[::1]:8446".
            let Some((host, rest)) = rest.split_once(']') else {
                return Err(human_errors::user(
                    format!("The listen address '{text}' is missing its closing ']'."),
                    ADVICE_LISTEN_FORMAT,
                ));
            };

            let Some(port) = rest.strip_prefix(':') else {
                return Err(human_errors::user(
                    format!("The listen address '{text}' does not give a port after ']'."),
                    ADVICE_LISTEN_FORMAT,
                ));
            };

            (host, port)
        } else {
            let Some((host, port)) = text.rsplit_once(':') else {
                return Err(human_errors::user(
                    format!("The listen address '{text}' does not give a port."),
                    ADVICE_LISTEN_FORMAT,
                ));
            };

            if host.contains(':') {
                // An unbracketed IPv6 literal is ambiguous: "::1:8446" is both
                // "port 8446 on ::1" and an address in its own right.
                return Err(human_errors::user(
                    format!(
                        "The listen address '{text}' looks like an IPv6 address without brackets."
                    ),
                    ADVICE_LISTEN_FORMAT,
                ));
            }

            (host, port)
        };

        let port: u16 = port.trim().parse().map_err(|_| {
            human_errors::user(
                format!("The listen address '{text}' does not have a valid port number."),
                ADVICE_LISTEN_FORMAT,
            )
        })?;

        Ok(Self::new(host.trim(), port))
    }
}

impl Serialize for ListenAddr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ListenAddr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(":8446", "0.0.0.0", 8446)]
    #[case("0.0.0.0:8446", "0.0.0.0", 8446)]
    #[case("127.0.0.1:8087", "127.0.0.1", 8087)]
    #[case("[::]:8089", "::", 8089)]
    #[case("[::1]:8443", "::1", 8443)]
    #[case("tak.example.com:8446", "tak.example.com", 8446)]
    #[case("0.0.0.0:0", "0.0.0.0", 0)]
    fn every_form_in_the_documented_table_parses(
        #[case] written: &str,
        #[case] host: &str,
        #[case] port: u16,
    ) {
        let parsed: ListenAddr = written.parse().expect("{written} should parse");

        assert_eq!(parsed.host(), host);
        assert_eq!(parsed.port(), port);
    }

    #[rstest]
    #[case("8446", "does not give a port")]
    #[case("::1:8446", "without brackets")]
    #[case("[::1:8446", "closing ']'")]
    #[case("[::1]8446", "port after ']'")]
    #[case("0.0.0.0:https", "valid port number")]
    #[case("0.0.0.0:70000", "valid port number")]
    fn an_address_we_cannot_bind_is_refused_and_says_why(
        #[case] written: &str,
        #[case] expected: &str,
    ) {
        let Err(err) = written.parse::<ListenAddr>() else {
            panic!("{written} should not parse");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains(expected), "{err}");
    }

    #[test]
    fn what_we_write_is_what_we_read() {
        // The property that matters for the settings table and for any file we
        // rewrite: an address that round-trips through the text form is the
        // same address, brackets and all.
        for written in [":8446", "127.0.0.1:8087", "[::]:8089", "[::1]:8443"] {
            let parsed: ListenAddr = written.parse().unwrap();
            let reparsed: ListenAddr = parsed.to_string().parse().unwrap();

            assert_eq!(parsed, reparsed, "{written}");
        }
    }

    #[test]
    fn an_omitted_host_means_every_ipv4_interface() {
        // The one piece of automate's inline parsing worth keeping: `:8446` is
        // how the example configuration writes "listen everywhere".
        let parsed: ListenAddr = ":8446".parse().unwrap();

        assert!(parsed.is_wildcard());
        assert_eq!(parsed.to_string(), "0.0.0.0:8446");
    }

    #[test]
    fn a_wildcard_is_recognised_in_both_families() {
        assert!(":8446".parse::<ListenAddr>().unwrap().is_wildcard());
        assert!("[::]:8446".parse::<ListenAddr>().unwrap().is_wildcard());
        assert!(
            !"127.0.0.1:8446"
                .parse::<ListenAddr>()
                .unwrap()
                .is_wildcard()
        );
    }

    #[test]
    fn a_literal_address_resolves_without_touching_the_network() {
        let resolved = "127.0.0.1:8446"
            .parse::<ListenAddr>()
            .unwrap()
            .to_socket_addrs()
            .unwrap();

        assert_eq!(resolved, vec!["127.0.0.1:8446".parse().unwrap()]);
    }

    #[test]
    fn a_host_that_does_not_resolve_is_reported_at_bind_time_not_at_parse_time() {
        // Loading the configuration must not depend on DNS: a container host
        // name that is not up yet is a start-up condition, not a syntax error.
        let parsed: ListenAddr = "rustak-nonexistent.invalid:8446".parse().unwrap();

        assert_eq!(parsed.host(), "rustak-nonexistent.invalid");
        assert!(parsed.to_socket_addrs().is_err());
    }

    #[test]
    fn an_address_survives_a_trip_through_a_configuration_file() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct Web {
            listen: Vec<ListenAddr>,
        }

        let original = Web {
            listen: vec![":8446".parse().unwrap(), "[::]:8446".parse().unwrap()],
        };

        let written = toml::to_string(&original).unwrap();
        assert!(written.contains("\"0.0.0.0:8446\""), "{written}");
        assert!(written.contains("\"[::]:8446\""), "{written}");

        let read: Web = toml::from_str(&written).unwrap();
        assert_eq!(read, original);
    }
}
