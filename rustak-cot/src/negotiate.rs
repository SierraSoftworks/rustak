//! TAK Protocol v1 negotiation: `t-x-takp-v`, `t-x-takp-q`, `t-x-takp-r`.
//!
//! The exchange is three ordinary CoT events carrying a `<TakControl>` detail,
//! and it happens exactly once per connection, in this order:
//!
//! 1. after authentication and after the latest-SA replay, the server sends
//!    **one** [`announce`] naming the versions it supports;
//! 2. a client that wants protobuf answers with [`request`], **reusing the
//!    announcement's uid**, and then sends nothing until it hears back;
//! 3. the server replies with [`response`], again on the same uid. On
//!    `status="true"` both directions switch to protobuf framing the moment
//!    that response is written, and the server never emits XML again.
//!
//! Silence is a valid move and the fallback everything else depends on: a
//! client that never asks — CloudTAK never does — stays on XML forever, and a
//! request whose version node is missing or unreadable gets **no answer at
//! all** rather than a rejection. [`parse_request`] returning `None` is how
//! that case reaches the caller.
//!
//! The announcement's 60-second stale window is the client's timeout, not a
//! server-side deadline: ATAK gives up waiting after it and carries on in XML.

use std::time::Duration;

use crate::detail::TakControl;
use crate::event::{Event, Point};
use crate::time::CotTime;
use crate::types::{cot_type, how};

/// The only protocol version this crate speaks.
pub const PROTO_VERSION: u32 = 1;

/// The Marti API version advertised alongside the protocol offer.
pub const API_VERSION: u32 = 3;

/// How long a negotiation message stays valid; also the client's timeout.
pub const VALIDITY: Duration = Duration::from_secs(60);

/// The point every negotiation message carries.
///
/// Note the error estimates: negotiation uses `999999`, one digit shorter than
/// the `9999999` sentinel of [`Point::zero`] that pings, pongs and
/// notifications use. Both are "unknown" to a client; they differ because the
/// templates they come from differ, and matching them keeps a byte-comparison
/// against a real server honest.
pub const POINT: Point = Point {
    lat: 0.0,
    lon: 0.0,
    hae: 0.0,
    ce: 999_999.0,
    le: 999_999.0,
};

/// What a server offered in its `t-x-takp-v`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Announce {
    /// Protocol versions the server supports, in document order.
    pub versions: Vec<u32>,
    /// The server's version string. CloudTAK displays this.
    pub server_version: Option<String>,
    /// The server's Marti API version.
    pub api_version: Option<u32>,
}

impl Announce {
    /// Whether the offer includes this protocol version.
    #[must_use]
    pub fn supports(&self, version: u32) -> bool {
        self.versions.contains(&version)
    }
}

/// The shared envelope: `how="m-g"`, [`POINT`], stale in [`VALIDITY`].
fn envelope(r#type: &str, uid: impl Into<String>, now: CotTime) -> Event {
    Event::builder(r#type, uid)
        .how(how::M_G)
        .point_full(POINT)
        .time(now)
        .stale_after(VALIDITY)
        .build()
}

/// The server's protocol offer.
///
/// `uid` is a fresh identifier for this negotiation; the client echoes it back
/// and the server's [`response`] must reuse it, so keep it for the connection's
/// lifetime.
#[must_use]
pub fn announce(
    uid: impl Into<String>,
    server_version: impl Into<String>,
    api_version: u32,
    now: CotTime,
) -> Event {
    let mut event = envelope(cot_type::TAKP_V, uid, now);
    event.detail.set(&TakControl::announce(
        PROTO_VERSION,
        server_version,
        api_version,
    ));
    event
}

/// Reads a server's offer, if this event is one.
#[must_use]
pub fn parse_announce(event: &Event) -> Option<Announce> {
    if event.r#type != cot_type::TAKP_V {
        return None;
    }
    let control = event.detail.get::<TakControl>()?;
    Some(Announce {
        versions: control.supported,
        server_version: control.server_version,
        api_version: control.api_version,
    })
}

/// The client's request to switch, which must reuse the announcement's `uid`.
#[must_use]
pub fn request(uid: impl Into<String>, version: u32, now: CotTime) -> Event {
    let mut event = envelope(cot_type::TAKP_Q, uid, now);
    event.detail.set(&TakControl::request(version));
    event
}

