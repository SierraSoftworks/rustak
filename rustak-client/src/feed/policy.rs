//! How often a feed is allowed to say the same thing.
//!
//! A thousand ships reporting every two seconds is a thousand CoT messages a
//! second, on a channel a phone is holding open over a cellular link. The
//! policy is what turns that into a rate an operator's device can carry without
//! the operator noticing anything is missing: a track that has not moved does
//! not need repeating, and a track that has moved does not need repeating twice
//! in the same second.

use std::time::Duration;

use rustak_core::config::duration;
use serde::{Deserialize, Serialize};

/// How long a track lives on a map without another report. Long enough that a
/// vessel at anchor (a position report every three minutes) does not flicker.
fn default_stale() -> chrono::Duration {
    chrono::Duration::seconds(120)
}

/// The floor between two publications of the same track.
fn default_min_interval() -> chrono::Duration {
    chrono::Duration::seconds(5)
}

/// How long a track may go unpublished while it is still being seen.
fn default_max_interval() -> chrono::Duration {
    chrono::Duration::seconds(60)
}

/// How far a track moves before it is worth saying so, in metres.
fn default_min_move_m() -> f64 {
    25.0
}

/// How many tracks one sidecar will hold. A busy 250 nm ADS-B circle is a few
/// hundred aircraft and a coastal AIS box a few thousand vessels, so this is a
/// ceiling on a runaway area rather than a working limit.
fn default_max_tracks() -> usize {
    5_000
}

/// A course change worth publishing on its own, in degrees. A vessel yawing at
/// anchor swings further than this, which is why it is paired with the speed
/// and distance tests rather than used alone.
pub(super) const COURSE_CHANGE_DEG: f64 = 10.0;

/// A speed change worth publishing on its own, in metres per second — about
/// five knots.
pub(super) const SPEED_CHANGE_MPS: f64 = 2.5;

/// What the publisher is allowed to send, and how long what it sent lives.
///
/// Every field has a default and every default is documented in each plugin's
/// `config.example.toml`, so an operator changes the one knob they came for.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublishPolicy {
    /// How long a published track stays on a map without another report.
    ///
    /// A feed sends no delete message: TAK clients expire a track by its
    /// `stale` attribute, which means a sidecar that is killed leaves a map
    /// that empties itself rather than one full of ghosts.
    #[serde(default = "default_stale", with = "duration::humane")]
    pub stale: chrono::Duration,

    /// The shortest gap between two publications of the same track.
    #[serde(default = "default_min_interval", with = "duration::humane")]
    pub min_interval: chrono::Duration,

    /// The longest a track that is still being seen goes unpublished. This is
    /// what keeps a motionless vessel on the map: it is republished before its
    /// `stale` passes even though nothing about it changed.
    #[serde(default = "default_max_interval", with = "duration::humane")]
    pub max_interval: chrono::Duration,

    /// How far a track must move since its last publication to be published
    /// again, in metres.
    #[serde(default = "default_min_move_m")]
    pub min_move_m: f64,

    /// How many tracks the publisher holds before it drops the least recently
    /// seen.
    #[serde(default = "default_max_tracks")]
    pub max_tracks: usize,
}

impl Default for PublishPolicy {
    /// Written out rather than derived, because serde's `default = "…"` applies
    /// only when deserialising: a derived `Default` would hand back zero-length
    /// intervals while an empty table gave the real ones.
    fn default() -> Self {
        Self {
            stale: default_stale(),
            min_interval: default_min_interval(),
            max_interval: default_max_interval(),
            min_move_m: default_min_move_m(),
            max_tracks: default_max_tracks(),
        }
    }
}

impl PublishPolicy {
    /// The same policy with a different staleness horizon, which is the one
    /// knob the two feeds disagree about: an aircraft reporting every second is
    /// gone in ninety, a vessel at anchor reports every three minutes.
    #[must_use]
    pub fn with_stale(mut self, stale: Duration) -> Self {
        self.stale = chrono::Duration::from_std(stale).unwrap_or_else(|_| default_stale());
        self
    }

    /// How long a published track lives, as [`Event::stale`](rustak_cot::Event)
    /// wants it.
    #[must_use]
    pub fn stale(&self) -> Duration {
        to_std(self.stale, default_stale())
    }

    /// The floor between two publications of one track.
    #[must_use]
    pub fn min_interval(&self) -> Duration {
        to_std(self.min_interval, default_min_interval())
    }

    /// The ceiling on how long a track goes unpublished while it is still seen.
    #[must_use]
    pub fn max_interval(&self) -> Duration {
        to_std(self.max_interval, default_max_interval())
    }
}

/// Converts a configured span for the standard library, falling back rather
/// than panicking on a negative one the adapter would already have refused.
fn to_std(value: chrono::Duration, fallback: chrono::Duration) -> Duration {
    value
        .to_std()
        .unwrap_or_else(|_| fallback.to_std().unwrap_or(Duration::from_secs(60)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_table_is_the_written_out_default() {
        let parsed: PublishPolicy = toml::from_str("").expect("every key has a default");

        assert_eq!(parsed, PublishPolicy::default());
        assert_eq!(parsed.stale(), Duration::from_secs(120));
        assert_eq!(parsed.min_interval(), Duration::from_secs(5));
        assert_eq!(parsed.max_interval(), Duration::from_secs(60));
        assert_eq!(parsed.min_move_m, 25.0);
        assert_eq!(parsed.max_tracks, 5_000);
    }

    #[test]
    fn spans_are_written_the_way_a_person_writes_them() {
        let parsed: PublishPolicy = toml::from_str(
            r#"
            stale = "90s"
            max_interval = "2m"
            min_move_m = 100.0
            "#,
        )
        .expect("a partial table keeps the defaults for what it omits");

        assert_eq!(parsed.stale(), Duration::from_secs(90));
        assert_eq!(parsed.max_interval(), Duration::from_secs(120));
        assert_eq!(parsed.min_interval(), Duration::from_secs(5));
        assert_eq!(parsed.min_move_m, 100.0);
    }

    #[test]
    fn a_misspelled_key_is_a_start_up_failure() {
        let refused = toml::from_str::<PublishPolicy>(r#"stale_after = "90s""#);

        assert!(refused.is_err());
    }

    #[test]
    fn the_adsb_default_is_the_ais_one_with_a_shorter_horizon() {
        let policy = PublishPolicy::default().with_stale(Duration::from_secs(90));

        assert_eq!(policy.stale(), Duration::from_secs(90));
        assert_eq!(
            policy.min_interval(),
            PublishPolicy::default().min_interval()
        );
    }
}
