//! How long the kernel waits before it gives up on a stream peer.
//!
//! # Why the kernel has to be asked
//!
//! The idle rule is `max(last_rx, last_tx)` ([`Liveness`]), and a completed
//! write only proves the bytes reached *our* kernel. A phone that drops off the
//! network on a busy channel therefore keeps its connection — and shows as
//! connected in contacts — until the send buffer fills and `write_timeout`
//! fires, or the kernel gives up retransmitting: `tcp_retries2 = 15`, which
//! Linux's `ip-sysctl` documentation puts at about 924.6 seconds. M10-02.
//!
//! The cure is the transport's own: keepalive probes on a quiet socket, and on
//! Linux a bound on how long transmitted data may go unacknowledged. Both are
//! sized from `idle_timeout` by [`PeerProbe::for_idle_timeout`], so that a peer
//! which has vanished is reclaimed in roughly `idle_timeout` whether or not
//! the server is writing to it — and without the server sending a TAK ping,
//! which no TAK server does (`compat/streaming.md` §6).
//!
//! # What each platform does with them
//!
//! - **Linux.** `SO_KEEPALIVE` with `TCP_KEEPIDLE`, `TCP_KEEPINTVL` and
//!   `TCP_KEEPCNT`, plus `TCP_USER_TIMEOUT` (RFC 5482; `tcp(7)`): the longest
//!   transmitted data may stay unacknowledged, or buffered data unsent behind
//!   a zero window, before the connection is closed with `ETIMEDOUT`. Linux's
//!   keepalive timer does not probe while data is outstanding, which is why a
//!   peer that vanished under outbound traffic needs the user timeout and not
//!   keepalive. `tcp(7)` also says that with keepalive on, `TCP_USER_TIMEOUT`
//!   *overrides* keepalive in deciding when to close: once a probe has gone
//!   unanswered and the user timeout has passed since anything was received,
//!   the connection is dropped, and `TCP_KEEPCNT` no longer counts. The two
//!   are sized so the answer is the same either way.
//! - **macOS.** `TCP_KEEPALIVE` (the idle time), `TCP_KEEPINTVL` and
//!   `TCP_KEEPCNT` exist; `TCP_USER_TIMEOUT` does not. A silent vanished peer
//!   is reclaimed by keepalive; one under outbound traffic by `write_timeout`
//!   or the system's retransmission limit.
//! - **Windows.** `SIO_KEEPALIVE_VALS` sets the idle time and the interval;
//!   `TCP_KEEPCNT` sets the count on Windows 10 1703 and later, and the count
//!   is otherwise fixed at ten. No user timeout either.
//! - **Anywhere else.** `SO_KEEPALIVE` alone, on the system's own timers
//!   (commonly two hours), or nothing. What stands then is what stood before
//!   M10-02: `idle_timeout` for a silent peer, `write_timeout` and the
//!   retransmission limit for one under traffic.
//!
//! When the kernel does give up it reports `ETIMEDOUT` on the next read or
//! write, and [`cause`] turns that into [`LeaveReason::PeerTimeout`] so the
//! disconnect line says so rather than blaming the socket.
//!
//! [`Liveness`]: super::liveness::Liveness

use std::io;
use std::time::Duration;

use rustak_cot::error::CodecError;

use super::liveness::LeaveReason;

/// The shortest a vanished peer is ever given.
///
/// Below this a phone crossing a coverage gap — seconds of lost packets on a
/// link that comes back — would lose its connection to a probe that was never
/// going to be answered in time. The stream's integration tests run with
/// sub-second idle timeouts; they get this.
pub const FLOOR: Duration = Duration::from_secs(10);

/// The longest, however long `idle_timeout` is.
///
/// Linux's own retransmission limit (about 924.6 s at the default
/// `tcp_retries2`), rounded down: an installation with a very long idle
/// timeout is never made *worse* at noticing a vanished peer than the kernel
/// would be on its own.
pub const CEILING: Duration = Duration::from_secs(900);

/// How many unanswered keepalive probes end a connection.
///
/// Enough that one lost probe is not a death; few enough that the interval
/// between them is still a meaningful fraction of the bound.
pub const RETRIES: u32 = 3;

/// The kernel settings one stream socket is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerProbe {
    /// How long the socket must be quiet before the first keepalive probe.
    pub keepalive_idle: Duration,
    /// How long between unanswered probes.
    pub keepalive_interval: Duration,
    /// How many unanswered probes end the connection (where settable).
    pub keepalive_retries: u32,
    /// How long transmitted data may go unacknowledged (Linux only).
    pub user_timeout: Duration,
}

