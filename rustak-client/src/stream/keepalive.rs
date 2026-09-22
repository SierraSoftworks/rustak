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
//! "Silence" there means nothing *received*. The server answers a ping with a
//! pong ([`rustak_cot::msgs::pong`]), so a healthy connection never reaches the
//! second ping, and a connection whose TCP socket is wedged open — the failure
//! this exists for, and the one a half-closed NAT produces — reaches 25 s and
//! is replaced.
//!
//! # One rule of our own: outbound silence
//!
//! ATAK's rule alone leaves a receive-only client mute. A sidecar that
//! subscribes to a busy channel and has nothing to publish — FIRMS with no
//! fires in Ireland — hears traffic constantly, so it is never *inbound*-silent,
//! so it never pings, so it never sends a byte. A server that measures idleness
//! from reads alone then drops it on a timer, which is exactly what rustak did
//! to that sidecar every two to four minutes (M9-15).
//!
//! So this client also pings after [`outbound_idle`](Keepalive::outbound_idle)
//! — **30 s** by default — of having *written* nothing. Any frame written
//! resets that clock, pings included, so a client that publishes regularly
//! never sends one. It costs a message every half minute on an otherwise
//! silent connection and it keeps every receive-only sidecar alive against any
//! server with a read-idle rule, including rustak builds from before the fix
//! and TAK Server itself.
//!
//! The **death** clock is untouched by all this: it is inbound silence only,
//! because what it answers is whether the *server* is still there.

use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::time::Duration;

use tokio::time::{Instant, Sleep, sleep_until};

/// The keepalive intervals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keepalive {
    /// How long a connection may go without *receiving* before the first ping.
    pub idle: Duration,

    /// How often to ping while the inbound silence continues.
    pub repeat: Duration,

    /// How long a connection may go without receiving before it is treated as
    /// dead. Inbound only: this is a question about the server.
    pub dead: Duration,

    /// How long this client may go without *sending* before it pings.
    ///
    /// [`Duration::ZERO`] switches it off. Any frame written resets it, pings
    /// included, so a client that publishes regularly never sends one.
    ///
    /// It exists because ATAK's rules above are all keyed on what has been
    /// *received*: a client that hears a busy channel and has nothing of its
    /// own to publish is never inbound-silent, so it never pings and never
    /// writes a byte — and a server that measures idleness from reads alone
    /// drops it on a timer. This keeps such a client alive against any server
    /// with a read-idle rule.
    pub outbound_idle: Duration,
}

impl Keepalive {
    /// How long a quiet client waits before reminding the server it is there.
    pub const OUTBOUND_IDLE: Duration = Duration::from_secs(30);

    /// ATAK's constants, exactly: 15 s idle, 4.5 s repeat, dead at 25 s, and
    /// no outbound rule at all.
    ///
    /// What a conformance test compares against. An ordinary client wants
    /// [`DEFAULT`](Self::DEFAULT), which is this plus the outbound ping.
    pub const ATAK: Self = Self {
        idle: Duration::from_secs(15),
        repeat: Duration::from_millis(4_500),
        dead: Duration::from_secs(25),
        outbound_idle: Duration::ZERO,
    };

    /// What a connection uses unless it says otherwise: ATAK's inbound rules,
    /// plus a ping after 30 s of having written nothing.
    pub const DEFAULT: Self = Self {
        outbound_idle: Self::OUTBOUND_IDLE,
        ..Self::ATAK
    };

    /// No pings and no deadline — for a test that drives time itself, or a
    /// transport with its own liveness signal.
    pub const OFF: Self = Self {
        idle: Duration::ZERO,
        repeat: Duration::ZERO,
        dead: Duration::ZERO,
        outbound_idle: Duration::ZERO,
    };

    /// Whether this configuration does anything at all.
    #[must_use]
    pub const fn is_off(&self) -> bool {
        self.dead.is_zero() && self.idle.is_zero() && self.outbound_idle.is_zero()
    }
}

