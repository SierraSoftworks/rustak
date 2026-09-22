//! The credential half of OpenSky: a client secret, the bearer token it buys,
//! and how often either of those is worth mentioning.
//!
//! Client credentials are exchanged for a token that lasts half an hour and is
//! replaced when it is nearly out or when a request comes back `401`. The
//! secret is a [`Secret`] from the settings file to the form body, so it
//! redacts itself in every log line and every `Debug` — including the ones this
//! module does not write.
//!
//! # A token endpoint that is down is not a dark feed
//!
//! OpenSky's anonymous tier answers the same endpoint at a coarser resolution,
//! so a renewal that fails costs resolution rather than aircraft. It is
//! therefore a warning and not an error — and, because a credential the server
//! will never take renews on *every* poll, a warning said once: the run is
//! announced, the repeats are `debug`, and a reminder every five minutes says
//! how many there have been.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

use crate::sources::notice::{Repeated, Report, humanised};

/// Where a client credential becomes a bearer token.
pub const TOKEN_URL: &str =
    "https://auth.opensky-network.org/auth/realms/opensky-network/protocol/openid-connect/token";

/// How long before a token expires it is replaced.
const TOKEN_SKEW: Duration = Duration::from_secs(60);

/// How long a token lasts when the response does not say.
const TOKEN_DEFAULT_LIFETIME: Duration = Duration::from_secs(1_800);

/// Advice for a credential OpenSky would not take.
pub const ADVICE_CREDENTIAL: &[&str] = &[
    "Create an API client under your OpenSky account and use its client id and secret.",
    "Write them as \"${{ env.NAME }}\" in the configuration so they stay out of the file.",
];

/// An OAuth2 client credential. The secret redacts itself.
#[derive(Debug)]
struct Credentials {
    client_id: String,
    client_secret: Secret,
}

/// A bearer token and when it stops being one.
#[derive(Debug)]
struct Token {
    value: Secret,
    expires_at: Instant,
    /// What the token endpoint said it was good for, for the log line.
    lifetime: Duration,
}

/// The token endpoint's answer.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

impl Token {
    /// Whether this token is still worth sending.
    fn usable(&self) -> bool {
        Instant::now() + TOKEN_SKEW < self.expires_at
    }
}

/// The credential, whatever token it currently buys, and what has been said
/// about getting one.
#[derive(Debug)]
pub struct Tokens {
    /// The token endpoint. A field rather than a constant so that this crate's
    /// own suite can point it at a mock instead of at OpenSky.
    pub url: reqwest::Url,

    credentials: Option<Credentials>,
    token: Option<Token>,

    /// The run of renewals that failed, so a token endpoint having a bad hour
    /// is one warning rather than one per poll.
    trouble: Repeated,

    /// How many renewals have worked. The first is worth an `info` — it is the
    /// proof that a credential an operator has just configured is a good one —
    /// and the half-hourly ones after it are not.
    renewals: u64,
}

impl Tokens {
    /// Half a credential is no credential: both halves, or the anonymous tier.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when this crate's own token URL
    /// will not parse, which is a bug rather than a setting.
    pub fn new(client_id: Option<String>, client_secret: Option<Secret>) -> Result<Self, Error> {
        Ok(Self {
            url: reqwest::Url::parse(TOKEN_URL)
                .or_system_err(rustak_core::errors::ADVICE_REPORT_DEV)?,
            credentials: match (client_id, client_secret) {
                (Some(client_id), Some(client_secret)) => Some(Credentials {
                    client_id,
                    client_secret,
                }),
                _ => None,
            },
            token: None,
            trouble: Repeated::new(Duration::ZERO),
            renewals: 0,
        })
    }

    /// Whether this feed has a credential to authenticate with at all.
    #[must_use]
    pub const fn authenticated(&self) -> bool {
        self.credentials.is_some()
    }

    /// The bearer token to send, when there is one worth sending.
    #[must_use]
    pub fn bearer(&self) -> Option<&str> {
        Some(self.token.as_ref()?.value.expose())
    }