impl PeerProbe {
    /// The settings that reclaim a vanished peer in roughly `idle_timeout`.
    ///
    /// The bound is `idle_timeout` clamped to [`FLOOR`]..=[`CEILING`], in whole
    /// seconds because that is what the keepalive options take. Half of it is
    /// quiet before the first probe and the rest is spent on [`RETRIES`]
    /// probes, so `keepalive_idle + keepalive_interval × keepalive_retries` is
    /// at most the bound; the user timeout *is* the bound.
    ///
    /// At the default 90 s: first probe after 45 s, then every 15 s, three of
    /// them, gone at 90 s.
    #[must_use]
    pub fn for_idle_timeout(idle_timeout: Duration) -> Self {
        let bound = idle_timeout.clamp(FLOOR, CEILING).as_secs();
        let idle = bound / 2;
        let interval = ((bound - idle) / u64::from(RETRIES)).max(1);

        Self {
            keepalive_idle: Duration::from_secs(idle),
            keepalive_interval: Duration::from_secs(interval),
            keepalive_retries: RETRIES,
            user_timeout: Duration::from_secs(bound),
        }
    }

    /// The longest a vanished, silent peer lasts on a platform that honours
    /// all three keepalive settings.
    #[must_use]
    pub fn keepalive_bound(&self) -> Duration {
        self.keepalive_idle + self.keepalive_interval * self.keepalive_retries
    }
}

/// What a failed read or write says about why the connection ended.
///
/// `ETIMEDOUT` is the kernel's verdict that the peer is gone — a keepalive
/// probe or retransmitted data went unanswered for as long as
/// [`PeerProbe`] allows — and is named as such. Anything else is the cause
/// the caller already had in mind.
#[must_use]
pub fn cause(error: &CodecError, otherwise: LeaveReason) -> LeaveReason {
    match error {
        CodecError::Io(io) if io.kind() == io::ErrorKind::TimedOut => LeaveReason::PeerTimeout,
        _ => otherwise,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_idle_timeout_reclaims_a_vanished_peer_in_ninety_seconds() {
        let probe = PeerProbe::for_idle_timeout(Duration::from_secs(90));

        assert_eq!(
            probe,
            PeerProbe {
                keepalive_idle: Duration::from_secs(45),
                keepalive_interval: Duration::from_secs(15),
                keepalive_retries: 3,
                user_timeout: Duration::from_secs(90),
            }
        );
        assert_eq!(probe.keepalive_bound(), Duration::from_secs(90));
    }

    #[test]
    fn a_very_short_idle_timeout_is_held_to_the_floor() {
        for short in [
            Duration::ZERO,
            Duration::from_millis(500),
            Duration::from_secs(2),
            Duration::from_secs(10),
        ] {
            let probe = PeerProbe::for_idle_timeout(short);

            assert_eq!(probe.user_timeout, FLOOR, "{short:?}");
            assert_eq!(probe.keepalive_idle, Duration::from_secs(5), "{short:?}");
            assert_eq!(
                probe.keepalive_interval,
                Duration::from_secs(1),
                "{short:?}"
            );
            assert!(probe.keepalive_bound() <= FLOOR, "{short:?}");
        }
    }

    #[test]
    fn a_very_long_idle_timeout_is_never_worse_than_the_kernel_alone() {
        for long in [
            Duration::from_secs(900),
            Duration::from_secs(3600),
            Duration::from_secs(86_400 * 365),
            Duration::MAX,
        ] {
            let probe = PeerProbe::for_idle_timeout(long);

            assert_eq!(probe.user_timeout, CEILING, "{long:?}");
            assert_eq!(probe.keepalive_idle, Duration::from_secs(450));
            assert_eq!(probe.keepalive_interval, Duration::from_secs(150));
            assert_eq!(probe.keepalive_bound(), CEILING);
        }
    }

    #[test]
    fn the_probes_never_outlast_the_bound_and_never_fire_back_to_back() {
        for secs in 0..=1_000 {
            let probe = PeerProbe::for_idle_timeout(Duration::from_secs(secs));

            assert!(probe.keepalive_bound() <= probe.user_timeout, "{secs}s");
            assert!(probe.keepalive_idle >= Duration::from_secs(5), "{secs}s");
            assert!(
                probe.keepalive_interval >= Duration::from_secs(1),
                "{secs}s"
            );
            assert!(probe.user_timeout >= FLOOR && probe.user_timeout <= CEILING);
        }
    }

    #[test]
    fn the_kernel_giving_up_on_a_peer_is_named_as_such() {
        let timed_out = CodecError::Io(io::Error::from(io::ErrorKind::TimedOut));

        assert_eq!(
            cause(&timed_out, LeaveReason::ReadError),
            LeaveReason::PeerTimeout
        );
        assert_eq!(
            cause(&timed_out, LeaveReason::WriteError),
            LeaveReason::PeerTimeout
        );

        // A reset, a broken pipe: the socket failed, but nothing timed out.
        for other in [io::ErrorKind::ConnectionReset, io::ErrorKind::BrokenPipe] {
            let failed = CodecError::Io(io::Error::from(other));

            assert_eq!(
                cause(&failed, LeaveReason::ReadError),
                LeaveReason::ReadError
            );
            assert_eq!(
                cause(&failed, LeaveReason::WriteError),
                LeaveReason::WriteError
            );
        }
    }
}
