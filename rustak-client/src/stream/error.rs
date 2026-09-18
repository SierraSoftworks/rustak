//! What can go wrong on a TAK stream, and what the operator should do about it.
//!
//! [`StreamError`] is a plain enum rather than a [`human_errors::Error`]
//! because it is the `Err` half of a [`Stream`](futures::Stream) item: the
//! reconnect loop matches on it, and only the code that finally gives up needs
//! the operator-facing rendering. [`From<StreamError>`] supplies that rendering,
//! with the advice attached, so a sidecar that bubbles one out of `start` still
//! reports it the way every other rustak failure is reported.

use rustak_cot::error::CodecError;

/// Advice for a connect string we could not read.
pub const ADVICE_CONNECT_STRING: &[&str] = &[
    "Write the server as 'ssl://host:8089', or as ATAK's own 'host:8089:ssl' connect string.",
    "Only TLS endpoints are available; plain TCP is a test-only build of this client.",
];

/// Advice for certificate material we could not load or use.
pub const ADVICE_TLS_MATERIAL: &[&str] = &[
    "Check that the certificate, key and truststore paths exist and hold PEM this process may read.",
    "Enrol the device again if its certificate has been replaced, revoked or has expired.",
];

/// Advice for a connection that would not come up, or did not stay up.
pub const ADVICE_CONNECTIVITY: &[&str] = &[
    "Check that the server is reachable on the port in your connect string and that its certificate is trusted.",
    "The client reconnects with backoff on its own, so a single drop is not something to act on.",
];

/// Anything that ends, or prevents, a TAK stream connection.
#[derive(Debug)]
#[non_exhaustive]
pub enum StreamError {
    /// A connect string we could not read, or one naming a transport this
    /// build does not have.
    Endpoint(String),

    /// Certificate, key or truststore material we could not load or use.
    Identity(String),

    /// The transport failed: the socket, the TLS handshake, or the peer.
    Io(std::io::Error),

    /// The peer's bytes could not be framed or parsed at all.
    ///
    /// Note that a *single* unframeable message never reaches here — the codec
    /// consumes it and reports it through its drop counters (see
    /// [`TakStream::dropped`](super::TakStream::dropped)). This variant means
    /// the transport itself failed underneath the codec.
    Codec(CodecError),

    /// Nothing at all was heard for the keepalive's dead interval (25 s for
    /// ATAK's constants). The connection is gone even if the socket has not
    /// noticed; reconnect.
    RxTimeout,

    /// Something did not happen in the time allowed, naming what was waited on.
    Timeout(String),

    /// Something arrived that a test harness was asserting would not.
    Unexpected(String),
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Endpoint(what) => write!(
                formatter,
                "We could not use {what} as a TAK server address."
            ),
            Self::Identity(what) => write!(
                formatter,
                "We could not use the TLS material for this connection: {what}."
            ),
            Self::Io(source) => write!(
                formatter,
                "The connection to the TAK server failed: {source}."
            ),
            Self::Codec(source) => write!(
                formatter,
                "The TAK server sent something we could not read: {source}."
            ),
            Self::RxTimeout => write!(
                formatter,
                "The TAK server stopped responding, so we are treating the connection as dead.",
            ),
            Self::Timeout(what) => write!(
                formatter,
                "We waited for {what} and it did not happen in time."
            ),
            Self::Unexpected(what) => write!(
                formatter,
                "We received {what} while asserting that nothing would arrive."
            ),
        }
    }
}

impl std::error::Error for StreamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::Codec(source) => Some(source),
            _ => None,
        }
    }
}

impl From<std::io::Error> for StreamError {
    fn from(source: std::io::Error) -> Self {
        Self::Io(source)
    }
}

impl From<CodecError> for StreamError {
    fn from(source: CodecError) -> Self {
        match source {
            CodecError::Io(io) => Self::Io(io),
            other => Self::Codec(other),
        }
    }
}

impl From<StreamError> for human_errors::Error {
    /// Renders the failure the way an operator should see it.
    ///
    /// The split is the usual one: a `Kind::User` error names something in the
    /// configuration or the network that somebody can go and fix, and a
    /// `Kind::System` error is ours. A peer we cannot frame is the only one of
    /// these that is a bug rather than a condition.
    fn from(error: StreamError) -> Self {
        let message = error.to_string();

        match error {
            StreamError::Endpoint(_) => human_errors::user(message, ADVICE_CONNECT_STRING),
            StreamError::Identity(_) => human_errors::user(message, ADVICE_TLS_MATERIAL),
            StreamError::Codec(_) => {
                human_errors::system(message, rustak_core::errors::ADVICE_REPORT_DEV)
            }
            _ => human_errors::user(message, ADVICE_CONNECTIVITY),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_advice_slice_gives_the_reader_something_to_do() {
        for advice in [
            ADVICE_CONNECT_STRING,
            ADVICE_TLS_MATERIAL,
            ADVICE_CONNECTIVITY,
        ] {
            assert!(!advice.is_empty());
            for line in advice {
                assert!(
                    line.ends_with('.'),
                    "advice should read as a sentence: {line:?}"
                );
                assert!(line.len() > 20, "advice should be an instruction: {line:?}");
            }
        }
    }

    #[test]
    fn a_transport_failure_inside_the_codec_is_reported_as_a_transport_failure() {
        // `rustak_cot::codec` wraps io errors in its own enum; a socket that
        // died is not a protocol bug and must not be reported to telemetry as
        // one.
        let wrapped = StreamError::from(CodecError::Io(std::io::Error::other("reset")));

        assert!(matches!(wrapped, StreamError::Io(_)), "{wrapped:?}");
        assert!(human_errors::Error::from(wrapped).is(human_errors::Kind::User));
    }

    #[test]
    fn a_bad_connect_string_is_the_operators_to_fix() {
        let rendered = human_errors::Error::from(StreamError::Endpoint("udp://host:1".into()));

        assert!(rendered.is(human_errors::Kind::User));
        assert!(rendered.to_string().contains("udp://host:1"), "{rendered}");
    }

    #[test]
    fn a_dead_connection_says_so_without_a_source_to_chase() {
        let error = StreamError::RxTimeout;

        assert!(std::error::Error::source(&error).is_none());
        assert!(error.to_string().contains("dead"), "{error}");
    }
}
