//! Which registered client is asking at `/oauth/token`, and whether it proved
//! it.
//!
//! A **public** client holds no secret and authenticates with nothing: its
//! proof key is what stands between an intercepted code and a session, and
//! [`mod@super::authorize`] refuses to start a flow for one without it. A
//! **confidential** client presents a secret here, and its proof key becomes
//! optional — which is the whole reason this module exists, because CloudTAK's
//! relying party sends no `code_challenge` at all.
//!
//! # The two ways a secret arrives
//!
//! `client_secret_post` puts `client_id` and `client_secret` in the form.
//! `client_secret_basic` puts them in `Authorization: Basic`, where RFC 6749
//! §2.3.1 says both halves are form-encoded first. The header wins when both
//! are present, and a request whose two `client_id`s disagree is refused rather
//! than resolved in either direction — it is either a confused client or
//! somebody hoping one half is read and the other checked.
//!
//! # What is compared, and how
//!
//! The registered secret and the presented one, in constant time, and never
//! logged, traced or put in an error. A wrong secret, an empty one and a
//! missing one are the same refusal: telling them apart is an oracle for which
//! identifiers are registered as confidential.

use actix_web::http::header::{AUTHORIZATION, HeaderMap};
use base64::Engine as _;

use crate::config::OAuthClient;
use crate::prelude::*;

use super::constant_time_eq;

/// What a token request said about which client is asking, and with what.
#[derive(Clone, PartialEq, Eq)]
pub struct Presented {
    /// The identifier the request named.
    pub client_id: String,

    /// The secret it presented, if any.
    secret: Option<String>,
}

impl Presented {
    /// The secret this request presented, for the one comparison that reads it.
    fn secret(&self) -> Option<&str> {
        self.secret.as_deref()
    }
}

impl std::fmt::Debug for Presented {
    /// Written out rather than derived: this struct is the one place a client
    /// secret is held in memory on the token path, and a `{:?}` of it in a log
    /// line or a `dbg!` left behind would be that secret in a log file.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Presented")
            .field("client_id", &self.client_id)
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Which client a token request names, and what it presented for it.
///
/// [`None`] when no identifier was given at all, or when the header and the
/// form disagree about which client is asking.
pub fn presented(
    headers: &HeaderMap,
    form_client_id: Option<&str>,
    form_secret: Option<&str>,
) -> Option<Presented> {
    let Some((header_id, header_secret)) = basic(headers) else {
        return Some(Presented {
            client_id: form_client_id?.to_string(),
            secret: form_secret.map(str::to_string),
        });
    };

    // Not "whichever one is right": a request naming two clients is refused, so
    // that neither half can be the one read while the other is the one checked.
    if form_client_id.is_some_and(|form| form != header_id) {
        debug!("Refused a token request whose header and form named different clients.");

        return None;
    }

    Some(Presented {
        client_id: header_id,
        secret: Some(header_secret),
    })
}

/// Whether `client` accepts what this request presented.
///
/// A public client authenticates with nothing, so a secret it was never
/// registered with is ignored rather than refused — the configuration already
/// refuses to register one, and the flow's security rests on the proof key.
pub fn authenticates(client: &OAuthClient, presented: &Presented) -> bool {
    if client.public {
        return true;
    }

    let Some(registered) = client.secret.as_deref() else {
        // Unreachable through `--check`, which refuses a confidential client
        // with no secret, and a refusal rather than a hole if it ever is.
        error!(
            client = %client.id,
            "A confidential client is registered with no secret; no request can authenticate as it.",
        );

        return false;
    };

    presented
        .secret()
        .is_some_and(|value| constant_time_eq(registered, value))
}

/// The `client_id` and `client_secret` of an `Authorization: Basic` header.
///
/// RFC 6749 §2.3.1 form-encodes both halves before base64. The identifier is
/// decoded, because it is only ever compared with a name from the
/// configuration; the **secret** is returned raw, because a secret containing a
/// `+` sent by a client that did not encode it would otherwise decode to a
/// space and fail for a reason nobody could see. Operators who put a `%` or a
/// `+` in a secret and a conforming library on the other end are the one case
/// this gets wrong, and `client_secret_post` is unaffected either way.
fn basic(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let encoded = value
        .strip_prefix("Basic ")
        .or_else(|| value.strip_prefix("basic "))?
        .trim();

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (id, secret) = text.split_once(':')?;

    Some((form_decode(id), secret.to_string()))
}

/// Percent-decodes one form-encoded value.
///
/// `url` has no single-value decoder, so the value is parsed as a one-field
/// body. A value carrying a raw `&` or `=` — which a conforming client would
/// have encoded — is left exactly as it arrived rather than being truncated at
/// the character that would have split the body.
fn form_decode(value: &str) -> String {
    if value.contains(['&', '=']) {
        return value.to_string();
    }

    url::form_urlencoded::parse(format!("v={value}").as_bytes())
        .find(|(key, _)| key == "v")
        .map(|(_, decoded)| decoded.into_owned())
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    fn client(public: bool, secret: Option<&str>) -> OAuthClient {
        OAuthClient {
            id: "app".to_string(),
            redirect_uris: vec!["https://app.example.com/cb".to_string()],
            public,
            secret: secret.map(str::to_string),
            post_logout_redirect_uris: Vec::new(),
        }
    }

    fn headers(authorization: Option<&str>) -> HeaderMap {
        let mut request = TestRequest::post();

        if let Some(value) = authorization {
            request = request.insert_header((AUTHORIZATION, value));
        }

        request.to_http_request().headers().clone()
    }

    fn basic_header(id: &str, secret: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{id}:{secret}"))
        )
    }