    /// Makes sure a usable token is held, when there is a credential to buy one
    /// with. `force` renews a token the server has just refused.
    pub async fn ensure(&mut self, client: &reqwest::Client, force: bool) {
        let Some(credentials) = &self.credentials else {
            return;
        };

        if !force && self.token.as_ref().is_some_and(Token::usable) {
            return;
        }

        let now = Utc::now();

        match refresh(client, &self.url, credentials).await {
            Ok(token) => {
                self.renewed(&token, now);
                self.token = Some(token);
            }
            Err(err) => {
                self.refused(&err, now);
                self.token = None;
            }
        }
    }

    /// Says what a renewal is worth saying: the first one, and the end of a run
    /// of failed ones.
    fn renewed(&mut self, token: &Token, now: DateTime<Utc>) {
        let seconds = token.lifetime.as_secs();

        match self.trouble.cleared(now) {
            Report::Recovered { count, over } => info!(
                seconds,
                "The OpenSky token endpoint is answering again after {} and {count} failed \
                 renewals.",
                humanised(over),
            ),
            _ if self.renewals == 0 => info!(seconds, "Renewed the OpenSky token."),
            _ => debug!(seconds, "Renewed the OpenSky token."),
        }

        self.renewals = self.renewals.saturating_add(1);
    }

    /// The same for a renewal that did not work.
    fn refused(&mut self, err: &Error, now: DateTime<Utc>) {
        match self.trouble.happened(now) {
            Report::First => {
                warn!("Could not renew the OpenSky token; continuing anonymously. {err}");
            }
            Report::Reminder { count, over } => warn!(
                "The OpenSky token endpoint has refused {count} renewals in the last {}; still \
                 polling anonymously. {err}",
                humanised(over),
            ),
            _ => debug!("Could not renew the OpenSky token; continuing anonymously. {err}"),
        }
    }

    /// Whether a token is being held, for a suite that would rather assert than
    /// read a log.
    #[cfg(test)]
    #[must_use]
    pub const fn held(&self) -> bool {
        self.token.is_some()
    }
}

/// Exchanges a client credential for a bearer token.
async fn refresh(
    client: &reqwest::Client,
    token_url: &reqwest::Url,
    credentials: &Credentials,
) -> Result<Token, Error> {
    let response = client
        .post(token_url.clone())
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", credentials.client_id.as_str()),
            ("client_secret", credentials.client_secret.expose()),
        ])
        .send()
        .await
        .wrap_user_err(
            "We could not reach OpenSky's token endpoint.",
            ADVICE_CREDENTIAL,
        )?
        .error_for_status()
        .wrap_user_err(
            "OpenSky would not issue a token for that credential.",
            ADVICE_CREDENTIAL,
        )?;

    let token: TokenResponse = response.json().await.wrap_user_err(
        "OpenSky's token endpoint sent something unexpected.",
        ADVICE_CREDENTIAL,
    )?;

    let lifetime = token
        .expires_in
        .map_or(TOKEN_DEFAULT_LIFETIME, Duration::from_secs);

    Ok(Token {
        value: Secret::new(token.access_token),
        expires_at: Instant::now() + lifetime,
        lifetime,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_a_credential_is_no_credential() {
        for (id, secret) in [
            (Some("rustak".to_string()), None),
            (None, Some(Secret::new("shhh"))),
            (None, None),
        ] {
            assert!(!Tokens::new(id, secret).expect("it builds").authenticated());
        }

        assert!(
            Tokens::new(Some("rustak".to_string()), Some(Secret::new("shhh")))
                .expect("it builds")
                .authenticated(),
        );
    }

    #[test]
    fn a_feed_with_no_token_sends_no_bearer() {
        let tokens = Tokens::new(None, None).expect("it builds");

        assert_eq!(tokens.bearer(), None);
        assert!(!tokens.held());
    }

    #[test]
    fn the_secret_never_appears_in_a_debug_rendering() {
        let rendered = format!(
            "{:?}",
            Tokens::new(Some("rustak".to_string()), Some(Secret::new("hunter2")))
                .expect("it builds"),
        );

        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("Secret(***)"), "{rendered}");
    }
}
