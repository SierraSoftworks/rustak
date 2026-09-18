//! Reading the two connect strings a TAK deployment writes.
//!
//! ATAK stores a server as `host:port:protocol` (`takserver.example.com:8089:ssl`)
//! and CloudTAK writes a URL (`ssl://takserver.example.com:8089`). Both name the
//! same thing, both appear in configuration files an operator copies between
//! products, and neither is a URL any URL parser will accept — so [`Endpoint`]
//! reads both.
//!
//! ```
//! use rustak_client::stream::Endpoint;
//!
//! let atak: Endpoint = "takserver.example.com:8089:ssl".parse()?;
//! let cloudtak: Endpoint = "ssl://takserver.example.com:8089".parse()?;
//!
//! assert_eq!(atak, cloudtak);
//! assert_eq!(atak.to_string(), "ssl://takserver.example.com:8089");
//! # Ok::<(), rustak_client::stream::StreamError>(())
//! ```
//!
//! # Plain TCP
//!
//! `tcp://…` and `host:port:tcp` are refused unless the crate was built with
//! the `insecure-tcp` feature (which the `testing` feature implies). rustak
//! has no plaintext stream input, so a client that will dial one is only ever
//! useful to a test harness — or to an attacker who can edit a config file.

use std::str::FromStr;

use super::StreamError;

/// Where a TAK server's streaming port is, and how to talk to it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint {
    /// The host to dial, and the name the server's certificate must match.
    /// Unbracketed, even for IPv6.
    pub host: String,

    /// The TCP port.
    pub port: u16,

    /// Whether to wrap the connection in TLS. Always true in a default build.
    pub tls: bool,
}

impl Endpoint {
    /// The streaming port every TAK server listens on.
    pub const DEFAULT_PORT: u16 = 8089;

    /// A TLS endpoint, the only kind a default build can dial.
    pub fn tls(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            tls: true,
        }
    }

    /// An endpoint with the transport named explicitly.
    ///
    /// Constructing a plain-TCP endpoint is not in itself refused — dialling
    /// one is, in a build without `insecure-tcp` — so that a configuration
    /// error is reported once, at connect time, rather than twice.
    pub fn new(host: impl Into<String>, port: u16, tls: bool) -> Self {
        Self {
            host: host.into(),
            port,
            tls,
        }
    }

    /// `host:port`, with an IPv6 host bracketed.
    #[must_use]
    pub fn authority(&self) -> String {
        match self.host.contains(':') {
            true => format!("[{}]:{}", self.host, self.port),
            false => format!("{}:{}", self.host, self.port),
        }
    }

    /// The scheme this endpoint is written with: `ssl` or `tcp`.
    #[must_use]
    pub const fn scheme(&self) -> &'static str {
        match self.tls {
            true => "ssl",
            false => "tcp",
        }
    }

    /// The ATAK form of this endpoint, `host:port:protocol`.
    #[must_use]
    pub fn connect_string(&self) -> String {
        format!("{}:{}:{}", self.host, self.port, self.scheme())
    }
}

impl std::fmt::Display for Endpoint {
    /// The CloudTAK form, `ssl://host:port`, which is also what a sidecar's
    /// `[server] stream` setting holds.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}://{}", self.scheme(), self.authority())
    }
}

impl FromStr for Endpoint {
    type Err = StreamError;

    fn from_str(text: &str) -> Result<Self, StreamError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(StreamError::Endpoint("an empty connect string".to_string()));
        }

        let (tls, rest) = match text.split_once("://") {
            Some((scheme, rest)) => (transport(scheme, text)?, rest.trim_end_matches('/')),
            None => (true, text),
        };

        let (host, port, suffix) = split_authority(rest, text)?;
        let tls = match suffix {
            Some(suffix) if text.contains("://") => {
                return Err(StreamError::Endpoint(format!(
                    "{text:?}, which mixes a '{suffix}' suffix into a URL",
                )));
            }
            Some(suffix) => transport(suffix, text)?,
            None => tls,
        };

        if host.is_empty() {
            return Err(StreamError::Endpoint(format!(
                "{text:?}, which names no host"
            )));
        }

        refuse_plaintext(tls, text)?;

        Ok(Self {
            host: host.to_string(),
            port,
            tls,
        })
    }
}

/// Reads a scheme or an ATAK protocol suffix.
fn transport(name: &str, text: &str) -> Result<bool, StreamError> {
    match name.trim().to_ascii_lowercase().as_str() {
        "ssl" | "tls" | "ssls" => Ok(true),
        "tcp" => Ok(false),
        other => Err(StreamError::Endpoint(format!(
            "{text:?}, whose '{other}' transport is not one a TAK stream speaks",
        ))),
    }
}

/// Splits `host:port[:protocol]`, bracketed IPv6 included.
fn split_authority<'a>(
    rest: &'a str,
    text: &str,
) -> Result<(&'a str, u16, Option<&'a str>), StreamError> {
    let (host, tail) = match rest.strip_prefix('[') {
        Some(bracketed) => match bracketed.split_once(']') {
            Some((host, tail)) => (host, tail.strip_prefix(':').unwrap_or("")),
            None => {
                return Err(StreamError::Endpoint(format!(
                    "{text:?}, whose '[' is never closed",
                )));
            }
        },
        None => match rest.split_once(':') {
            Some((host, tail)) => (host, tail),
            None => (rest, ""),
        },
    };

    let (port, suffix) = match tail.split_once(':') {
        Some((port, suffix)) => (port, Some(suffix)),
        None => (tail, None),
    };

    let port = match port.trim() {
        "" => Endpoint::DEFAULT_PORT,
        digits => digits.parse().map_err(|_| {
            StreamError::Endpoint(format!("{text:?}, whose port {digits:?} is not a number"))
        })?,
    };

    Ok((host, port, suffix))
}

