//! Where the tracks come from.

use async_trait::async_trait;
use human_errors::Error;

use super::Track;

/// An upstream a plugin reads observations from.
///
/// # The contract
///
/// [`poll`](Feed::poll) is called once per sidecar tick and answers **what has
/// arrived since the last call**. It may wait, but never for longer than a tick
/// is worth: a source that blocks holds up the plugin's heartbeat and its
/// shutdown alike. A streaming upstream (an AIS WebSocket, say) therefore runs
/// its own task, buffers what arrives, and drains that buffer here; a polling
/// one (an `aircraft.json`, an aggregator's HTTP endpoint) makes its request
/// here and answers what it got.
///
/// # Failure is expected and is not fatal
///
/// An upstream that is down, rate-limiting or talking nonsense is an ordinary
/// Tuesday for an open feed. Answering an error says only "this poll produced
/// nothing and here is why": the plugin logs it and carries on, tracks age out
/// on their own `stale`, and the next poll tries again. **Reconnection and
/// backoff belong to the source**, which is the only thing that knows whether
/// its upstream wants a new socket, a new token or simply another minute.
#[async_trait]
pub trait Feed: Send {
    /// What to call this source in a log line — the upstream's name, not the
    /// plugin's: `aisstream.io`, `adsb.lol`, `replay`.
    fn name(&self) -> &str;

    /// The observations seen since the last call.
    ///
    /// # Errors
    ///
    /// Whatever went wrong upstream, for the plugin to log. It is never a
    /// reason to stop the sidecar.
    async fn poll(&mut self) -> Result<Vec<Track>, Error>;
}
