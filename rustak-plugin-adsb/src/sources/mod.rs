//! Where the aircraft come from.
//!
//! Four upstreams, one shape: [`readsb`] reads a decoder's own
//! `aircraft.json`, [`aggregator`] reads the same document from a public pool
//! of receivers, [`opensky`] reads OpenSky's state vectors, and [`replay`]
//! reads a file for demonstrations and tests. Each is a
//! [`Feed`] that also answers how its upstream is
//! doing, which is what the sidecar's heartbeat reports.
//!
//! # Being a good citizen of somebody else's service
//!
//! Three of these are free services run by volunteers, and the plugin behaves
//! like it:
//!
//! - every request carries [`USER_AGENT`], which names the software and links
//!   to it, so an operator of the upstream can find out who we are;
//! - [`SourceState`] puts a floor under the request rate that is independent of
//!   the sidecar's tick, and backs off when the answer is a failure;
//! - a `429` is honoured for as long as the response asks, within reason, and a
//!   provider that asks twice inside ten polls has the poll interval raised to
//!   what it asked for until the process restarts;
//! - a provider that refuses twice inside ten polls *without* saying how long
//!   to wait — which is what adsb.lol does — is backed off from by half as much
//!   again, up to two minutes, and eased back towards after sixty polls in a
//!   row that nobody refused, so a configured `poll` is a floor and not a pin;
//! - repeated `403`s stop the source rather than hammering a service that has
//!   said no.
//!
//! # And it says so once
//!
//! The shared [`Repeated`](rustak_client::feed::upstream::Repeated) is the
//! other half of being a guest: an operator who cannot see a
//! rate limit in a log because the log is nothing but rate limits is no better
//! off than one who was never told. Every run of the same thing — an outage,
//! a rate limit, a token that will not renew — is announced once, reminded
//! about every five minutes with a count, and closed with one line.

pub mod aggregator;
pub mod opensky;
pub mod readsb;
pub mod replay;
mod state;

use std::time::Duration;

use reqwest::header::HeaderMap;
use rustak_client::feed::Feed;
use rustak_client::feed::upstream;
use rustak_core::prelude::*;

pub use aggregator::{AggregatorFeed, Provider};
pub use opensky::OpenSkyFeed;
pub use readsb::ReadsbFeed;
pub use replay::ReplayFeed;
pub use rustak_client::feed::upstream::{retry_after, retry_after_at};
pub use state::{
    Adsb, CLEAN_RUN, LIMIT_WINDOW, MAX_ADAPTED, MAX_BACKOFF, RECENT_POLLS, SourceState,
};

/// What this plugin calls itself to every upstream it reaches.
///
/// Descriptive on purpose: adsb.fi and airplanes.live both ask that a client
/// identify itself, and a service that can see who is calling can ask us to
/// stop rather than blocking an address range.
pub const USER_AGENT: &str = concat!(
    "rustak-plugin-adsb/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/SierraSoftworks/rustak)",
);

/// How long any one request to an upstream may take.
///
/// Short: a source that is wedged must not hold up the sidecar's tick, and the
/// next poll is only an interval away.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A [`Feed`] this plugin can also ask how its upstream is doing.
///
/// The shared trait is deliberately two methods wide — a name and a poll — so
/// this is the plugin's own extension rather than a change to
/// [`rustak_client::feed`]: what an administrator wants on the Services page is
/// specific to a feed that reaches out over HTTP, which not every feed does.
pub trait AdsbFeed: Feed {
    /// How the upstream is doing, for the heartbeat and for the logs.
    fn state(&self) -> &SourceState;
}

/// Builds the HTTP client a live source reaches its upstream with.
///
/// Public roots rather than a deployment truststore: these are public services
/// behind public certificates, and they have nothing to do with the rustak
/// server's own CA.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the TLS backend will not
/// initialise, which is not something an operator can do anything about.
pub fn http_client() -> Result<reqwest::Client, Error> {
    upstream::http_client(USER_AGENT, REQUEST_TIMEOUT, HeaderMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_agent_names_the_software_and_links_to_it() {
        assert!(USER_AGENT.starts_with("rustak-plugin-adsb/"));
        assert!(USER_AGENT.contains("github.com/SierraSoftworks/rustak"));
    }

    #[test]
    fn the_client_builds() {
        assert!(http_client().is_ok());
    }
}
