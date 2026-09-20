//! A [`Feed`] that reads a file, for demonstrations and for tests.
//!
//! The fixture format is one JSON [`Track`] per line — newline-delimited JSON,
//! so a file can be appended to, `grep`ped and diffed, and a single malformed
//! line is a single malformed track rather than an unreadable file:
//!
//! ```json
//! {"id":"AIS-244660000","kind":{"vessel":"merchant"},"position":[51.95,4.13],"speed_mps":6.2,"course_deg":271.5,"callsign":"ZEEBRUGGE","remarks":[["MMSI","244660000"]],"observed_at":"2026-09-20T12:00:00Z"}
//! ```
//!
//! Blank lines and lines beginning with `#` are skipped, so a fixture can carry
//! a comment saying what it is a fixture *of*.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use human_errors::{Error, ResultExt};
use rustak_core::errors::ADVICE_FILE_ACCESS;

use super::{Feed, Track};

/// Advice for a fixture file we could read but not understand.
const ADVICE_FIXTURE: &[&str] = &[
    "A replay fixture is one JSON track per line; see the \"Feed sidecars\" section of docs/plugins.md for the fields.",
    "Blank lines and lines starting with '#' are ignored, so a comment is fine but a truncated line is not.",
];

/// Replays a file of observations as though they were arriving now.
///
/// Every [`poll`](Feed::poll) answers the whole file, **stamped with the time
/// it was offered**: a fixture written last week would otherwise publish tracks
/// that every client expires the moment they arrive. The file's own
/// `observed_at` is what a real source would have reported and is kept in the
/// fixture as documentation of the format.
///
/// Offering the same observations on every tick is not a mistake — it is what
/// makes a replay a fair test of the [`PublishPolicy`](super::PublishPolicy):
/// the publisher is what turns a file offered ten times a minute back into one
/// CoT event per track per minute.
#[derive(Clone, Debug)]
pub struct Replay {
    path: PathBuf,
    tracks: Vec<Track>,
}

impl Replay {
    /// Reads a fixture file.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the file cannot be read or a
    /// line is not a track, naming the line.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let body = std::fs::read_to_string(&path).wrap_user_err(
            format!("We could not read the replay file '{}'.", path.display()),
            ADVICE_FILE_ACCESS,
        )?;

        Ok(Self {
            tracks: Self::parse(&body, &path)?,
            path,
        })
    }

    /// A replay over tracks that are already in hand, for a test that would
    /// rather not write a file.
    #[must_use]
    pub fn over(tracks: Vec<Track>) -> Self {
        Self {
            path: PathBuf::from("(in memory)"),
            tracks,
        }
    }

    /// How many observations the file holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Whether the file held no observations at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// The file this replay was read from.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the fixture body, naming the line that failed.
    fn parse(body: &str, path: &Path) -> Result<Vec<Track>, Error> {
        let mut tracks = Vec::new();

        for (index, line) in body.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            tracks.push(serde_json::from_str::<Track>(line).wrap_user_err(
                format!(
                    "We could not read line {} of the replay file '{}' as a track.",
                    index + 1,
                    path.display(),
                ),
                ADVICE_FIXTURE,
            )?);
        }

        Ok(tracks)
    }
}

#[async_trait]
impl Feed for Replay {
    fn name(&self) -> &str {
        "replay"
    }

    async fn poll(&mut self) -> Result<Vec<Track>, Error> {
        let now = Utc::now();

        Ok(self
            .tracks
            .iter()
            .map(|track| Track {
                observed_at: now,
                ..track.clone()
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::{TrackKind, VesselClass};

    const FIXTURE: &str = concat!(
        "# Two vessels in the Maas approach.\n",
        "\n",
        r#"{"id":"AIS-1","kind":{"vessel":"merchant"},"position":[51.95,4.13],"observed_at":"2020-01-01T00:00:00Z"}"#,
        "\n",
        r#"{"id":"AIS-2","kind":{"vessel":"fishing"},"position":[51.96,4.14],"callsign":"NORDKAP","observed_at":"2020-01-01T00:00:00Z"}"#,
        "\n",
    );

    #[tokio::test]
    async fn a_fixture_reads_as_tracks_and_ignores_its_comments() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("tracks.ndjson");
        std::fs::write(&path, FIXTURE).expect("the fixture lands");

        let mut replay = Replay::open(&path).expect("the fixture reads");

        assert_eq!(replay.len(), 2);
        assert!(!replay.is_empty());
        assert_eq!(replay.path(), path);
        assert_eq!(replay.name(), "replay");

        let polled = replay.poll().await.expect("a replay never fails to poll");

        assert_eq!(polled.len(), 2);
        assert_eq!(polled[0].id, "AIS-1");
        assert_eq!(polled[0].kind, TrackKind::Vessel(VesselClass::Merchant));
        assert_eq!(polled[1].callsign.as_deref(), Some("NORDKAP"));
    }

    #[tokio::test]
    async fn observations_are_stamped_as_they_are_offered() {
        // A fixture written in 2020 would otherwise publish events that every
        // client expires on arrival.
        let mut replay = Replay::over(vec![Track::new(
            "AIS-1",
            TrackKind::Vessel(VesselClass::Merchant),
            (51.95, 4.13),
            "2020-01-01T00:00:00Z".parse().unwrap(),
        )]);

        let before = Utc::now();
        let polled = replay.poll().await.unwrap();

        assert!(polled[0].observed_at >= before);
    }

    #[test]
    fn a_line_that_is_not_a_track_says_which_line() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("tracks.ndjson");
        std::fs::write(&path, "{\"id\":\"AIS-1\"}\n").expect("the fixture lands");

        let err = Replay::open(&path).expect_err("a truncated track is refused");

        assert!(err.to_string().contains("line 1"), "{err}");
        assert!(err.is(human_errors::Kind::User));
    }

    #[test]
    fn a_missing_file_is_the_operators_problem_rather_than_a_panic() {
        let err = Replay::open("/nonexistent/tracks.ndjson").expect_err("there is no such file");

        assert!(err.is(human_errors::Kind::User));
    }
}
