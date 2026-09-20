//! Where the vessels come from.
//!
//! One file per upstream, one variant of [`Source`] each, and the same shape
//! throughout: a task that keeps the upstream open and buffers what arrives, a
//! [`Feed::poll`] that drains that buffer, and a [`ConnectionTx`] that says on
//! the sidecar's heartbeat whether the upstream is answering.
//!
//! | `kind` | What it is | What it costs |
//! |---|---|---|
//! | `replay` | A file of tracks, offered on every tick | Nothing; for demonstrations and tests |
//! | `aisstream` | The AISStream.io WebSocket feed | A free API key, and the site's terms |
//! | `udp` | `!AIVDM` sentences from a receiver of your own | An antenna |
//!
//! A fourth — Fintraffic's Digitraffic MQTT feed — is described in the README
//! and deliberately not implemented; see the M9-01 status note.
//!
//! # Reconnection belongs here
//!
//! [`Feed::poll`] answering an error means "this poll produced nothing and here
//! is why", and the plugin logs it and carries on. Deciding whether the
//! upstream wants a new socket, a new subscription or simply another minute is
//! the source's own business, which is what this module's capped exponential
//! backoff is for.

mod aisstream;
mod udp;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use rustak_client::feed::{Area, Feed, PublishPolicy, Replay};
use rustak_core::prelude::*;

use crate::status::ConnectionTx;

/// The shortest an upstream is left alone after it failed.
const RETRY_MIN: Duration = Duration::from_secs(1);

/// The longest, however many times it has failed. A minute: an open feed that
/// is down is usually down for a while, and hammering it is how a free API key
/// stops being one.
const RETRY_MAX: Duration = Duration::from_secs(60);

/// What a source needs from the plugin to open itself.
pub struct SourceContext {
    /// Where the feed is looking. An upstream that takes a subscription
    /// subscribes with this; the publisher checks it again regardless.
    pub area: Area,
    /// The publishing policy, which also bounds the per-MMSI caches.
    pub policy: PublishPolicy,
    /// The process-wide stop signal, so that a source's own task ends with it.
    pub shutdown: Shutdown,
    /// Where the source says whether its upstream is answering.
    pub connection: ConnectionTx,
}

/// Where this plugin reads observations from.
///
/// `#[serde(tag = "kind")]`, so a settings file names the upstream it means:
///
/// ```toml
/// [settings.source]
/// kind = "aisstream"
/// api_key = "${{ env.AISSTREAM_API_KEY }}"
/// ```
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// A file of tracks, replayed on every tick. What the demonstration and the
    /// integration suite use, and what proves the rest of the plugin works
    /// without an upstream to be down.
    Replay {
        /// The newline-delimited JSON file; see [`Replay`] for the format.
        path: PathBuf,
    },

    /// The AISStream.io WebSocket feed: worldwide AIS, a free API key, and a
    /// bounding-box subscription.
    #[serde(rename = "aisstream")]
    AisStream {
        /// The key from the site's `/account` page. A credential: hold it in
        /// the environment and write `"${{ env.AISSTREAM_API_KEY }}"` here.
        #[serde(deserialize_with = "secret")]
        api_key: Secret,

        /// Which message types to subscribe to. Default: the four that carry a
        /// position or a name.
        #[serde(default = "aisstream::default_message_types")]
        message_types: Vec<String>,
    },

    /// NMEA 0183 `!AIVDM` sentences over UDP, from a receiver of your own:
    /// AIS-catcher, `rtl_ais`, a dAISy hat.
    Udp {
        /// The address to listen on. `0.0.0.0:10110` takes datagrams from
        /// anywhere on the network; `127.0.0.1:10110` only from this host,
        /// which is what a receiver in the same container should send to.
        listen: SocketAddr,
    },
}

impl Source {
    /// Opens the upstream this setting names.
    ///
    /// # Errors
    ///
    /// Whatever the source could not do, as something the operator can fix: a
    /// replay file that is missing or malformed names itself, and a UDP port
    /// that is already taken names the address.
    pub fn open(&self, context: SourceContext) -> Result<Box<dyn Feed>, Error> {
        match self {
            Self::Replay { path } => {
                // A file is never disconnected, so the heartbeat says so from
                // the moment it opens.
                let feed = Replay::open(path)?;
                context
                    .connection
                    .send_replace(crate::status::Connection::connected());

                Ok(Box::new(feed))
            }
            Self::AisStream {
                api_key,
                message_types,
            } => Ok(Box::new(aisstream::AisStream::open(
                api_key,
                message_types,
                context,
            ))),
            Self::Udp { listen } => Ok(Box::new(udp::UdpFeed::open(*listen, context)?)),
        }
    }

