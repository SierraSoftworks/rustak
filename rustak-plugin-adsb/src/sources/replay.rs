//! The offline source: a file of tracks, replayed on every tick.
//!
//! [`rustak_client::feed::Replay`] does the reading; this wraps it so that it
//! answers [`AdsbFeed::state`] like the live sources do, and so the heartbeat
//! and the Services page have something to say about a sidecar that is running
//! a demonstration.

use std::path::Path;
use std::time::Duration;

use rustak_client::feed::{Feed, Replay, Track};
use rustak_client::sidecar::async_trait;
use rustak_core::prelude::*;

use super::{AdsbFeed, SourceState};

/// The interval a replay reports; it is a file, so every tick is fine.
const INTERVAL: Duration = Duration::from_secs(0);

/// A [`Replay`] that also says how it is doing.
#[derive(Debug)]
pub struct ReplayFeed {
    inner: Replay,
    state: SourceState,
}

impl ReplayFeed {
    /// Opens a fixture file.
    ///
    /// # Errors
    ///
    /// Whatever [`Replay::open`] answers: a file that is missing or holds a
    /// line that is not a track, naming itself.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let inner = Replay::open(path)?;
        let mut state = SourceState::new("replay", INTERVAL);

        // The file opened, which is the whole of a replay's connection.
        state.succeeded();

        Ok(Self { inner, state })
    }

    /// How many observations the file holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the file held no observations at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[async_trait]
impl Feed for ReplayFeed {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn poll(&mut self) -> Result<Vec<Track>, Error> {
        self.inner.poll().await
    }
}

impl AdsbFeed for ReplayFeed {
    fn state(&self) -> &SourceState {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_replay_is_connected_as_soon_as_its_file_opens() {
        let directory = tempfile::tempdir().expect("a directory");
        let path = directory.path().join("tracks.ndjson");
        std::fs::write(
            &path,
            r#"{"id":"ADSB-3c6444","kind":{"aircraft":"civil_fixed_wing"},"position":[51.4,-0.4],"observed_at":"2026-09-20T12:00:00Z"}"#,
        )
        .expect("the fixture lands");

        let mut feed = ReplayFeed::open(&path).expect("it opens");

        assert!(feed.state().is_connected());
        assert!(feed.state().ever_connected());
        assert_eq!(feed.len(), 1);
        assert!(!feed.is_empty());
        assert_eq!(feed.poll().await.expect("a poll").len(), 1);
    }

    #[test]
    fn a_missing_file_names_itself() {
        let err = ReplayFeed::open("/nowhere/at/all/tracks.ndjson").expect_err("it cannot open");

        assert!(err.to_string().contains("tracks.ndjson"), "{err}");
    }
}
