//! Deciding when to ping, and when to give up.
//!
//! ATAK's own constants, because a rustak client that behaves differently from
//! the reference client is a client whose disconnections nobody can reason
//! about (`compat/streaming.md` §6, `research/07` §3.6):
//!
//! * ping after **15 s** of inbound silence (`t-x-c-t`, `how="m-g"`, stale +10 s);
//! * repeat every **4.5 s** while the silence continues;
//! * declare the connection dead after **25 s** of silence and reconnect.
//!
//! "Silence" means nothing *received*. The server answers a ping with a pong
//! ([`rustak_cot::msgs::pong`]), so a healthy connection never reaches the
//! second ping, and a connection whose TCP socket is wedged open — the failure
//! this exists for, and the one a half-closed NAT produces — reaches 25 s and
//! is replaced.

use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::time::Duration;

use tokio::time::{Instant, Sleep, sleep_until};

/// The three keepalive intervals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keepalive {
    /// How long a connection may be silent before the first ping.
    pub idle: Duration,

    /// How often to ping while the silence continues.
    pub repeat: Duration,

    /// How long a connection may be silent before it is treated as dead.
    pub dead: Duration,
}

impl Keepalive {
    /// ATAK's constants: 15 s idle, 4.5 s repeat, dead at 25 s.
    pub const ATAK: Self = Self {
        idle: Duration::from_secs(15),
        repeat: Duration::from_millis(4_500),
        dead: Duration::from_secs(25),
    };

    /// No pings and no deadline — for a test that drives time itself, or a
    /// transport with its own liveness signal.
    pub const OFF: Self = Self {
        idle: Duration::ZERO,
        repeat: Duration::ZERO,
        dead: Duration::ZERO,
    };

    /// Whether this configuration does anything at all.
    #[must_use]
    pub const fn is_off(&self) -> bool {
        self.dead.is_zero()
    }
}

impl Default for Keepalive {
    fn default() -> Self {
        Self::ATAK
    }
}

/// What the keepalive wants the connection to do right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tick {
    /// Nothing to do; the timer has been armed and will wake the task.
    Quiet,

    /// Send a ping.
    Ping,

    /// Give up on this connection.
    Dead,
}

/// The live half of [`Keepalive`]: when the last message arrived, when the next
/// ping is due, and one timer that wakes the connection for whichever comes
/// first.
pub(crate) struct KeepaliveState {
    config: Keepalive,
    last_rx: Instant,
    next_ping: Instant,
    timer: Pin<Box<Sleep>>,
}

impl KeepaliveState {
    /// Starts the clock, as though a message had just arrived.
    pub(crate) fn new(config: Keepalive) -> Self {
        let now = Instant::now();

        Self {
            config,
            last_rx: now,
            next_ping: now + config.idle,
            timer: Box::pin(sleep_until(now + config.idle)),
        }
    }

    /// Records that something was received, whatever it was.
    ///
    /// A pong counts, an SA message counts and a message we could not parse
    /// counts: the question this answers is whether the peer is still there,
    /// not whether it is making sense.
    pub(crate) fn record_rx(&mut self) {
        self.last_rx = Instant::now();
        self.next_ping = self.last_rx + self.config.idle;
    }

    /// How long the connection has been silent.
    pub(crate) fn silence(&self) -> Duration {
        Instant::now().saturating_duration_since(self.last_rx)
    }

    /// Decides what to do, arming the timer for the next decision.
    ///
    /// `muted` suppresses pings without suspending the death clock: it is what
    /// the connection passes while a negotiation request is outstanding, when
    /// the protocol requires the client to send nothing at all. A server that
    /// answers nothing still gets 25 seconds, which is the right outcome — the
    /// reconnect that follows is how ATAK handles the same silence.
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>, muted: bool) -> Tick {
        if self.config.is_off() {
            return Tick::Quiet;
        }

        let now = Instant::now();
        let dead_at = self.last_rx + self.config.dead;

        if now >= dead_at {
            return Tick::Dead;
        }

        if !muted && now >= self.next_ping {
            self.next_ping = now + self.config.repeat;
            return Tick::Ping;
        }

        let wake_at = match muted {
            true => dead_at,
            false => dead_at.min(self.next_ping),
        };

        self.timer.as_mut().reset(wake_at);
        let _ = self.timer.as_mut().poll(cx);

        Tick::Quiet
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Waker;

    fn context() -> Context<'static> {
        Context::from_waker(Waker::noop())
    }

    #[tokio::test(start_paused = true)]
    async fn a_quiet_connection_pings_at_fifteen_seconds_then_every_four_and_a_half() {
        // The constants are ATAK's; a client that pings on its own schedule is
        // a client whose timeouts nobody can correlate with a real EUD's.
        let mut state = KeepaliveState::new(Keepalive::ATAK);

        assert_eq!(state.poll(&mut context(), false), Tick::Quiet);

        tokio::time::advance(Duration::from_secs(15)).await;
        assert_eq!(state.poll(&mut context(), false), Tick::Ping);
        assert_eq!(state.poll(&mut context(), false), Tick::Quiet);

        tokio::time::advance(Duration::from_millis(4_500)).await;
        assert_eq!(state.poll(&mut context(), false), Tick::Ping);
    }

    #[tokio::test(start_paused = true)]
    async fn twenty_five_seconds_of_silence_is_a_dead_connection() {
        let mut state = KeepaliveState::new(Keepalive::ATAK);

        tokio::time::advance(Duration::from_secs(24)).await;
        assert_ne!(state.poll(&mut context(), false), Tick::Dead);

        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(state.poll(&mut context(), false), Tick::Dead);
        assert!(state.silence() >= Duration::from_secs(25));
    }

    #[tokio::test(start_paused = true)]
    async fn anything_received_restarts_both_clocks() {
        let mut state = KeepaliveState::new(Keepalive::ATAK);

        tokio::time::advance(Duration::from_secs(14)).await;
        state.record_rx();
        tokio::time::advance(Duration::from_secs(14)).await;

        // 28 s after connecting, but only 14 s since the last message: neither
        // the ping nor the death clock has fired.
        assert_eq!(state.poll(&mut context(), false), Tick::Quiet);
        assert!(state.silence() < Duration::from_secs(15));
    }

    #[tokio::test(start_paused = true)]
    async fn a_muted_connection_does_not_ping_but_can_still_die() {
        // Negotiation requires the client to write nothing between its request
        // and the response. It does not require it to wait forever.
        let mut state = KeepaliveState::new(Keepalive::ATAK);

        tokio::time::advance(Duration::from_secs(20)).await;
        assert_eq!(state.poll(&mut context(), true), Tick::Quiet);

        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(state.poll(&mut context(), true), Tick::Dead);
    }

    #[tokio::test(start_paused = true)]
    async fn a_keepalive_that_is_off_never_fires() {
        let mut state = KeepaliveState::new(Keepalive::OFF);

        tokio::time::advance(Duration::from_secs(600)).await;

        assert_eq!(state.poll(&mut context(), false), Tick::Quiet);
        assert!(Keepalive::OFF.is_off());
        assert!(!Keepalive::default().is_off());
    }
}
