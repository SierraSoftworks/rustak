//! Being a good guest of somebody else's service: what every feed that polls
//! an upstream shares.
//!
//! Four feed plugins read open data — aircraft, ships, fires, power cuts — and
//! three of them poll an HTTP API on a timer. Each grew the same machinery,
//! and the fourth copy is where it came here.
//!
//! | Piece | What it gives a feed |
//! |---|---|
//! | [`SourceState`] | A floor under the request rate independent of the tick, doubling backoff to a ceiling, `Retry-After` honoured above `poll`, the connection state a heartbeat reports, and every state change logged once |
//! | [`Rules`] | The ceilings and the words: two things to state, the rest defaulted |
//! | [`Repeated`] / [`Report`] | A thing that keeps happening, said once, reminded about every [`REMIND_EVERY`], and closed with one line |
//! | [`retry_after`] | The tolerant `Retry-After` reading: seconds, decimal seconds, and all three HTTP-date spellings |
//! | [`Every`] | The periodic counters line's cadence ([`REPORT_EVERY`]), judged on the caller's clock |
//! | [`http_client`] | A client on public roots, with a descriptive `User-Agent` and a timeout |
//!
//! # What was shared, and what was not
//!
//! Decided from the four plugins as they stood (M10-13):
//!
//! - **Shared as it was:** the state-change notices (`Repeated`, which ADS-B,
//!   ESB and FIRMS carried identically and AIS as the case that ends on the
//!   first clear); the outage and rate-limit runs and their reminder cadence;
//!   the doubling backoff; "`poll` is a floor", with a stated delay waited out
//!   above it and an unstated one guessed at twice the interval and said to be
//!   our guess; the tolerant `Retry-After` reading from M9-12; the counters
//!   line's five-minute cadence.
//! - **Shared as a parameter**, because the plugins disagree and the
//!   disagreement is kept rather than settled here: the backoff ceiling, how
//!   far a stated delay is believed, whether a refusal is an answer, how long a
//!   run of refusals takes to settle, and every sentence ([`Rules`]).
//! - **Not shared:** ADS-B's adaptive cadence (AIMD, the refused-rung memory).
//!   No second plugin would use it as it is, so it stays in ADS-B and moves the
//!   interval through [`SourceState::set_interval`]. AIS's streaming sources
//!   are not pollers; they share [`Repeated`] and nothing else. Settings
//!   (`area`, `area_from`) are not source state and stay with each plugin.

mod every;
mod notice;
mod retry;
mod rules;
mod state;

use std::time::Duration;

use reqwest::header::HeaderMap;
use rustak_core::prelude::*;

pub use every::{Every, REPORT_EVERY};
pub use notice::{REMIND_EVERY, Repeated, Report, humanised};
pub use retry::{RETRY_AFTER, retry_after, retry_after_at};
pub use rules::Rules;
pub use state::SourceState;

/// Builds the HTTP client a live source reaches its upstream with.
///
/// `user_agent` should name the software and link to it — a service that can
/// see who is calling can ask us to stop rather than blocking an address
/// range. `timeout` bounds any one request, because a wedged source must not
/// hold up the sidecar's tick. `headers` are sent on every request (an API
/// key, say); pass an empty map for none.
///
/// Public roots rather than a deployment truststore: these are public services
/// behind public certificates, and they have nothing to do with the rustak
/// server's own CA.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the TLS backend will not
/// initialise, which is not something an operator can do anything about.
pub fn http_client(
    user_agent: &str,
    timeout: Duration,
    headers: HeaderMap,
) -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(timeout)
        .default_headers(headers)
        .build()
        .or_system_err(&[
            "This usually means the TLS backend could not be initialised.",
            "Please report this issue to the development team via GitHub.",
        ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_builds() {
        assert!(http_client("rustak-test/0", Duration::from_secs(10), HeaderMap::new()).is_ok());
    }
}
