//! Whether a connection is still there, and why it stopped being there.
//!
//! Two questions one [`Liveness`] answers, because both are asked by the same
//! pair of tasks about the same socket.
//!
//! # Idle means idle in both directions
//!
//! A TAK client pings after fifteen seconds of **inbound** silence and not
//! otherwise (`compat/streaming.md` §6): a receive-only client on a busy
//! server — a sidecar with nothing to publish, an operator watching a map —
//! hears traffic constantly, so it never pings and never writes. A server that
//! measured idleness from reads alone would drop exactly the clients that are
//! working correctly, which is what rustak did to the FIRMS sidecar every two
//! to four minutes until M9-15.
//!
//! So the idle clock is `max(last_rx, last_tx)`: a connection is reclaimed only
//! when nothing has arrived from it **and** nothing has been written to it for
//! `[stream.tls] idle_timeout`. Writing is not proof that the peer is reading —
//! but proving that is `[stream.limits] write_timeout`'s job, which fires when
//! the socket stops taking bytes, and the client's own 25-second death clock's
//! job on the other end. The idle timer is for a connection where *nothing at
//! all* is happening.
//!
//! # Every close names a cause
//!
//! [`LeaveReason`] is recorded once, by whichever task noticed first, and read
//! by `connection::run` for the one `info` line a disconnect produces. First
//! cause wins: a peer that vanishes produces a write timeout *and* a read
//! error, and the second is a consequence of the first.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use tokio::time::Instant;

/// Why a connection ended.
///
/// Every close path in the stream listener sets one of these — the variants
/// are the causes that exist in the code, not a wish list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LeaveReason {
    /// The client closed its end, or the socket reached end-of-file.
    ClientClosed = 1,
    /// Nothing was received **and** nothing was written for `idle_timeout`.
    Idle = 2,
    /// Reading from the socket failed.
    ReadError = 3,
    /// The peer stopped taking bytes and overran `write_timeout`.
    WriteTimeout = 4,
    /// Writing to the socket failed.
    WriteError = 5,
    /// The connection fell `close_after_drops` deliveries behind.
    SlowConsumer = 6,
    /// The certificate it authenticated with was revoked.
    Revoked = 7,
    /// The account it belongs to was switched off.
    AccountDisabled = 8,
    /// An administrator closed it.
    Administrator = 9,
    /// The server is stopping.
    Shutdown = 10,
}

impl LeaveReason {
    /// Every cause, in declaration order. The counters are indexed by it.
    pub const ALL: [Self; 10] = [
        Self::ClientClosed,
        Self::Idle,
        Self::ReadError,
        Self::WriteTimeout,
        Self::WriteError,
        Self::SlowConsumer,
        Self::Revoked,
        Self::AccountDisabled,
        Self::Administrator,
        Self::Shutdown,
    ];

    /// What the `reason` field of the disconnect line reads.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClientClosed => "client_closed",
            Self::Idle => "idle",
            Self::ReadError => "read_error",
            Self::WriteTimeout => "write_timeout",
            Self::WriteError => "write_error",
            Self::SlowConsumer => "slow_consumer",
            Self::Revoked => "revoked",
            Self::AccountDisabled => "account_disabled",
            Self::Administrator => "administrator",
            Self::Shutdown => "shutdown",
        }
    }

    /// Where this cause's counter lives.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize - 1
    }

    /// The cause a stored discriminant names, if it is one.
    fn from_u8(stored: u8) -> Option<Self> {
        let index = usize::from(stored).checked_sub(1)?;

        Self::ALL.get(index).copied()
    }
}

impl std::fmt::Display for LeaveReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One connection's clocks, and the cause of its death.
///
/// Shared between the connection's reader, its writer and anything that closes
/// it from outside. Relaxed atomics throughout: the clocks are advisory to the
/// millisecond and the reason is written once.
#[derive(Debug)]
pub struct Liveness {
    /// When the connection was accepted; both clocks are offsets from it.
    started: Instant,
    /// Milliseconds after [`started`](Self::started) at the last frame read.
    last_rx: AtomicU64,
    /// Milliseconds after [`started`](Self::started) at the last completed
    /// write.
    last_tx: AtomicU64,
    /// A [`LeaveReason`] discriminant, or zero while the connection is up.
    reason: AtomicU8,
}

impl Default for Liveness {
    fn default() -> Self {
        Self::new()
    }
}