/// Refuses a plaintext endpoint in a build that has no business dialling one.
#[cfg(any(test, feature = "insecure-tcp"))]
const fn refuse_plaintext(_tls: bool, _text: &str) -> Result<(), StreamError> {
    Ok(())
}

/// Refuses a plaintext endpoint in a build that has no business dialling one.
#[cfg(not(any(test, feature = "insecure-tcp")))]
fn refuse_plaintext(tls: bool, text: &str) -> Result<(), StreamError> {
    match tls {
        true => Ok(()),
        false => Err(StreamError::Endpoint(format!(
            "{text:?}, because this build speaks TLS only — rebuild with the 'insecure-tcp' feature if you are running a test harness",
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_products_write_the_same_endpoint_differently() {
        // The property that matters: an operator copying a server between ATAK
        // and CloudTAK ends up at the same socket.
        let atak: Endpoint = "takserver.example.com:8089:ssl".parse().unwrap();
        let cloudtak: Endpoint = "ssl://takserver.example.com:8089".parse().unwrap();

        assert_eq!(atak, cloudtak);
        assert_eq!(atak.host, "takserver.example.com");
        assert_eq!(atak.port, 8089);
        assert!(atak.tls);
    }

    #[test]
    fn the_two_renderings_round_trip_through_their_own_parsers() {
        let endpoint = Endpoint::tls("tak.example.com", 8089);

        assert_eq!(endpoint.to_string(), "ssl://tak.example.com:8089");
        assert_eq!(endpoint.connect_string(), "tak.example.com:8089:ssl");
        assert_eq!(endpoint.to_string().parse::<Endpoint>().unwrap(), endpoint);
        assert_eq!(
            endpoint.connect_string().parse::<Endpoint>().unwrap(),
            endpoint,
        );
    }

    #[test]
    fn tls_is_what_an_endpoint_without_a_transport_means() {
        // Secure by default, at the only place where the default is chosen.
        for text in ["tak.example.com:8089", "tls://tak.example.com:8089"] {
            assert!(text.parse::<Endpoint>().unwrap().tls, "{text}");
        }
    }

    #[test]
    fn the_streaming_port_is_the_default_when_none_is_written() {
        let endpoint: Endpoint = "ssl://tak.example.com".parse().unwrap();

        assert_eq!(endpoint.port, Endpoint::DEFAULT_PORT);
    }

    #[test]
    fn an_ipv6_host_survives_both_forms() {
        // The host is stored unbracketed because that is what
        // `ServerName::try_from` and `TcpStream::connect` both want; the
        // brackets come back in `authority`.
        for text in ["[::1]:8089:ssl", "ssl://[::1]:8089"] {
            let endpoint: Endpoint = text.parse().unwrap();

            assert_eq!(endpoint.host, "::1", "{text}");
            assert_eq!(endpoint.port, 8089, "{text}");
            assert_eq!(endpoint.authority(), "[::1]:8089", "{text}");
        }
    }

    #[test]
    fn plain_tcp_is_read_only_where_the_build_allows_it() {
        // Under `cfg(test)` this crate is always the permissive build; the
        // refusal itself is asserted by the compile-time configuration, and
        // this test pins the *parse* so that the feature gate is the only
        // thing standing between a config file and a plaintext socket.
        let endpoint: Endpoint = "tcp://localhost:8087".parse().unwrap();

        assert!(!endpoint.tls);
        assert_eq!(endpoint.to_string(), "tcp://localhost:8087");
        assert!(!"localhost:8087:tcp".parse::<Endpoint>().unwrap().tls);
    }

    #[test]
    fn a_transport_a_tak_stream_does_not_speak_is_refused_by_name() {
        for text in ["udp://host:1", "host:1:udp", "quic://host:1"] {
            let Err(error) = text.parse::<Endpoint>() else {
                panic!("{text} should not parse");
            };

            assert!(matches!(error, StreamError::Endpoint(_)), "{error:?}");
            assert!(error.to_string().contains(text), "{error}");
        }
    }

    #[test]
    fn nonsense_is_refused_rather_than_guessed_at() {
        for text in [
            "",
            "   ",
            "ssl://",
            "ssl://host:notaport",
            "ssl://[::1:8089",
        ] {
            assert!(
                text.parse::<Endpoint>().is_err(),
                "{text:?} should not parse"
            );
        }
    }

    #[test]
    fn a_url_does_not_also_carry_an_atak_suffix() {
        // `ssl://host:8089:tcp` says two contradictory things; guessing which
        // one the operator meant is how a client ends up in plaintext.
        let Err(error) = "ssl://host:8089:tcp".parse::<Endpoint>() else {
            panic!("a mixed connect string should not parse");
        };

        assert!(error.to_string().contains("tcp"), "{error}");
    }
}
