//! What the stream listener counts.
//!
//! One shared [`StreamMetrics`] per listener, incremented with relaxed atomics
//! from every connection task. Counters rather than gauges wherever possible:
//! a counter that is only ever added to can be read from another thread without
//! coordination, and the difference between two readings is the rate.
//!
//! # Why the drops are counted separately
//!
//! "Messages dropped" is four different operational problems wearing one name.
//! A message dropped because a queue was full means a device is not keeping up;
//! one dropped for a flow tag means two servers are bridged and the loop
//! suppression is working; one dropped at parse means somebody is sending
//! malformed CoT; one dropped for having no recipients means the channel
//! memberships are not what an operator thinks they are. Collapsing them would
//! make every one of those look like the others.

use std::sync::atomic::{AtomicU64, Ordering};

/// The counters one stream listener keeps.
#[derive(Debug, Default)]
pub struct StreamMetrics {
    /// Connections accepted since start-up.
    pub accepted: AtomicU64,
    /// Connections that never became a subscription — a handshake that failed,
    /// a certificate we would not resolve to an account.
    pub rejected: AtomicU64,
    /// Subscriptions currently registered.
    pub connected: AtomicU64,
    /// Messages read from clients and understood.
    pub rx_msgs: AtomicU64,
    /// Deliveries handed to a connection's writer.
    pub tx_msgs: AtomicU64,
    /// Deliveries discarded because a connection's queue was full.
    pub dropped_queue: AtomicU64,
    /// Inbound messages that would not parse, or carried no `<point>`.
    pub dropped_parse: AtomicU64,
    /// Inbound messages already carrying this server's flow tag.
    pub dropped_flowtag: AtomicU64,
    /// Inbound messages from an incognito subscription with no explicit
    /// recipient.
    pub dropped_incognito: AtomicU64,
    /// Messages nobody was allowed to receive.
    pub no_recipients: AtomicU64,
    /// Undeliverable GeoChats handed back to their sender as `b-t-f-s`.
    ///
    /// A subset of [`no_recipients`](Self::no_recipients), and the one an
    /// operator can act on: it counts people typing to somebody who is not
    /// there, which is either a device that has dropped off or a channel
    /// membership that does not match the contact list somebody is reading.
    pub chat_bounced: AtomicU64,
    /// Bytes skipped resynchronising a protobuf stream.
    pub proto_resyncs: AtomicU64,
    /// Outbound messages replaced by a `b-f-t-r` pointer for being too large.
    pub oversize_substituted: AtomicU64,
    /// Connections closed for falling too far behind.
    pub closed_slow: AtomicU64,
    /// Cached peer positions a new connection never received.
    ///
    /// Separate from [`dropped_queue`](Self::dropped_queue) because it means
    /// something different and is acted on differently: a replay drop is a
    /// client whose map started out incomplete, and the number of them is how
    /// far `[stream.limits] queue_len` is below the fleet size. Non-zero here
    /// with `dropped_queue` at zero is a configuration to change, not a network
    /// to investigate. R-03 C1.
    pub replay_dropped: AtomicU64,
    /// Writer tasks that would not stop on their own and were aborted.
    ///
    /// Each one is a socket and a TLS session reclaimed from a peer that
    /// stopped reading — see `[stream.limits] write_timeout`. A steady rate is
    /// ordinary on a mobile fleet; a rate that tracks the connection count is a
    /// network path that is black-holing.
    pub writer_aborted: AtomicU64,
    /// `<dest>` elements discarded for being past the per-message cap.
    ///
    /// Only a client that is broken or hostile reaches this: no real one
    /// addresses more than a handful of people. R-03 H2.
    pub dests_truncated: AtomicU64,
}

impl StreamMetrics {
    /// Adds one to a counter.
    pub fn incr(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Adds `by` to a counter.
    pub fn add(counter: &AtomicU64, by: u64) {
        if by > 0 {
            counter.fetch_add(by, Ordering::Relaxed);
        }
    }

    /// Takes one off a counter, never wrapping past zero.
    pub fn decr(counter: &AtomicU64) {
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            Some(value.saturating_sub(1))
        });
    }

    /// Reads a counter.
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_start_at_zero_and_add_up() {
        let metrics = StreamMetrics::default();

        StreamMetrics::incr(&metrics.rx_msgs);
        StreamMetrics::add(&metrics.rx_msgs, 4);

        assert_eq!(StreamMetrics::get(&metrics.rx_msgs), 5);
        assert_eq!(StreamMetrics::get(&metrics.tx_msgs), 0);
    }

    #[test]
    fn the_connection_gauge_never_wraps_below_zero() {
        // `connected` is decremented on every disconnect, including the ones
        // that never finished registering; wrapping would make the gauge read
        // eighteen quintillion and send somebody looking for a leak.
        let metrics = StreamMetrics::default();

        StreamMetrics::decr(&metrics.connected);

        assert_eq!(StreamMetrics::get(&metrics.connected), 0);
    }

    #[test]
    fn adding_nothing_is_not_a_write() {
        let metrics = StreamMetrics::default();

        StreamMetrics::add(&metrics.proto_resyncs, 0);

        assert_eq!(StreamMetrics::get(&metrics.proto_resyncs), 0);
    }
}
