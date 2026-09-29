//! What a failed request says to the operator reading about it.
//!
//! # Advice has to fit the failure
//!
//! One advice block used to follow every transport failure, and it led with two
//! truststore hints. The Dublin deployment's log shows what that costs: a plain
//! `tcp connect error: deadline has elapsed` during a redeploy, followed by
//! advice about certificates that could not have had anything to do with it.
//!
//! So a failure is first put in a [`Class`], found by walking its cause chain
//! with typed checks — `rustls::Error`, `std::io::ErrorKind`, `reqwest`'s own
//! `is_*` predicates — and the advice is chosen from the class. The truststore
//! hints belong to [`Class::Tls`] alone.
//!
//! The one check that is not typed is name resolution: the resolver's error
//! types are private to `reqwest` and `hyper-util`, so a chain that did not come
//! through a `reqwest::Error` (whose `is_dns` *is* typed) is matched on the
//! words those two crates put on it.

use std::error::Error as StdError;
use std::io;

use rustak_core::prelude::*;
use url::Url;

use super::full_stop;

/// How many links of a transport error's cause chain are rendered and walked.
///
/// A handshake failure is three deep; anything longer is a chain that has
/// started repeating itself.
const MAX_CAUSES: usize = 6;

/// Advice for a host name that did not resolve.
const ADVICE_RESOLVE: &[&str] = &[
    "The server's host name did not resolve. Check it for typos under [server], and that this host's DNS can resolve it.",
    "Inside a container, the name has to resolve on the container's network, which is not always the host's.",
];

/// Advice for a connection the server's host refused.
const ADVICE_REFUSED: &[&str] = &[
    "Nothing accepted the connection at that address and port. Check that the rustak server is running and that [server] names the right port.",
    "While the server is being restarted or redeployed this is expected, and the sidecar keeps retrying.",
];

/// Advice for a connection that was never answered.
const ADVICE_CONNECT_TIMEOUT: &[&str] = &[
    "The server did not answer the connection in time. Check that [server] names the right address, and that no firewall or security group is dropping the traffic.",
    "A server that is restarting can look like this for a moment, and the sidecar keeps retrying.",
];

/// Advice for a network the server is not on.
const ADVICE_UNREACHABLE: &[&str] = &[
    "There is no route to the server. Check this host's network, and that [server] names an address it can reach.",
];

/// Advice for a handshake that failed.
const ADVICE_TLS: &[&str] = &[
    "The TLS handshake failed: the server presented a certificate this sidecar does not trust, or the two could not agree on how to talk.",
    "[service] truststore verifies the CoT stream and [server] marti; [server] control is verified against the platform's roots *and* that truststore, unless [service] control_truststore pins it to a PKI of your own.",
];

/// Advice for a server that took the connection and never answered.
const ADVICE_TIMEOUT: &[&str] = &[
    "The server accepted the connection but did not answer in time. Check its health and its logs for the same moment.",
];

/// Advice for an answer that carried an error status.
const ADVICE_STATUS: &[&str] =
    &["The server answered with an error status; its logs say why it refused."];

/// Advice for an answer that broke off or could not be read.
const ADVICE_BODY: &[&str] = &[
    "The server's answer broke off or could not be decoded. A proxy between this sidecar and the server that cuts or rewrites responses can cause this.",
    "Check the server's logs, and any proxy's, for the same moment.",
];

/// Advice for a failure none of the classes above describes.
const ADVICE_OTHER: &[&str] =
    &["Check that the server is reachable and that [server] names the right address."];

/// Every class that means the server was never reached, for [`is_transport`].
const OUTAGES: &[&[&str]] = &[
    ADVICE_RESOLVE,
    ADVICE_REFUSED,
    ADVICE_CONNECT_TIMEOUT,
    ADVICE_UNREACHABLE,
    ADVICE_TLS,
    ADVICE_TIMEOUT,
    ADVICE_BODY,
    ADVICE_OTHER,
];

/// What kind of failure a request met, which is what its advice is chosen by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// The host name did not resolve.
    Resolve,
    /// The host answered the connection with a refusal.
    Refused,
    /// The connection was never answered.
    ConnectTimeout,
    /// There is no route to the host.
    Unreachable,
    /// The TLS handshake failed.
    Tls,
    /// The connection was made and the answer did not come in time.
    Timeout,
    /// The server answered with an error status.
    Status,
    /// The answer broke off, or could not be decoded.
    Body,
    /// None of the above.
    Other,
}

