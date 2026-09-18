//! Reading the things an HTTP request says about itself.
//!
//! Every function here honours `[server] trust_proxy`, because each of them
//! reads a header a client is free to send. With no proxy in front, believing
//! `X-Forwarded-For` would let anybody claim any source address — which is
//! exactly what the credential rate limiter keys on — and believing
//! `X-Forwarded-Proto` would let a plaintext request claim to have been secure.
//!
//! Where a proxy *is* trusted, the left-most entry of a comma-separated list is
//! taken, which is the client as the closest proxy recorded it. That is only
//! correct behind a proxy which overwrites any inbound value, and the
//! configuration key documents that requirement.

use std::net::{IpAddr, SocketAddr};

use actix_web::HttpRequest;
use actix_web::http::header::HeaderMap;

/// One header as a string, when it is present and is text.
pub fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// The left-most entry of a header that may carry a list.
fn leftmost<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    header_str(headers, name)
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
}

/// Where the request came from, as a string for the access-control filter.
pub fn client_ip(
    trust_proxy: bool,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> Option<String> {
    if trust_proxy && let Some(client) = leftmost(headers, "x-forwarded-for") {
        return Some(client.to_string());
    }

    peer.map(|address| address.ip().to_string())
}

/// Where the request came from, parsed, for the rate limiter's buckets.
///
/// A forwarded address that does not parse is dropped rather than guessed at:
/// an unparseable key would put every such request in one bucket, which is a
/// denial of service somebody could aim at an account.
pub fn client_address(
    trust_proxy: bool,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> Option<IpAddr> {
    if trust_proxy
        && let Some(client) = leftmost(headers, "x-forwarded-for")
        && let Ok(address) = client.parse()
    {
        return Some(address);
    }

    peer.map(|address| address.ip())
}

/// Whether the request reached the client over TLS.
pub fn is_https(trust_proxy: bool, headers: &HeaderMap, scheme: Option<&str>) -> bool {
    if trust_proxy && let Some(proto) = leftmost(headers, "x-forwarded-proto") {
        return proto.eq_ignore_ascii_case("https");
    }

    scheme == Some("https")
}

/// The base URL this request was addressed to, reconstructed from its own
/// headers.
///
/// The last resort: it is what a first-run installation has before the wizard
/// has been told a host name. Everything else should prefer the configured or
/// stored value, because a `Host` header is the client's to choose and an
/// issuer that varies per request is not an issuer.
pub fn base_url_from(
    trust_proxy: bool,
    headers: &HeaderMap,
    scheme: Option<&str>,
) -> Option<String> {
    let host = if trust_proxy {
        leftmost(headers, "x-forwarded-host").or_else(|| header_str(headers, "host"))
    } else {
        header_str(headers, "host")
    }?;

    let scheme = if is_https(trust_proxy, headers, scheme) {
        "https"
    } else {
        "http"
    };

    Some(format!("{scheme}://{host}"))
}

/// [`base_url_from`], reading everything off an actix request.
///
/// actix does not put a scheme on the request line, so whether the listener
/// itself is a TLS one is what stands in for it. Deliberately not
/// `connection_info()`, which consults the forwarding headers whether or not a
/// proxy is trusted.
pub fn request_base_url(trust_proxy: bool, request: &HttpRequest) -> Option<String> {
    let secure = request.app_config().secure();

    base_url_from(trust_proxy, request.headers(), secure.then_some("https"))
}

#[cfg(test)]
mod tests {
    use actix_web::http::header::{HeaderName, HeaderValue};

    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();

        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }

        map
    }

    fn peer() -> Option<SocketAddr> {
        Some("203.0.113.9:51234".parse().unwrap())
    }

    #[test]
    fn a_forwarded_address_is_ignored_unless_a_proxy_is_trusted() {
        let headers = headers(&[("x-forwarded-for", "198.51.100.1")]);

        assert_eq!(
            client_ip(false, &headers, peer()).as_deref(),
            Some("203.0.113.9"),
            "a client that sends the header itself must not get to choose its address",
        );
        assert_eq!(
            client_ip(true, &headers, peer()).as_deref(),
            Some("198.51.100.1")
        );
    }

    #[test]
    fn the_left_most_entry_of_a_chain_is_the_client() {
        let headers = headers(&[("x-forwarded-for", "198.51.100.1, 203.0.113.7")]);

        assert_eq!(
            client_ip(true, &headers, peer()).as_deref(),
            Some("198.51.100.1")
        );
    }

    #[test]
    fn an_unparseable_forwarded_address_falls_back_to_the_socket() {
        // Otherwise every request carrying junk would share one rate-limit
        // bucket, which is a lockout somebody else gets to cause.
        let headers = headers(&[("x-forwarded-for", "not-an-address")]);

        assert_eq!(
            client_address(true, &headers, peer()),
            Some("203.0.113.9".parse().unwrap())
        );
    }

    #[test]
    fn a_plaintext_request_cannot_claim_to_have_been_secure() {
        let forwarded = headers(&[("x-forwarded-proto", "https")]);
        let chained = headers(&[("x-forwarded-proto", "https, http")]);

        assert!(!is_https(false, &forwarded, Some("http")));
        assert!(is_https(true, &forwarded, Some("http")));
        assert!(is_https(true, &chained, None));
    }

    #[test]
    fn the_base_url_is_rebuilt_from_the_host_and_the_scheme() {
        let headers = headers(&[("host", "tak.example.com:8446")]);

        assert_eq!(
            base_url_from(false, &headers, Some("https")).as_deref(),
            Some("https://tak.example.com:8446")
        );
        assert_eq!(
            base_url_from(false, &headers, None).as_deref(),
            Some("http://tak.example.com:8446")
        );
    }

    #[test]
    fn a_forwarded_host_is_only_believed_behind_a_trusted_proxy() {
        let headers = headers(&[
            ("host", "127.0.0.1:8446"),
            ("x-forwarded-host", "tak.example.com"),
            ("x-forwarded-proto", "https"),
        ]);

        assert_eq!(
            base_url_from(false, &headers, None).as_deref(),
            Some("http://127.0.0.1:8446")
        );
        assert_eq!(
            base_url_from(true, &headers, None).as_deref(),
            Some("https://tak.example.com")
        );
    }

    #[test]
    fn a_request_with_no_host_header_has_no_base_url() {
        assert_eq!(base_url_from(false, &HeaderMap::new(), Some("https")), None);
    }
}