impl Default for Keepalive {
    fn default() -> Self {
        Self::DEFAULT
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

/// The live half of [`Keepalive`]: when the last message arrived, when the last
/// one went out, when the next ping is due, and one timer that wakes the
/// connection for whichever comes first.
pub(crate) struct KeepaliveState {
    config: Keepalive,
    last_rx: Instant,
    next_ping: Instant,
    /// When the outbound rule next fires, when there is one.
    next_tx_ping: Option<Instant>,
    timer: Pin<Box<Sleep>>,
}

impl KeepaliveState {
    /// Starts both clocks, as though a message had just arrived and one had
    /// just gone out.
    pub(crate) fn new(config: Keepalive) -> Self {
        let now = Instant::now();

        Self {
            config,
            last_rx: now,
            next_ping: now + config.idle,
            next_tx_ping: outbound_ping_at(now, config),
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

    /// Records that a frame was written, whatever it was.
    ///
    /// A ping counts, which is what makes the outbound rule one ping every
    /// [`outbound_idle`](Keepalive::outbound_idle) rather than a flood.
    pub(crate) fn record_tx(&mut self) {
        self.next_tx_ping = outbound_ping_at(Instant::now(), self.config);
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
        let dead_at = match self.config.dead.is_zero() {
            true => None,
            false => Some(self.last_rx + self.config.dead),
        };

        if dead_at.is_some_and(|at| now >= at) {
            return Tick::Dead;
        }

        if !muted {
            if !self.config.idle.is_zero() && now >= self.next_ping {
                self.next_ping = now + self.config.repeat;
                return Tick::Ping;
            }

            if self.next_tx_ping.is_some_and(|at| now >= at) {
                // Throttled on `repeat`, not on `outbound_idle`: a connection
                // whose socket will not take the ping must not be handed
                // another one on every poll. The write itself calls
                // `record_tx`, which is what normally sets the next one.
                self.next_tx_ping = Some(now + self.config.repeat);
                return Tick::Ping;
            }
        }

        // Whichever deadline comes first, and nothing at all when a muted
        // connection has no death clock either: there is then nothing for a
        // timer to wake us for, and the socket will do it.
        let next_ping = (!self.config.idle.is_zero()).then_some(self.next_ping);
        let wake_at = match muted {
            true => dead_at,
            false => [dead_at, next_ping, self.next_tx_ping]
                .into_iter()
                .flatten()
                .min(),
        };

        if let Some(wake_at) = wake_at {
            self.timer.as_mut().reset(wake_at);
            let _ = self.timer.as_mut().poll(cx);
        }

        Tick::Quiet
    }
}

/// When a connection that writes nothing from `now` would next ping.
fn outbound_ping_at(now: Instant, config: Keepalive) -> Option<Instant> {
    match config.outbound_idle.is_zero() {
        true => None,
        false => Some(now + config.outbound_idle),
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

    #[tokio::test(start_paused = true)]
    async fn a_client_that_hears_everything_and_says_nothing_still_pings() {
        // M9-15. The receive-only case: a sidecar subscribed to a busy channel
        // with nothing of its own to publish is never *inbound* silent, so
        // ATAK's rule alone never fires and the client never writes a byte.
        // Every ten seconds a message arrives; every thirty seconds — of
        // having written nothing — this client pings anyway.
        let mut state = KeepaliveState::new(Keepalive::DEFAULT);
        let mut pings = 0;

        for second in 1..=120 {
            tokio::time::advance(Duration::from_secs(1)).await;

            if second % 10 == 0 {
                state.record_rx();
            }

            if state.poll(&mut context(), false) == Tick::Ping {
                pings += 1;
                // What the connection does with a ping it has written.
                state.record_tx();
            }

            assert_ne!(
                state.poll(&mut context(), false),
                Tick::Dead,
                "a connection hearing traffic is never dead",
            );
        }

        assert_eq!(
            pings, 4,
            "one ping per 30s of outbound silence over two minutes, and no more",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn anything_sent_puts_the_outbound_ping_off_by_another_thirty_seconds() {
        let mut state = KeepaliveState::new(Keepalive::DEFAULT);

        tokio::time::advance(Duration::from_secs(29)).await;
        state.record_rx();
        assert_eq!(state.poll(&mut context(), false), Tick::Quiet);

        // The caller publishes something one second before the ping was due.
        state.record_tx();

        tokio::time::advance(Duration::from_secs(29)).await;
        state.record_rx();
        assert_eq!(
            state.poll(&mut context(), false),
            Tick::Quiet,
            "the clock restarted when the client wrote",
        );

        tokio::time::advance(Duration::from_secs(1)).await;
        state.record_rx();
        assert_eq!(state.poll(&mut context(), false), Tick::Ping);
    }

    #[tokio::test(start_paused = true)]
    async fn the_outbound_rule_leaves_atak_s_own_rules_exactly_as_they_were() {
        // `compat/streaming.md` §6 is ATAK's, and the outbound ping is ours:
        // a client that hears nothing must still ping at 15s, repeat at 4.5s
        // and die at 25s, whichever configuration it is running.
        let mut state = KeepaliveState::new(Keepalive::DEFAULT);

        tokio::time::advance(Duration::from_secs(15)).await;
        assert_eq!(state.poll(&mut context(), false), Tick::Ping);
        state.record_tx();

        tokio::time::advance(Duration::from_millis(4_500)).await;
        assert_eq!(state.poll(&mut context(), false), Tick::Ping);
        state.record_tx();

        tokio::time::advance(Duration::from_millis(5_500)).await;
        assert_eq!(
            state.poll(&mut context(), false),
            Tick::Dead,
            "25s without hearing anything, however much we have written",
        );
    }

    #[test]
    fn the_shipped_default_is_atak_plus_the_outbound_rule() {
        assert_eq!(Keepalive::DEFAULT.idle, Keepalive::ATAK.idle);
        assert_eq!(Keepalive::DEFAULT.repeat, Keepalive::ATAK.repeat);
        assert_eq!(Keepalive::DEFAULT.dead, Keepalive::ATAK.dead);
        assert_eq!(Keepalive::DEFAULT.outbound_idle, Duration::from_secs(30));
        assert!(
            Keepalive::ATAK.outbound_idle.is_zero(),
            "ATAK's own constants stay ATAK's own, for whatever compares against them",
        );
        assert_eq!(Keepalive::default(), Keepalive::DEFAULT);
    }
}