impl Class {
    /// The advice an operator gets for a failure of this class.
    pub(crate) fn advice(self) -> &'static [&'static str] {
        match self {
            Self::Resolve => ADVICE_RESOLVE,
            Self::Refused => ADVICE_REFUSED,
            Self::ConnectTimeout => ADVICE_CONNECT_TIMEOUT,
            Self::Unreachable => ADVICE_UNREACHABLE,
            Self::Tls => ADVICE_TLS,
            Self::Timeout => ADVICE_TIMEOUT,
            Self::Status => ADVICE_STATUS,
            Self::Body => ADVICE_BODY,
            Self::Other => ADVICE_OTHER,
        }
    }
}

/// The class of a failure, from its cause chain.
///
/// A TLS error anywhere in the chain wins, because a handshake runs inside the
/// connector and `reqwest` calls it a connect error too. After that the
/// `reqwest` error's own kind decides whether the failure was in getting a
/// connection at all — in which case the chain's I/O error says how — or in
/// what came back over one.
pub(crate) fn classify(err: &(dyn StdError + 'static)) -> Class {
    let links = links(err);
    let chain = || links.iter().copied();
    let request = err.downcast_ref::<reqwest::Error>();

    if chain().any(|link| link.is::<rustls::Error>()) {
        return Class::Tls;
    }

    if request.is_some_and(reqwest::Error::is_status) {
        return Class::Status;
    }

    if request.is_some_and(|request| request.is_body() || request.is_decode()) {
        return Class::Body;
    }

    // A chain that did not come through `reqwest` is taken to be a connector's.
    let connecting = request.is_none_or(reqwest::Error::is_connect);
    let timed_out = request.is_some_and(reqwest::Error::is_timeout);

    if connecting {
        if request.is_some_and(reqwest::Error::is_dns) || chain().any(is_resolve) {
            return Class::Resolve;
        }

        if let Some(class) = chain().find_map(connect_class) {
            return class;
        }

        if timed_out {
            return Class::ConnectTimeout;
        }
    } else if timed_out {
        return Class::Timeout;
    }

    Class::Other
}

/// Every link of a cause chain, including the ones an [`io::Error`] hides.
///
/// `tokio-rustls` hands a handshake failure up inside an `io::Error`, which
/// `hyper-util` wraps in another, and `io::Error`'s own `source` skips past the
/// error it carries — so a walk that only followed `source` would never meet
/// the `rustls::Error` at the bottom. Each `io::Error` is looked inside too.
fn links<'a>(err: &'a (dyn StdError + 'static)) -> Vec<&'a (dyn StdError + 'static)> {
    let mut links = Vec::new();
    let mut next = Some(err);

    while let Some(link) = next {
        if links.len() > MAX_CAUSES * 2 {
            break;
        }

        links.push(link);

        next = match link
            .downcast_ref::<io::Error>()
            .and_then(io::Error::get_ref)
        {
            Some(inner) => Some(inner as &(dyn StdError + 'static)),
            None => link.source(),
        };
    }

    links
}

/// Whether this link reads as a resolver's failure — see the module
/// documentation for why this one check is on words.
fn is_resolve(link: &(dyn StdError + 'static)) -> bool {
    let text = link.to_string();

    text.starts_with("dns error") || text.starts_with("error resolving DNS")
}

/// What an I/O error met while connecting says about the connection.
fn connect_class(link: &(dyn StdError + 'static)) -> Option<Class> {
    if link.is::<tokio::time::error::Elapsed>() {
        return Some(Class::ConnectTimeout);
    }

    match link.downcast_ref::<io::Error>()?.kind() {
        io::ErrorKind::ConnectionRefused => Some(Class::Refused),
        io::ErrorKind::TimedOut => Some(Class::ConnectTimeout),
        io::ErrorKind::HostUnreachable
        | io::ErrorKind::NetworkUnreachable
        | io::ErrorKind::NetworkDown
        | io::ErrorKind::AddrNotAvailable => Some(Class::Unreachable),
        _ => None,
    }
}

/// Whether this is an error [`transport`] built for a server that was never
/// reached.
///
/// A refusal carrying a status is the server *answering* — the link is up,
/// however unwelcome the answer — and only a failure to reach it at all is an
/// outage. The harness uses this to decide what a failed call says about the
/// control link as a whole, so that one endpoint answering `404` does not stop
/// a sidecar heartbeating into another that is perfectly well.
///
/// Matched on the advice rather than on the message, because the message names
/// whatever the caller happened to be doing and the advice survives being
/// wrapped by a caller that adds its own. Every outage class leads with a line
/// no other class uses, and that line is the marker.
#[must_use]
pub fn is_transport(err: &Error) -> bool {
    let advice = err.advice();

    OUTAGES
        .iter()
        .filter_map(|class| class.first())
        .any(|marker| advice.contains(marker))
}

/// Turns a transport failure into something an operator can act on.
///
/// Kept in one place because the modules that make requests would otherwise each
/// invent their own phrasing for "the server did not answer" — and because
/// `reqwest`'s own words for a failed handshake are "error sending request for
/// url (…)", which is the shape a production outage was mistaken for a network
/// problem in. The *cause* is the answer, so the whole chain is rendered, and
/// the advice is the advice for that kind of cause.
pub fn transport(err: reqwest::Error, what: &str) -> Error {
    let class = classify(&err);
    let detail = rendered(err);

    human_errors::user(
        format!("Could not {what}: {detail}{}", full_stop(&detail)),
        class.advice(),
    )
}

/// A transport failure and every cause under it, on one line.
///
/// The URL is rebuilt from [`reqwest::Error::url`] with its userinfo and query
/// removed rather than taken from `reqwest`'s own `Display`, because neither a
/// credential nor a header may reach a log line. Nothing in the chain is a
/// header: the chain below a request error is the connector's, and the deepest
/// link of a handshake failure is `rustls`'s own reason.
fn rendered(err: reqwest::Error) -> String {
    let url = err.url().map(redacted);
    // Rendering our own URL, so `reqwest`'s copy of it would only repeat.
    let err = err.without_url();

    let mut message = match url {
        Some(url) => format!("{err} for url ({url})"),
        None => err.to_string(),
    };

    let mut source = StdError::source(&err);

    for _ in 0..MAX_CAUSES {
        let Some(cause) = source else { break };
        let text = cause.to_string();

        // hyper wraps its connector error in one with the same words.
        if !message.ends_with(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }

        source = cause.source();
    }

    message
}

/// A URL with everything a credential could hide in taken out of it.
fn redacted(url: &Url) -> String {
    let mut url = url.clone();

    // Each answers `Err(())` for a URL that cannot hold the part being cleared,
    // which is the same thing as it already being clear.
    let _ = url.set_password(None);
    let _ = url.set_username("");
    url.set_query(None);
    url.set_fragment(None);

    url.to_string()
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use tokio::io::AsyncWriteExt;

    use super::*;

    /// One link of a constructed chain: its own words and what caused it, the
    /// way `reqwest`, `hyper` and `hyper-util` each wrap the layer below.
    #[derive(Debug)]
    struct Link {
        text: &'static str,
        cause: Box<dyn StdError + Send + Sync>,
    }

    impl fmt::Display for Link {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.text)
        }
    }

    impl StdError for Link {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            Some(self.cause.as_ref())
        }
    }

    /// `client error (Connect)` over `<text>` over `cause` — the shape a
    /// connector failure reaches us in.
    fn connector(text: &'static str, cause: impl StdError + Send + Sync + 'static) -> Link {
        Link {
            text: "client error (Connect)",
            cause: Box::new(Link {
                text,
                cause: Box::new(cause),
            }),
        }
    }

    fn mentions_a_truststore(class: Class) -> bool {
        class
            .advice()
            .iter()
            .any(|line| line.contains("truststore"))
    }

    #[test]
    fn the_dublin_connect_timeout_is_a_connect_timeout_and_says_nothing_about_certificates() {
        // The production line: `tcp connect error: deadline has elapsed`, then
        // two truststore hints that could not have applied to it.
        let err = connector(
            "tcp connect error",
            io::Error::new(io::ErrorKind::TimedOut, "deadline has elapsed"),
        );

        let class = classify(&err);

        assert_eq!(class, Class::ConnectTimeout);
        assert!(!mentions_a_truststore(class), "{:?}", class.advice());
    }

    #[test]
    fn each_connect_failure_is_told_apart_by_its_io_kind() {
        for (kind, expected) in [
            (io::ErrorKind::ConnectionRefused, Class::Refused),
            (io::ErrorKind::TimedOut, Class::ConnectTimeout),
            (io::ErrorKind::HostUnreachable, Class::Unreachable),
            (io::ErrorKind::NetworkUnreachable, Class::Unreachable),
        ] {
            let err = connector("tcp connect error", io::Error::from(kind));

            assert_eq!(classify(&err), expected, "{kind:?}");
        }
    }

    #[test]
    fn a_name_that_did_not_resolve_is_a_resolution_failure() {
        let err = connector(
            "dns error",
            io::Error::other("failed to lookup address information"),
        );

        assert_eq!(classify(&err), Class::Resolve);
    }

    #[test]
    fn a_rustls_refusal_is_tls_even_inside_the_io_error_that_carries_it() {
        // `io::Error::source` skips the error it wraps, so a walk that only
        // followed `source` would never see this one.
        // The shape `reqwest` 0.13 really produces: two of them, nested.
        let err = connector(
            "tls handshake",
            io::Error::other(io::Error::new(
                io::ErrorKind::InvalidData,
                rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer),
            )),
        );

        assert_eq!(classify(&err), Class::Tls);
    }

    #[test]
    fn only_a_tls_failure_is_advised_about_truststores() {
        for class in [
            Class::Resolve,
            Class::Refused,
            Class::ConnectTimeout,
            Class::Unreachable,
            Class::Timeout,
            Class::Status,
            Class::Body,
            Class::Other,
        ] {
            assert!(!mentions_a_truststore(class), "{class:?}");
            assert!(!class.advice().is_empty(), "{class:?} has advice");
        }

        assert!(mentions_a_truststore(Class::Tls));
        assert!(
            Class::Tls
                .advice()
                .iter()
                .any(|line| line.contains("control_truststore")),
            "the TLS advice names the way out",
        );
    }

    #[test]
    fn every_class_leads_with_advice_of_its_own() {
        // The first line is the marker `is_transport` matches on, so two
        // classes sharing one would make a refusal read as an outage.
        let firsts: Vec<_> = OUTAGES
            .iter()
            .chain([&ADVICE_STATUS])
            .filter_map(|advice| advice.first())
            .collect();
        let unique: std::collections::HashSet<_> = firsts.iter().collect();

        assert_eq!(firsts.len(), unique.len());
    }

    #[test]
    fn only_a_failure_to_reach_the_server_counts_as_one() {
        // What the harness reads to decide whether the control link is down: a
        // refusal is the server answering, and an answer is not an outage.
        let refused = human_errors::user(
            "Could not register the service 'weather': 409 Conflict.",
            &["Another account already holds that service name. Choose another."],
        );
        let status = human_errors::user("Could not read it.", Class::Status.advice());

        assert!(!is_transport(&refused), "a status is an answer");
        assert!(!is_transport(&status), "and so is one reqwest reported");

        for class in [
            Class::Refused,
            Class::ConnectTimeout,
            Class::Tls,
            Class::Other,
        ] {
            let err = human_errors::user("Could not reach it.", class.advice());

            assert!(is_transport(&err), "{class:?}");
        }
    }

    #[test]
    fn a_wrapped_transport_failure_is_still_one() {
        // `enrolment::ensure` wraps what `enroll` answered; the classification
        // has to survive that or a wrapped outage reads as a refusal.
        let inner = human_errors::user("Could not reach it.", Class::Refused.advice());
        let wrapped = human_errors::wrap_user(
            inner,
            "Could not enrol 'svc.weather'.",
            &["The sidecar does not start without an identity, so this is fatal."],
        );

        assert!(is_transport(&wrapped));
    }

    #[test]
    fn a_rendered_url_carries_no_credential_and_no_query() {
        // `reqwest`'s own Display prints the URL verbatim, userinfo and all.
        let url = Url::parse("https://svc:hunter2@tak.example.com:8446/oauth/token?assertion=ey.J")
            .unwrap();

        assert_eq!(redacted(&url), "https://tak.example.com:8446/oauth/token");
    }

    #[tokio::test]
    async fn a_port_nothing_listens_on_is_a_refusal_with_advice_to_match() {
        // Bound and released: the port is ours, and nothing is on it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);

        let err = reqwest::Client::new()
            .get(format!("http://{address}/api/v1/services"))
            .send()
            .await
            .expect_err("nothing is listening");

        assert_eq!(classify(&err), Class::Refused, "{err:?}");

        let rendered = transport(err, "read the services");

        assert!(is_transport(&rendered), "a refusal to connect is an outage");
        assert!(
            !rendered
                .advice()
                .iter()
                .any(|line| line.contains("truststore")),
            "{:?}",
            rendered.advice(),
        );
    }

    #[tokio::test]
    async fn an_error_status_is_classed_as_the_server_answering() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = reqwest::get(server.uri())
            .await
            .unwrap()
            .error_for_status()
            .expect_err("a 503");

        assert_eq!(classify(&err), Class::Status);
        assert!(!is_transport(&transport(err, "read it")));
    }

    #[tokio::test]
    async fn an_answer_that_will_not_decode_is_a_body_failure() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let err = reqwest::get(server.uri())
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .expect_err("not JSON");

        assert_eq!(classify(&err), Class::Body);
    }

    #[tokio::test]
    async fn an_answer_that_breaks_off_is_a_body_failure() {
        // A proxy cutting a response: the headers promise more than arrives.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\nshort")
                .await;
            let _ = stream.shutdown().await;
        });

        let err = reqwest::get(format!("http://{address}/"))
            .await
            .unwrap()
            .text()
            .await
            .expect_err("the body is cut short");

        server.abort();

        assert_eq!(classify(&err), Class::Body, "{err:?}");
    }
}
