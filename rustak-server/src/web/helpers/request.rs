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

use crate::config::ServerConfig;

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

/// `https` or `http`, as the request and the listener between them say.
fn scheme_for(trust_proxy: bool, headers: &HeaderMap, scheme: Option<&str>) -> &'static str {
    if is_https(trust_proxy, headers, scheme) {
        "https"
    } else {
        "http"
    }
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

    Some(format!(
        "{}://{host}",
        scheme_for(trust_proxy, headers, scheme)
    ))
}

/// [`base_url_from`], reading everything off an actix request.
///
/// actix does not put a scheme on the request line, so whether the listener
/// itself is a TLS one is what stands in for it. Deliberately not
/// `connection_info()`, which consults the forwarding headers whether or not a
/// proxy is trusted.
///
/// # Why the URI is consulted as well as the headers
///
/// Over HTTP/1.1 the authority is the `Host` header. Over HTTP/2 there is no
/// such header at all: the authority travels in the `:authority` pseudo-header,
/// which actix puts on the request URI rather than into the header map — so a
/// header-only reading returns [`None`] for **every** h2 client, and our TLS
/// listeners advertise `h2` in ALPN, which is what ATAK takes. That is how a
/// mission package came to be advertised to a peer at `https://<display name>`
/// (CI-01), a host nothing can resolve.
pub fn request_base_url(trust_proxy: bool, request: &HttpRequest) -> Option<String> {
    let headers = request.headers();
    let scheme = request.app_config().secure().then_some("https");

    base_url_from(trust_proxy, headers, scheme).or_else(|| {
        let authority = request.uri().authority()?.as_str();

        Some(format!(
            "{}://{authority}",
            scheme_for(trust_proxy, headers, scheme)
        ))
    })
}

/// The base URL to hand to somebody else, which is never a display name when
/// this installation knows a real one.
///
/// The request comes first because it is the only thing that is right on an
/// installation reached on a host nobody has written down yet. After it,
/// `[server] base_url` and then the canonical `[server] domains` entry — both
/// of which are external URLs — and `[server] name` only when the operator has
/// configured neither, which is the pre-wizard state this used to fall into on
/// every h2 request.
pub fn public_base_url(config: &ServerConfig, request: &HttpRequest) -> String {
    request_base_url(config.trust_proxy, request)
        .or_else(|| config.base_url())
        .unwrap_or_else(|| format!("https://{}", config.name))
}

#[cfg(test)]
mod tests {
    use actix_web::http::header::{HeaderName, HeaderValue};
    use actix_web::test::TestRequest;

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

    /// An HTTP/2-shaped request: the authority on the URI, where actix puts
    /// `:authority`, and no `host` header anywhere.
    fn h2(uri: &str, pairs: &[(&str, &str)]) -> HttpRequest {
        let mut request = TestRequest::get().uri(uri);

        for (name, value) in pairs {
            request = request.insert_header((*name, *value));
        }

        request.to_http_request()
    }

    #[test]
    fn an_http_2_request_is_read_from_its_authority_rather_than_a_header() {
        // The bug this file exists to have fixed: every h2 client — which is
        // every ATAK, because the listeners advertise `h2` — used to come out
        // of here as `None`, and the caller then advertised a display name.
        let request = h2("https://tak.example.com:8446/Marti/sync/content", &[]);

        assert!(
            request.headers().get("host").is_none(),
            "the premise of this test is a request with no host header",
        );
        assert_eq!(
            request_base_url(false, &request).as_deref(),
            Some("http://tak.example.com:8446"),
            "the authority is the only thing an h2 request says about its host",
        );
    }

    #[test]
    fn an_http_1_request_still_uses_its_host_header() {
        // Both present and disagreeing is the absolute-form request line, which
        // RFC 9112 says the target wins over `Host` for — but this reading is
        // the one every other caller has depended on, and the authority is only
        // ever set by actix itself on h2, so the header stays in front.
        let request = h2(
            "https://authority.example.com/Marti/sync/content",
            &[("host", "tak.example.com:8446")],
        );

        assert_eq!(
            request_base_url(false, &request).as_deref(),
            Some("http://tak.example.com:8446")
        );
    }

    #[test]
    fn a_proxy_in_front_of_an_http_2_client_is_still_believed_first() {
        let request = h2(
            "https://internal.example.com/Marti/sync/content",
            &[
                ("x-forwarded-host", "tak.example.com"),
                ("x-forwarded-proto", "https"),
            ],
        );

        assert_eq!(
            request_base_url(false, &request).as_deref(),
            Some("http://internal.example.com"),
            "an untrusted client does not get to rename the server",
        );
        assert_eq!(
            request_base_url(true, &request).as_deref(),
            Some("https://tak.example.com")
        );
    }

    #[test]
    fn a_forwarded_scheme_applies_to_the_authority_reading_too() {
        // The h2 branch has to run the same `X-Forwarded-Proto` rule as the
        // header branch, or a proxied installation would advertise `http://`
        // URLs to peers that can only reach it over TLS.
        let request = h2(
            "https://tak.example.com/Marti/sync/content",
            &[("x-forwarded-proto", "https")],
        );

        assert_eq!(
            request_base_url(true, &request).as_deref(),
            Some("https://tak.example.com")
        );
    }

    #[test]
    fn a_request_that_names_no_host_at_all_has_no_base_url() {
        let request = TestRequest::get()
            .uri("/Marti/sync/content")
            .to_http_request();

        assert_eq!(request_base_url(false, &request), None);
    }

    #[test]
    fn a_public_url_falls_back_to_a_configured_url_before_a_display_name() {
        // CI-01: the old order went straight from "the request said nothing" to
        // `[server] name`, and handed a peer `https://rustak-interop-eud-…`.
        let request = TestRequest::get()
            .uri("/Marti/sync/content")
            .to_http_request();
        let named = ServerConfig {
            name: "Ops rustak".to_string(),
            ..ServerConfig::default()
        };

        assert_eq!(public_base_url(&named, &request), "https://Ops rustak");

        let with_domain = ServerConfig {
            domains: vec!["tak.example.com".to_string(), "tak.lan".to_string()],
            ..named.clone()
        };

        assert_eq!(
            public_base_url(&with_domain, &request),
            "https://tak.example.com",
        );

        let with_base_url = ServerConfig {
            base_url: Some("https://tak.example.com:8446/".to_string()),
            ..with_domain
        };

        assert_eq!(
            public_base_url(&with_base_url, &request),
            "https://tak.example.com:8446",
        );

        // And the request still wins over all of it, because it is the only
        // reading that is right on a host nobody has written down yet.
        let addressed = h2("https://tak.lan:8446/Marti/sync/content", &[]);

        assert_eq!(
            public_base_url(&with_base_url, &addressed),
            "http://tak.lan:8446",
        );
    }
}
