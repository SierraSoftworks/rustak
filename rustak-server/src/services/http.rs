//! Reading an outbound response body without trusting how long it is.
//!
//! `Response::json()` and `Response::text()` read until the peer stops sending.
//! That is fine for an endpoint we control and wrong for every endpoint in this
//! server: an identity provider, an ACME directory and a plugin's webhook are
//! all *configured* rather than trusted, and a compromised or merely broken one
//! can stream bytes into the server's memory for as long as `[server]
//! http_timeout` allows. The cap here is what stops it.
//!
//! The `Content-Length` is checked first because it costs nothing, and then the
//! body is accumulated a chunk at a time so that a response which lies about
//! its length — or does not declare one — is stopped at the same limit.

use std::fmt;

use bytes::Bytes;

/// How much of a configured endpoint's JSON response we will read.
///
/// Generous for the documents this is used on: an OpenID discovery document is
/// a couple of kilobytes and a JWKS with a dozen keys is under ten.
pub const MAX_JSON_BYTES: u64 = 256 * 1024;

/// Why a response body could not be read.
#[derive(Debug)]
pub enum BodyError {
    /// The peer sent, or declared, more than the cap allows.
    TooLarge {
        /// The cap it went past.
        limit: u64,
    },
    /// The transfer failed part-way through.
    Transport(reqwest::Error),
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => {
                write!(f, "the response was longer than the {limit} bytes we read")
            }
            Self::Transport(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for BodyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TooLarge { .. } => None,
            Self::Transport(err) => Some(err),
        }
    }
}

/// Reads a response body, refusing one longer than `limit`.
///
/// # Errors
///
/// [`BodyError::TooLarge`] when the body declares or reaches more than `limit`
/// bytes, and [`BodyError::Transport`] when the transfer fails.
pub async fn body_within(mut response: reqwest::Response, limit: u64) -> Result<Bytes, BodyError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(BodyError::TooLarge { limit });
    }

    let mut body = Vec::new();

    while let Some(chunk) = response.chunk().await.map_err(BodyError::Transport)? {
        if body.len() as u64 + chunk.len() as u64 > limit {
            return Err(BodyError::TooLarge { limit });
        }

        body.extend_from_slice(&chunk);
    }

    Ok(Bytes::from(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn respond(body: Vec<u8>) -> reqwest::Response {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/thing"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        reqwest::Client::new()
            .get(format!("{}/thing", server.uri()))
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_body_inside_the_cap_is_returned_whole() {
        let body = body_within(respond(b"{\"ok\":true}".to_vec()).await, 64)
            .await
            .unwrap();

        assert_eq!(&body[..], b"{\"ok\":true}");
    }

    #[tokio::test]
    async fn a_body_past_the_cap_is_refused_rather_than_buffered() {
        // A compromised or merely broken provider streaming into our memory.
        let err = body_within(respond(vec![b'x'; 4096]).await, 64)
            .await
            .expect_err("an oversize body should be refused");

        assert!(matches!(err, BodyError::TooLarge { limit: 64 }), "{err}");
    }
}