    /// What to call this source before it has been opened, for a log line.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Replay { .. } => "replay",
            Self::AisStream { .. } => aisstream::NAME,
            Self::Udp { .. } => udp::NAME,
        }
    }
}

/// Reads a string into a [`Secret`], which redacts itself when logged.
///
/// The settings type derives `Debug` because the SDK requires it; this is what
/// stops that being the place an API key escapes.
fn secret<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Secret, D::Error> {
    Ok(Secret::new(String::deserialize(deserializer)?))
}

/// A capped exponential wait between attempts at an upstream.
#[derive(Debug)]
pub(crate) struct Backoff {
    wait: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self { wait: RETRY_MIN }
    }
}

impl Backoff {
    /// Forgets every failure so far. Called on a connection that succeeded.
    pub(crate) fn reset(&mut self) {
        self.wait = RETRY_MIN;
    }

    /// Waits, then doubles. Answers `false` when the sidecar is stopping, so
    /// that a source in its backoff is not what holds up a Ctrl-C.
    pub(crate) async fn wait(&mut self, shutdown: &Shutdown) -> bool {
        let waited = tokio::select! {
            biased;

            () = shutdown.cancelled() => false,
            () = tokio::time::sleep(self.wait) => true,
        };

        self.wait = (self.wait * 2).min(RETRY_MAX);

        waited
    }

    /// How long the next failure will wait, for a log line.
    pub(crate) fn next(&self) -> Duration {
        self.wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(toml: &str) -> Result<Source, toml::de::Error> {
        toml::from_str(toml)
    }

    #[test]
    fn every_source_kind_loads_from_the_settings_file() {
        assert!(matches!(
            load("kind = \"replay\"\npath = \"tracks.ndjson\"").expect("replay"),
            Source::Replay { .. },
        ));
        assert!(matches!(
            load("kind = \"udp\"\nlisten = \"127.0.0.1:10110\"").expect("udp"),
            Source::Udp { .. },
        ));

        let stream = load("kind = \"aisstream\"\napi_key = \"abc123\"").expect("aisstream");

        let Source::AisStream {
            api_key,
            message_types,
        } = &stream
        else {
            panic!("an aisstream source");
        };

        assert_eq!(api_key.expose(), "abc123");
        assert_eq!(message_types, &aisstream::default_message_types());
        assert_eq!(stream.kind(), "aisstream.io");
    }

    #[test]
    fn an_api_key_never_reaches_a_log_line() {
        // `Sidecar::Settings` must be `Debug`, and plugins log their settings
        // at start-up; this is the assertion that makes that safe.
        let printed = format!(
            "{:?}",
            load("kind = \"aisstream\"\napi_key = \"rsk_supersecret\"").expect("aisstream"),
        );

        assert!(!printed.contains("rsk_supersecret"), "{printed}");
        assert!(printed.contains("Secret(***)"), "{printed}");
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let refused = load("kind = \"udp\"\nlissen = \"127.0.0.1:10110\"")
            .expect_err("an unknown key is a start-up failure");

        assert!(refused.to_string().contains("lissen"), "{refused}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_backoff_doubles_to_a_ceiling_and_gives_up_on_a_shutdown() {
        let shutdown = Shutdown::new();
        let mut backoff = Backoff::default();

        assert_eq!(backoff.next(), RETRY_MIN);
        assert!(backoff.wait(&shutdown).await);
        assert_eq!(backoff.next(), RETRY_MIN * 2);

        for _ in 0..10 {
            assert!(backoff.wait(&shutdown).await);
        }

        assert_eq!(backoff.next(), RETRY_MAX, "capped, not unbounded");

        shutdown.cancel();

        assert!(
            !backoff.wait(&shutdown).await,
            "a source waiting to retry must not hold up a Ctrl-C",
        );
    }
}
