//! The offline source: a file of outages, offered on every poll.
//!
//! One JSON [`Outage`] per line; blank lines and `#` comments are ignored.

use std::path::Path;
use std::time::Duration;

use rustak_client::sidecar::async_trait;
use rustak_core::errors::ADVICE_FILE_ACCESS;
use rustak_core::prelude::*;

use super::{OutageFeed, SourceState};
use crate::outage::Outage;

/// A fixture file, replayed.
#[derive(Debug)]
pub struct ReplayFeed {
    outages: Vec<Outage>,
    state: SourceState,
}

impl ReplayFeed {
    /// Opens a fixture file.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the file cannot be read or a
    /// line is not an outage, naming the line.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let body = std::fs::read_to_string(path).wrap_user_err(
            format!("We could not read the replay file '{}'.", path.display()),
            ADVICE_FILE_ACCESS,
        )?;

        let mut outages = Vec::new();

        for (index, line) in body.lines().enumerate() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }

            outages.push(serde_json::from_str(line).wrap_user_err(
                format!(
                    "Line {} of '{}' is not an outage.",
                    index + 1,
                    path.display()
                ),
                &["Each line is one JSON object; see outages.example.ndjson for the fields."],
            )?);
        }

        let mut state = SourceState::new("replay", Duration::ZERO);
        // The file opened, which is the whole of a replay's connection.
        state.succeeded();

        Ok(Self { outages, state })
    }
}

#[async_trait]
impl OutageFeed for ReplayFeed {
    fn name(&self) -> &str {
        self.state.name()
    }

    async fn poll(&mut self) -> Result<Vec<Outage>, Error> {
        Ok(self.outages.clone())
    }

    fn state(&self) -> &SourceState {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("outages.ndjson");
        std::fs::write(&path, body).expect("the fixture lands");

        (directory, path)
    }

    #[tokio::test]
    async fn the_demonstration_fixture_replays_in_full_on_every_poll() {
        let (_directory, path) = file(include_str!("../../outages.example.ndjson"));
        let mut feed = ReplayFeed::open(&path).expect("it opens");

        assert_eq!(feed.poll().await.expect("a poll").len(), 5);
        assert_eq!(feed.poll().await.expect("another").len(), 5);
        assert!(feed.state().is_connected());
    }

    #[test]
    fn a_line_that_is_not_an_outage_is_refused_by_number() {
        let (_directory, path) = file("# a comment\n\n{\"id\": \"1\"}\n");

        let err = ReplayFeed::open(&path).expect_err("no kind, no position");

        assert!(err.to_string().contains("Line 3"), "{err}");
    }

    #[test]
    fn a_missing_file_names_itself() {
        let err = ReplayFeed::open("/nonexistent/outages.ndjson").expect_err("not there");

        assert!(err.to_string().contains("outages.ndjson"), "{err}");
    }
}
