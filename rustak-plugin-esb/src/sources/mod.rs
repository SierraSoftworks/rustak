//! Where the outages come from.
//!
//! [`powercheck`] reads ESB Networks' PowerCheck API and [`replay`] reads a
//! file, for demonstrations and tests. Either is an [`OutageFeed`]: it owns its
//! own cadence, backoff and memory, answers **everything currently listed** on
//! every poll, and says how its upstream is doing for the heartbeat.

pub mod powercheck;
pub mod replay;
mod state;

use std::time::Duration;

use reqwest::header::HeaderMap;
use rustak_client::feed::upstream;
use rustak_client::sidecar::async_trait;
use rustak_core::prelude::*;

use crate::outage::Outage;
use crate::scope::Scope;

pub use powercheck::PowerCheckFeed;
pub use replay::ReplayFeed;
pub use rustak_client::feed::upstream::{REMIND_EVERY, Report};
pub use state::{Esb, MAX_BACKOFF, MAX_RETRY_AFTER, SourceState};

/// What this plugin calls itself upstream, so that ESB can tell who is asking.
pub const USER_AGENT: &str = concat!(
    "rustak-plugin-esb/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/SierraSoftworks/rustak)",
);

/// How long any one request may take; the next tick is only seconds away.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A source of outages.
#[async_trait]
pub trait OutageFeed: Send {
    /// The upstream's name, for a log line.
    fn name(&self) -> &str;

    /// Every outage currently listed, as far as this source knows.
    ///
    /// Called on every tick, which is far more often than an upstream is
    /// asked: a poll between requests answers what the last one learned.
    ///
    /// # Errors
    ///
    /// Whatever went wrong upstream, for the plugin to log. Never a reason to
    /// stop the sidecar, and never a reason to clear the map.
    async fn poll(&mut self) -> Result<Vec<Outage>, Error>;

    /// Moves the feed onto a new area without opening it again, so that an
    /// administrator moving the area keeps the schedule, the backoff and any
    /// `Retry-After` ESB asked for. A source that does not filter by area
    /// has nothing to do.
    fn rescope(&mut self, scope: Scope) {
        let _ = scope;
    }

    /// How the upstream is doing.
    fn state(&self) -> &SourceState;
}

/// Builds the HTTP client a live source uses, against the public roots.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the TLS backend will not start.
pub fn http_client(headers: HeaderMap) -> Result<reqwest::Client, Error> {
    upstream::http_client(USER_AGENT, REQUEST_TIMEOUT, headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_agent_names_the_software_and_links_to_it() {
        assert!(USER_AGENT.starts_with("rustak-plugin-esb/"));
        assert!(USER_AGENT.contains("github.com/SierraSoftworks/rustak"));
    }
}
