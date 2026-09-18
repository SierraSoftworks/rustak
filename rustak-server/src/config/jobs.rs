//! `[jobs]` — how the background queue consumer behaves when things go wrong.
//!
//! Two bounds, both of them about a failure that does not resolve itself. A
//! message whose payload no longer deserialises, or whose handler has a
//! deterministic bug, is not going to succeed on the thousandth attempt any
//! more than on the tenth, and retrying it for the life of the installation
//! costs an error line every backoff and hides the failures that matter. And a
//! backlog — after an outage, or a sweep that enqueued a lot — is spawned as
//! fast as the queue hands rows over, each job holding a context and doing work
//! on the single database writer.

use serde::{Deserialize, Serialize};

/// How many times a message is retried before it is set aside.
///
/// Ten attempts with the host's doubling backoff is a little over an hour of
/// trying, which covers every transient failure worth covering — a restarting
/// database, an identity provider rebooting, an ACME directory rate-limiting
/// us — and stops well short of for ever.
fn default_max_attempts() -> u32 {
    10
}

/// How many jobs may run at once.
///
/// Four rather than the core count: the work these do is almost entirely
/// database writes against one writer connection, so more of them in flight
/// buys nothing and costs the web API its turn.
fn default_concurrency() -> usize {
    4
}

/// `[jobs]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobsConfig {
    /// How many times a failing message is retried before it is moved to the
    /// dead letters and stops being run.
    ///
    /// `0` means "retry for ever", which is the behaviour this setting exists
    /// to replace; it is here so that an installation debugging a job can ask
    /// for it deliberately.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,

    /// How many jobs may run at the same time.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
}

impl Default for JobsConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            concurrency: default_concurrency(),
        }
    }
}

impl JobsConfig {
    /// How many jobs may run at once, never zero.
    pub fn in_flight(&self) -> usize {
        self.concurrency.max(1)
    }

    /// Whether a message that has been tried `attempts` times is finished with.
    pub fn is_exhausted(&self, attempts: u32) -> bool {
        self.max_attempts > 0 && attempts >= self.max_attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_section_is_the_written_out_default() {
        let parsed: JobsConfig = toml::from_str("").unwrap();

        assert_eq!(parsed, JobsConfig::default());
        assert_eq!(parsed.max_attempts, 10);
        assert_eq!(parsed.concurrency, 4);
    }

    #[test]
    fn a_zero_ceiling_is_the_old_retry_for_ever_behaviour() {
        let parsed: JobsConfig = toml::from_str("max_attempts = 0").unwrap();

        assert!(!parsed.is_exhausted(1_000_000));
    }

    #[test]
    fn a_message_is_finished_with_once_it_reaches_the_ceiling() {
        let jobs = JobsConfig::default();

        assert!(!jobs.is_exhausted(9));
        assert!(jobs.is_exhausted(10));
    }

    #[test]
    fn no_concurrency_still_runs_one_job() {
        let parsed: JobsConfig = toml::from_str("concurrency = 0").unwrap();

        assert_eq!(parsed.in_flight(), 1);
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let Err(err) = toml::from_str::<JobsConfig>("max_retries = 3") else {
            panic!("an unknown key should be refused");
        };

        assert!(err.to_string().contains("max_retries"), "{err}");
    }
}