    #[test]
    fn a_form_names_the_client_when_no_header_does() {
        let read = presented(&headers(None), Some("app"), Some("s3cret")).unwrap();

        assert_eq!(read.client_id, "app");
        assert_eq!(read.secret(), Some("s3cret"));
    }

    #[test]
    fn a_request_naming_no_client_at_all_is_nobody() {
        assert_eq!(presented(&headers(None), None, Some("s3cret")), None);
    }

    #[test]
    fn a_basic_header_is_read_and_beats_the_form() {
        let read = presented(
            &headers(Some(&basic_header("app", "s3cret"))),
            Some("app"),
            Some("something-else"),
        )
        .unwrap();

        assert_eq!(read.client_id, "app");
        assert_eq!(read.secret(), Some("s3cret"));
    }

    #[test]
    fn a_request_naming_two_different_clients_is_refused_rather_than_resolved() {
        // Otherwise one half could be the one read and the other the one
        // checked, which is the shape of every confused-deputy bug.
        assert_eq!(
            presented(
                &headers(Some(&basic_header("app", "s3cret"))),
                Some("another-app"),
                None,
            ),
            None,
        );
    }

    #[test]
    fn a_form_encoded_identifier_comes_back_decoded() {
        let read = presented(
            &headers(Some(&basic_header("a%20client", "s3cret"))),
            None,
            None,
        )
        .unwrap();

        assert_eq!(read.client_id, "a client");
    }

    #[test]
    fn a_header_that_is_not_basic_leaves_the_form_to_answer() {
        for value in ["Bearer a-token", "Basic not-base64", "Basic"] {
            let read = presented(&headers(Some(value)), Some("app"), Some("s3cret"));

            assert_eq!(
                read.map(|read| read.client_id),
                Some("app".to_string()),
                "{value}",
            );
        }
    }

    #[test]
    fn a_confidential_client_needs_its_own_secret_and_nothing_else_will_do() {
        let client = client(false, Some("s3cret"));

        assert!(authenticates(
            &client,
            &Presented {
                client_id: "app".to_string(),
                secret: Some("s3cret".to_string()),
            },
        ));

        for wrong in [
            None,
            Some(""),
            Some("s3crat"),
            Some("s3cret "),
            Some("S3CRET"),
        ] {
            assert!(
                !authenticates(
                    &client,
                    &Presented {
                        client_id: "app".to_string(),
                        secret: wrong.map(str::to_string),
                    },
                ),
                "{wrong:?}",
            );
        }
    }

    #[test]
    fn a_confidential_client_registered_with_no_secret_authenticates_nobody() {
        // `--check` refuses that configuration; this is the refusal rather than
        // the hole if one ever reaches a running server.
        assert!(!authenticates(
            &client(false, None),
            &Presented {
                client_id: "app".to_string(),
                secret: None,
            },
        ));
    }

    #[test]
    fn a_public_client_authenticates_with_nothing() {
        // Its flow rests on the proof key instead, which `authorize` makes
        // mandatory for exactly this reason.
        for secret in [None, Some("a-secret-it-was-never-registered-with")] {
            assert!(authenticates(
                &client(true, None),
                &Presented {
                    client_id: "app".to_string(),
                    secret: secret.map(str::to_string),
                },
            ));
        }
    }

    #[test]
    fn a_secret_never_appears_in_a_debug_dump() {
        let read = Presented {
            client_id: "app".to_string(),
            secret: Some("s3cret-value".to_string()),
        };

        assert!(!format!("{read:?}").contains("s3cret-value"), "{read:?}");
    }
}
