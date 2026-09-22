//! A FIRMS CSV file, replayed: what the demonstration and the tests run on.
//!
//! The file is the same document FIRMS serves — save a reply, or write one by
//! hand — with blank lines and `#` comments allowed.
//!
//! Acquisition times are **shifted once, when the file is opened**, so that
//! the newest row was seen "just now" and the rest keep their distance behind
//! it: a file written last week would otherwise hold nothing but detections
//! too old to draw. Once, rather than on every poll, because the uid is made
//! from the acquisition time and a detection that was re-stamped every tick
//! would be a new object every tick. A replay therefore lasts one `max_age`;
//! restart the sidecar to play it again.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use rustak_client::sidecar::async_trait;
use rustak_core::prelude::*;

use super::{HotspotFeed, SourceState};
use crate::wire::{self, Detection};

/// A [`HotspotFeed`] over a file.
#[derive(Debug)]
pub struct ReplayFeed {
    detections: Vec<Detection>,
    state: SourceState,
}

impl ReplayFeed {
    /// Opens a FIRMS CSV file.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error naming the file, when it is
    /// missing or is not a FIRMS CSV.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let body = std::fs::read_to_string(path).wrap_user_err(
            format!("We could not read the replay file '{}'.", path.display()),
            rustak_core::errors::ADVICE_FILE_ACCESS,
        )?;

        let parsed = wire::parse(&body).map_err(|first_line| {
            human_errors::user(
                format!(
                    "The replay file '{}' is not a FIRMS CSV: it starts with '{first_line}'.",
                    path.display(),
                ),
                &["The first line must be a FIRMS header naming latitude, longitude, acq_date and acq_time."],
            )
        })?;

        Ok(Self::over(parsed.detections))
    }

    /// A replay of detections already in hand, shifted the same way.
    #[must_use]
    pub fn over(mut detections: Vec<Detection>) -> Self {
        if let Some(newest) = detections.iter().map(|d| d.acquired_at).max() {
            let shift = Utc::now() - newest;

            for detection in &mut detections {
                detection.acquired_at += shift;
            }
        }

        // The file opened, which is the whole of a replay's connection; and it
        // is a file, so every tick is a fine time to read it.
        let mut state = SourceState::new("replay", Duration::ZERO);
        state.succeeded();

        Self { detections, state }
    }

    /// How many detections the file holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.detections.len()
    }

    /// Whether the file held no detections at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.detections.is_empty()
    }
}

#[async_trait]
impl HotspotFeed for ReplayFeed {
    fn name(&self) -> &str {
        self.state.name()
    }

    async fn poll(&mut self) -> Result<Vec<Detection>, Error> {
        Ok(self.detections.clone())
    }

    fn state(&self) -> &SourceState {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_replay_is_as_recent_as_now_and_the_same_on_every_poll() {
        let newest = "2020-01-02T12:00:00Z".parse().expect("an instant");
        let older = "2020-01-02T09:00:00Z".parse().expect("an instant");
        let mut feed = ReplayFeed::over(vec![
            Detection::at(40.0, -8.0, newest),
            Detection::at(40.1, -8.0, older),
        ]);

        let first = feed.poll().await.expect("a file always answers");
        let second = feed.poll().await.expect("a file always answers");

        assert_eq!(first, second, "a re-stamped detection would be a new uid");
        assert!((Utc::now() - first[0].acquired_at).num_seconds() < 5);
        assert_eq!(
            (first[0].acquired_at - first[1].acquired_at).num_hours(),
            3,
            "the rows keep their distance from each other",
        );
    }

    #[test]
    fn a_file_that_is_missing_or_not_a_firms_csv_names_itself() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("not-firms.csv");

        let missing = ReplayFeed::open(&path).expect_err("it is not there");
        assert!(missing.to_string().contains("not-firms.csv"), "{missing}");

        std::fs::write(&path, "name,value\nfire,1\n").expect("it lands");

        let wrong = ReplayFeed::open(&path).expect_err("it is not FIRMS");
        assert!(wrong.to_string().contains("not-firms.csv"), "{wrong}");
    }
}