/// The version a client is asking for, if this event is a readable request.
///
/// `None` means "do not answer": either the event is not a request at all, or
/// its `<TakRequest version>` is missing or unparseable, and TAK Server's
/// behaviour in that case — which ATAK is built around — is silence.
#[must_use]
pub fn parse_request(event: &Event) -> Option<u32> {
    if event.r#type != cot_type::TAKP_Q {
        return None;
    }
    event.detail.get::<TakControl>()?.request
}

/// The server's answer. `accepted` becomes the literal `true` or `false`.
#[must_use]
pub fn response(uid: impl Into<String>, accepted: bool, now: CotTime) -> Event {
    let mut event = envelope(cot_type::TAKP_R, uid, now);
    event.detail.set(&TakControl::response(accepted));
    event
}

/// Whether the server accepted, if this event is a response.
#[must_use]
pub fn parse_response(event: &Event) -> Option<bool> {
    if event.r#type != cot_type::TAKP_R {
        return None;
    }
    event.detail.get::<TakControl>()?.response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;

    const NOW: CotTime = CotTime::from_millis(1_789_646_400_000);

    fn rendered(event: &Event) -> String {
        String::from_utf8(xml::write(event).to_vec()).expect("the writer emits UTF-8")
    }

    /// The three templates are asserted byte for byte against the shapes
    /// `tests/golden/takp-{v,q,r}.xml` pin, so a change to the writer, the
    /// attribute order or the point sentinels cannot pass unnoticed.
    #[test]
    fn the_offer_matches_the_verified_server_template() {
        let event = announce(
            "rustak-test-server",
            "TAK Server rustak-0.1.0",
            API_VERSION,
            NOW,
        );

        assert_eq!(event.r#type, "t-x-takp-v");
        assert_eq!(event.how.as_deref(), Some("m-g"));
        assert_eq!(event.time, NOW);
        assert_eq!(event.start, NOW);
        assert_eq!(event.stale - event.time, 60_000);
        assert_eq!(event.point, POINT);
        assert_eq!(
            rendered(&event),
            format!(
                "{}\n{}",
                xml::DECLARATION,
                concat!(
                    r#"<event version="2.0" uid="rustak-test-server" type="t-x-takp-v" how="m-g" "#,
                    r#"time="2026-09-17T12:00:00.000Z" start="2026-09-17T12:00:00.000Z" stale="2026-09-17T12:01:00.000Z">"#,
                    r#"<point lat="0.0" lon="0.0" hae="0.0" ce="999999.0" le="999999.0"/>"#,
                    r#"<detail><TakControl><TakProtocolSupport version="1"/>"#,
                    r#"<TakServerVersionInfo serverVersion="TAK Server rustak-0.1.0" apiVersion="3"/>"#,
                    r#"</TakControl></detail></event>"#,
                )
            )
        );
    }

    #[test]
    fn the_request_matches_the_verified_client_template() {
        let event = request(
            "ANDROID-rustak-alpha",
            PROTO_VERSION,
            NOW + Duration::from_millis(500),
        );

        assert_eq!(event.r#type, "t-x-takp-q");
        assert_eq!(event.how.as_deref(), Some("m-g"));
        assert_eq!(event.point, POINT);
        assert_eq!(
            rendered(&event),
            format!(
                "{}\n{}",
                xml::DECLARATION,
                concat!(
                    r#"<event version="2.0" uid="ANDROID-rustak-alpha" type="t-x-takp-q" how="m-g" "#,
                    r#"time="2026-09-17T12:00:00.500Z" start="2026-09-17T12:00:00.500Z" stale="2026-09-17T12:01:00.500Z">"#,
                    r#"<point lat="0.0" lon="0.0" hae="0.0" ce="999999.0" le="999999.0"/>"#,
                    r#"<detail><TakControl><TakRequest version="1"/></TakControl></detail></event>"#,
                )
            )
        );
    }

    #[test]
    fn the_response_matches_the_verified_server_template() {
        let event = response("rustak-test-server", true, NOW + Duration::from_millis(600));

        assert_eq!(event.r#type, "t-x-takp-r");
        assert_eq!(
            event.uid, "rustak-test-server",
            "the response reuses the offer's uid"
        );
        assert_eq!(
            rendered(&event),
            format!(
                "{}\n{}",
                xml::DECLARATION,
                concat!(
                    r#"<event version="2.0" uid="rustak-test-server" type="t-x-takp-r" how="m-g" "#,
                    r#"time="2026-09-17T12:00:00.600Z" start="2026-09-17T12:00:00.600Z" stale="2026-09-17T12:01:00.600Z">"#,
                    r#"<point lat="0.0" lon="0.0" hae="0.0" ce="999999.0" le="999999.0"/>"#,
                    r#"<detail><TakControl><TakResponse status="true"/></TakControl></detail></event>"#,
                )
            )
        );
    }

    #[test]
    fn a_refusal_says_false_in_the_same_shape() {
        let refused = response("U", false, NOW);
        assert!(rendered(&refused).ends_with(
            r#"<detail><TakControl><TakResponse status="false"/></TakControl></detail></event>"#
        ));
        assert_eq!(parse_response(&refused), Some(false));
    }

    #[test]
    fn negotiation_uses_six_nines_where_notifications_use_seven() {
        assert_eq!(POINT.ce, 999_999.0);
        assert_eq!(POINT.le, 999_999.0);
        assert_eq!(Point::zero().ce, 9_999_999.0);
    }

    #[test]
    fn every_message_round_trips_through_its_own_parser() {
        let offer = announce("U", "rustak-0.1.0", API_VERSION, NOW);
        let read = parse_announce(&offer).expect("an offer");
        assert_eq!(read.versions, vec![PROTO_VERSION]);
        assert!(read.supports(PROTO_VERSION));
        assert!(!read.supports(2));
        assert_eq!(read.server_version.as_deref(), Some("rustak-0.1.0"));
        assert_eq!(read.api_version, Some(API_VERSION));

        assert_eq!(parse_request(&request("U", 1, NOW)), Some(1));
        assert_eq!(parse_response(&response("U", true, NOW)), Some(true));
        assert_eq!(parse_response(&response("U", false, NOW)), Some(false));
    }

    #[test]
    fn the_parsers_survive_a_trip_over_the_wire() {
        let offer = announce("U", "rustak-0.1.0", API_VERSION, NOW);
        let reparsed = xml::parse(&xml::write(&offer)).expect("our own XML");
        assert_eq!(parse_announce(&reparsed), parse_announce(&offer));
    }

    #[test]
    fn a_parser_ignores_an_event_of_another_type() {
        let offer = announce("U", "rustak-0.1.0", API_VERSION, NOW);
        assert_eq!(parse_request(&offer), None);
        assert_eq!(parse_response(&offer), None);
        assert_eq!(parse_announce(&request("U", 1, NOW)), None);
    }

    #[test]
    fn a_request_without_a_readable_version_means_stay_silent() {
        // No detail at all.
        let bare = envelope(cot_type::TAKP_Q, "U", NOW);
        assert_eq!(parse_request(&bare), None);

        // A `<TakControl>` whose request node is missing.
        let mut empty_control = envelope(cot_type::TAKP_Q, "U", NOW);
        empty_control.detail.set(&TakControl::default());
        assert_eq!(parse_request(&empty_control), None);

        // A version that is not a number.
        let unreadable = xml::parse_str(&format!(
            "{}\n{}",
            xml::DECLARATION,
            r#"<event version="2.0" uid="U" type="t-x-takp-q" how="m-g" time="2026-09-18T00:00:00.000Z" start="2026-09-18T00:00:00.000Z" stale="2026-09-18T00:01:00.000Z"><point lat="0" lon="0" hae="0" ce="999999" le="999999"/><detail><TakControl><TakRequest version="one"/></TakControl></detail></event>"#
        ))
        .expect("well formed XML");
        assert_eq!(parse_request(&unreadable), None);
    }

    #[test]
    fn an_unsupported_version_is_reported_rather_than_hidden() {
        // Reading the request is the caller's decision point: the parser
        // reports what was asked for, the connection decides what to answer.
        let asking_for_two = request("U", 2, NOW);
        assert_eq!(parse_request(&asking_for_two), Some(2));
        assert_ne!(parse_request(&asking_for_two), Some(PROTO_VERSION));
    }
}