impl Liveness {
    /// Starts both clocks, as though the connection had just been used.
    #[must_use]
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            last_rx: AtomicU64::new(0),
            last_tx: AtomicU64::new(0),
            reason: AtomicU8::new(0),
        }
    }

    /// Records that a frame arrived from the client.
    pub fn received(&self) {
        self.last_rx
            .fetch_max(self.elapsed_millis(), Ordering::Relaxed);
    }

    /// Records that a write to the client completed.
    ///
    /// Called by the writer when a flush returns, which is the point at which
    /// the bytes have left this process.
    pub fn wrote(&self) {
        self.last_tx
            .fetch_max(self.elapsed_millis(), Ordering::Relaxed);
    }

    /// When this connection may next be reclaimed for idleness.
    ///
    /// Measured from the **later** of the two clocks; see the module
    /// documentation for why writing counts.
    #[must_use]
    pub fn idle_deadline(&self, idle_timeout: Duration) -> Instant {
        let last = self
            .last_rx
            .load(Ordering::Relaxed)
            .max(self.last_tx.load(Ordering::Relaxed));

        self.started + Duration::from_millis(last) + idle_timeout
    }

    /// Whether nothing has happened in either direction for `idle_timeout`.
    #[must_use]
    pub fn is_idle(&self, idle_timeout: Duration) -> bool {
        Instant::now() >= self.idle_deadline(idle_timeout)
    }

    /// How long the connection has been up.
    #[must_use]
    pub fn connected_for(&self) -> Duration {
        Instant::now().saturating_duration_since(self.started)
    }

    /// Records why the connection is ending, and answers the cause that
    /// stands.
    ///
    /// The first cause wins, because everything after it is a consequence of
    /// it: a client that vanishes mid-flush produces a write timeout and then
    /// a read error, and only the first says anything an operator can act on.
    pub fn ended(&self, reason: LeaveReason) -> LeaveReason {
        match self
            .reason
            .compare_exchange(0, reason as u8, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => reason,
            Err(stored) => LeaveReason::from_u8(stored).unwrap_or(reason),
        }
    }

    /// Why the connection is ending, if anything has said yet.
    #[must_use]
    pub fn reason(&self) -> Option<LeaveReason> {
        LeaveReason::from_u8(self.reason.load(Ordering::SeqCst))
    }

    /// Milliseconds since the connection was accepted.
    fn elapsed_millis(&self) -> u64 {
        u64::try_from(self.connected_for().as_millis()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_connection_being_written_to_is_not_idle_however_quiet_the_client_is() {
        // The production fault this module exists for: FIRMS receives the
        // channel's traffic and has nothing to publish, so it sends nothing at
        // all and a read-idle timer reclaims it while the server is happily
        // writing to it.
        let idle = Duration::from_secs(90);
        let liveness = Liveness::new();

        for _ in 0..10 {
            tokio::time::advance(Duration::from_secs(30)).await;
            liveness.wrote();

            assert!(
                !liveness.is_idle(idle),
                "a connection written to 30s ago has not been idle for 90s",
            );
        }

        // Five minutes on the connection, and not one byte received.
        assert!(liveness.connected_for() >= Duration::from_secs(300));
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_with_no_traffic_either_way_goes_idle_on_time() {
        let idle = Duration::from_secs(90);
        let liveness = Liveness::new();

        tokio::time::advance(Duration::from_secs(89)).await;
        assert!(!liveness.is_idle(idle));

        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(
            liveness.is_idle(idle),
            "90s with nothing in either direction"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn receiving_moves_the_deadline_exactly_as_writing_does() {
        let idle = Duration::from_secs(90);
        let liveness = Liveness::new();

        tokio::time::advance(Duration::from_secs(60)).await;
        liveness.received();
        let after_rx = liveness.idle_deadline(idle);

        tokio::time::advance(Duration::from_secs(60)).await;
        liveness.wrote();

        assert!(!liveness.is_idle(idle));
        assert_eq!(
            liveness.idle_deadline(idle) - after_rx,
            Duration::from_secs(60),
            "the deadline follows whichever clock moved last",
        );
    }

    #[test]
    fn the_first_cause_is_the_one_that_stands() {
        let liveness = Liveness::new();

        assert_eq!(liveness.reason(), None);
        assert_eq!(
            liveness.ended(LeaveReason::WriteTimeout),
            LeaveReason::WriteTimeout,
        );
        assert_eq!(
            liveness.ended(LeaveReason::ReadError),
            LeaveReason::WriteTimeout,
            "the read error is a consequence of the peer that stopped reading",
        );
        assert_eq!(liveness.reason(), Some(LeaveReason::WriteTimeout));
    }

    #[test]
    fn every_cause_has_its_own_name_and_its_own_counter_slot() {
        let names: Vec<&str> = LeaveReason::ALL.iter().map(|r| r.as_str()).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();

        assert_eq!(names.len(), unique.len(), "two causes share a name");

        for (index, reason) in LeaveReason::ALL.iter().enumerate() {
            assert_eq!(reason.index(), index);
            assert_eq!(LeaveReason::from_u8(*reason as u8), Some(*reason));
            assert_eq!(reason.to_string(), reason.as_str());
        }

        assert_eq!(LeaveReason::from_u8(0), None, "zero means still connected");
        assert_eq!(LeaveReason::from_u8(200), None);
    }
}
