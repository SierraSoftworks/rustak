//! The small control and notification messages a TAK stream carries.
//!
//! None of these describe anything on the map: they are keepalives and
//! housekeeping notices, and every one of them is a template a real client
//! already matches on. Three details cost interoperability if they drift:
//!
//! * a control message with no `<point lat=…>` is **dropped before it reaches
//!   any handler**, so every message here carries [`Point::zero`] even though
//!   none of them has a position;
//! * the pong's uid is the literal string [`PONG_UID`] — not the ping's uid,
//!   not a derivative of it. ATAK discards pongs and CloudTAK matches only on
//!   the type, so nothing breaks loudly if this is wrong, which is exactly why
//!   it is pinned here; and
//! * the pong goes **only to the connection that pinged**: it bypasses the
//!   broker, so it carries no flow tag and gets no group check.
//!
//! Event uids that a server generates fresh (the disconnect and group-change
//! notices) are parameters rather than something this crate invents:
//! `rustak-cot` has no random source and stays deterministic.
//!
//! [`chat_delivery_failure`] is the odd one out: it is not a template at all
//! but the sender's own undeliverable GeoChat handed back with its type
//! changed, because that is the only form a client can match to the
//! conversation the message came from.

use std::time::Duration;

use crate::detail::link::{Link, RELATION_P_P};
use crate::detail::{Element, TypedDetail, flow_tags, marti};
use crate::event::{Event, Point};
use crate::time::CotTime;
use crate::types::{cot_type, how, is_chat};

/// The uid a server's pong always carries.
pub const PONG_UID: &str = "takPong";

/// What a client appends to its device uid to build a ping uid.
pub const PING_SUFFIX: &str = "-ping";

/// How long a ping stays valid.
pub const PING_VALIDITY: Duration = Duration::from_secs(10);

/// How long a pong or a server notice stays valid.
pub const NOTICE_VALIDITY: Duration = Duration::from_secs(20);

/// The shared envelope: a point at null island, and nothing else.
fn envelope(r#type: &str, uid: impl Into<String>, how: &str, now: CotTime) -> Event {
    Event::builder(r#type, uid)
        .how(how)
        .point_full(Point::zero())
        .time(now)
        .stale_after(NOTICE_VALIDITY)
        .build()
}

/// A client's keepalive.
///
/// ATAK sends one after 15 s of silence, repeats every 4.5 s, and gives up at
/// 25 s; CloudTAK simply pings every 5 s. `device_uid` is the client's own uid,
/// to which [`PING_SUFFIX`] is appended.
#[must_use]
pub fn ping(device_uid: &str, now: CotTime) -> Event {
    let mut event = envelope(
        cot_type::PING,
        format!("{device_uid}{PING_SUFFIX}"),
        how::M_G,
        now,
    );
    event.stale = event.start.stale_after(PING_VALIDITY);
    event
}

/// The server's reply to a ping, addressed to the pinging connection alone.
#[must_use]
pub fn pong(now: CotTime) -> Event {
    envelope(cot_type::PONG, PONG_UID, how::H_G_I_G_O, now)
}

/// Whether this event is a keepalive we owe a [`pong`].
///
/// The match is case-sensitive on purpose: TAK Server classifies `T-X-C-T` as
/// a control message (so it is consumed, never relayed) but then falls through
/// its dispatch without answering, and clients are built around that.
#[must_use]
pub fn is_ping(event: &Event) -> bool {
    event.r#type == cot_type::PING
}

/// Whether this event is a keepalive reply.
///
/// CloudTAK marks a connection open on the first one it sees, so a client
/// waiting to be told it is connected watches for this.
#[must_use]
pub fn is_pong(event: &Event) -> bool {
    event.r#type == cot_type::PONG
}

/// The notice that a peer went away, sent to everyone reachable from it.
///
/// Only worth sending for a subscription that had both a callsign and a client
/// uid — one that never sent an SA message was never visible to anyone.
/// `uid` is a fresh identifier for the notice itself; `client_uid` and
/// `last_sa_type` identify the peer that left.
#[must_use]
pub fn disconnect(
    uid: impl Into<String>,
    client_uid: &str,
    last_sa_type: &str,
    now: CotTime,
) -> Event {
    let mut event = envelope(cot_type::DISCONNECT, uid, how::H_G_I_G_O, now);
    // Built attribute by attribute rather than through `Link::peer` so the
    // order matches the verified template: relation, uid, type.
    event.detail.push(
        Element::new(Link::NAME)
            .attr("relation", RELATION_P_P)
            .attr("uid", client_uid)
            .attr("type", last_sa_type),
    );
    event
}

/// The notice that the recipient's channel membership changed.
///
/// Sent to the user's **other** devices, never back to the one whose change
/// caused it. Both ATAK and CloudTAK react by clearing this server's map items
/// and re-fetching `/Marti/api/groups/all?sendLatestSA=true`, so the notice
/// carries no detail beyond a bare peer link.
#[must_use]
pub fn group_change(uid: impl Into<String>, now: CotTime) -> Event {
    let mut event = envelope(cot_type::GROUP_CHANGE, uid, how::H_G_I_G_O, now);
    event.detail.push(Link::bare_peer().to_element());
    event
}

/// Builds the uid of a group-change notice.
///
/// TAK Server appends the originating device's uid after a dot when the change
/// names one, which is how a client tells "my own membership changed" from "a
/// device of mine changed it".
#[must_use]
pub fn group_change_uid(fresh: &str, client_uid: Option<&str>) -> String {
    match client_uid {
        Some(client_uid) => format!("{fresh}.{client_uid}"),
        None => fresh.to_owned(),
    }
}

/// Whether this event asks to enter (`true`) or leave (`false`) incognito.
///
/// An incognito subscription's own messages are dropped at ingest unless they
/// carry at least one `<marti><dest callsign=…/></marti>`, and it is skipped by
/// latest-SA replay: the client can address people explicitly but is otherwise
/// invisible.
#[must_use]
pub fn incognito_toggle(event: &Event) -> Option<bool> {
    match event.r#type.as_str() {
        cot_type::INCOGNITO_ON => Some(true),
        cot_type::INCOGNITO_OFF => Some(false),
        _ => None,
    }
}

/// Whether an undeliverable copy of this message owes its sender a bounce.
///
/// The whole GeoChat family bounces — the message and the delivery, read and
/// pending receipts — with one exception: a [`chat_delivery_failure`] must
/// never itself bounce, or a sender that left between the chat and the bounce
/// would set two servers, or a server and its own store, talking in circles.
/// That is the `b-t-f` prefix minus `b-t-f-s`, which is exactly the test TAK
/// Server applies.
#[must_use]
pub fn bounces_when_undeliverable(event: &Event) -> bool {
    is_chat(&event.r#type) && event.r#type != cot_type::CHAT_FAILED
}

/// The sender's own chat handed straight back to it, marked undelivered.
///
/// Not a fresh notice: the bounce **is** the original message with its type
/// changed to [`cot_type::CHAT_FAILED`], and everything a client needs to know
/// which line to mark is carried by that copy. ATAK matches the bounce to a
/// conversation the same way it matches any chat — `__chat@id`, falling back to
/// the dot-separated event uid — so re-deriving a template here instead of
/// echoing would land the notice in the wrong conversation, or in none.
///
/// Two things come off the copy, both because the sender is about to receive a
/// message it originally sent:
///
/// * `<marti>`, because an address list never survives a relay and this one
///   named the very people who could not be reached; and
/// * this server's flow tag, so that the bounce is not read as a message that
///   has already been here — `server_id` is the same identifier
///   [`flow_tags::add_flow_tag`] stamps with, and other servers' tags stay put.
///
/// The bounce is delivered straight to the sender's connection: it is not
/// brokered, so it is never given a flow tag of its own and asks no channel
/// question.
#[must_use]
pub fn chat_delivery_failure(undelivered: &Event, server_id: &str) -> Event {
    let mut bounce = undelivered.clone();

    bounce.r#type = cot_type::CHAT_FAILED.to_owned();
    let _ = marti::take_marti(&mut bounce.detail);
    flow_tags::remove_flow_tag(&mut bounce.detail, server_id);

    bounce
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::link::links;
    use crate::detail::{Chat, Node};
    use crate::xml;

    /// `2026-09-17T12:00:40.000Z`, the instant the golden fixtures use.
    const NOW: CotTime = CotTime::from_millis(1_789_646_440_000);

    /// The flow-tag key the bounce tests hand to [`chat_delivery_failure`].
    const SERVER_ID: &str = "rustak-test";

    fn rendered(event: &Event) -> String {
        String::from_utf8(xml::write(event).to_vec()).expect("the writer emits UTF-8")
    }

    /// A direct GeoChat as it looks at the point the router gives up on it:
    /// ATAK's own shape, already carrying this server's flow tag, still holding
    /// the `<marti>` that named the peer nobody could reach.
    fn undelivered_chat() -> Event {
        let mut event = Event::builder(
            cot_type::CHAT,
            "GeoChat.ANDROID-rustak-alpha.ANDROID-rustak-bravo.MSG-0002",
        )
        .how(how::H_G_I_G_O)
        .point(51.5074, -0.1278)
        .time(NOW)
        .stale_after(Duration::from_secs(86_400))
        .push(
            Element::new("__chat")
                .attr("id", "ANDROID-rustak-bravo")
                .attr("chatroom", "BRAVO")
                .attr("senderCallsign", "ALPHA")
                .attr("groupOwner", "false")
                .attr("messageId", "MSG-0002")
                .with(
                    Element::new("chatgrp")
                        .attr("id", "ANDROID-rustak-bravo")
                        .attr("uid0", "ANDROID-rustak-alpha")
                        .attr("uid1", "ANDROID-rustak-bravo"),
                ),
        )
        .push(Link::peer("ANDROID-rustak-alpha", "a-f-G-U-C").to_element())
        .push(marti::marti_element(&[marti::Dest::callsign("BRAVO")]))
        .push(
            Element::new("remarks")
                .attr("source", "BAO.F.ATAK.ANDROID-rustak-alpha")
                .attr("to", "ANDROID-rustak-bravo")
                .attr("time", "2026-09-17T12:00:40.000Z")
                .with(Node::Text("moving to the RV".to_owned())),
        )
        .build();

        flow_tags::add_flow_tag(&mut event.detail, SERVER_ID, NOW);

        event
    }

    #[test]
    fn the_ping_matches_the_verified_client_template() {
        let event = ping("ANDROID-rustak-alpha", NOW);

        assert_eq!(event.uid, "ANDROID-rustak-alpha-ping");
        assert_eq!(event.r#type, "t-x-c-t");
        assert_eq!(event.how.as_deref(), Some("m-g"));
        assert_eq!(event.stale - event.time, 10_000);
        assert!(event.detail.is_empty(), "a ping carries no detail");
        assert_eq!(
            rendered(&event),
            format!(
                "{}\n{}",
                xml::DECLARATION,
                concat!(
                    r#"<event version="2.0" uid="ANDROID-rustak-alpha-ping" type="t-x-c-t" how="m-g" "#,
                    r#"time="2026-09-17T12:00:40.000Z" start="2026-09-17T12:00:40.000Z" stale="2026-09-17T12:00:50.000Z">"#,
                    r#"<point lat="0.0" lon="0.0" hae="0.0" ce="9999999.0" le="9999999.0"/></event>"#,
                )
            )
        );
    }

    #[test]
    fn the_pong_matches_the_verified_server_template() {
        let event = pong(NOW + Duration::from_millis(100));

        assert_eq!(event.uid, PONG_UID, "the uid is a constant, not derived");
        assert_eq!(event.r#type, "t-x-c-t-r");
        assert_eq!(event.how.as_deref(), Some("h-g-i-g-o"));
        assert_eq!(event.stale - event.time, 20_000);
        assert!(event.detail.is_empty(), "no <detail> element at all");
        assert_eq!(
            rendered(&event),
            format!(
                "{}\n{}",
                xml::DECLARATION,
                concat!(
                    r#"<event version="2.0" uid="takPong" type="t-x-c-t-r" how="h-g-i-g-o" "#,
                    r#"time="2026-09-17T12:00:40.100Z" start="2026-09-17T12:00:40.100Z" stale="2026-09-17T12:01:00.100Z">"#,
                    r#"<point lat="0.0" lon="0.0" hae="0.0" ce="9999999.0" le="9999999.0"/></event>"#,
                )
            )
        );
    }

    #[test]
    fn the_pong_uid_is_not_derived_from_the_ping() {
        let ping = ping("ANDROID-rustak-alpha", NOW);
        let pong = pong(NOW);
        assert!(!pong.uid.contains(&ping.uid));
        assert_eq!(pong.uid, "takPong");
    }

    #[test]
    fn the_disconnect_notice_matches_the_verified_server_template() {
        let event = disconnect(
            "rustak-test-server-dd-0001",
            "ANDROID-rustak-bravo",
            "a-f-G-U-C",
            CotTime::from_millis(1_789_646_700_000),
        );

        assert_eq!(event.r#type, "t-x-d-d");
        assert_eq!(event.how.as_deref(), Some("h-g-i-g-o"));
        assert_eq!(event.stale - event.time, 20_000);
        assert_eq!(
            rendered(&event),
            format!(
                "{}\n{}",
                xml::DECLARATION,
                concat!(
                    r#"<event version="2.0" uid="rustak-test-server-dd-0001" type="t-x-d-d" how="h-g-i-g-o" "#,
                    r#"time="2026-09-17T12:05:00.000Z" start="2026-09-17T12:05:00.000Z" stale="2026-09-17T12:05:20.000Z">"#,
                    r#"<point lat="0.0" lon="0.0" hae="0.0" ce="9999999.0" le="9999999.0"/>"#,
                    r#"<detail><link relation="p-p" uid="ANDROID-rustak-bravo" type="a-f-G-U-C"/></detail></event>"#,
                )
            )
        );
    }

    #[test]
    fn the_disconnect_link_reads_back_as_a_peer_link() {
        let event = disconnect("N", "ANDROID-rustak-bravo", "a-f-G-U-C", NOW);
        assert_eq!(
            links(&event.detail),
            vec![Link::peer("ANDROID-rustak-bravo", "a-f-G-U-C")]
        );
    }

    #[test]
    fn the_group_change_notice_carries_a_bare_peer_link() {
        let event = group_change("NOTICE-UID", NOW);

        assert_eq!(event.r#type, "t-x-g-c");
        assert_eq!(event.how.as_deref(), Some("h-g-i-g-o"));
        assert_eq!(event.stale - event.time, 20_000);
        assert_eq!(event.point, Point::zero());
        assert!(rendered(&event).ends_with(r#"<detail><link relation="p-p"/></detail></event>"#));
        assert_eq!(links(&event.detail), vec![Link::bare_peer()]);
    }

    #[test]
    fn a_group_change_uid_names_the_device_that_caused_it() {
        assert_eq!(group_change_uid("FRESH", None), "FRESH");
        assert_eq!(
            group_change_uid("FRESH", Some("ANDROID-rustak-alpha")),
            "FRESH.ANDROID-rustak-alpha"
        );
    }

    #[test]
    fn keepalives_are_recognised_by_type_alone() {
        assert!(is_ping(&ping("UID", NOW)));
        assert!(!is_pong(&ping("UID", NOW)));
        assert!(is_pong(&pong(NOW)));
        assert!(!is_ping(&pong(NOW)));
        assert!(!is_ping(&group_change("N", NOW)));
    }

    #[test]
    fn a_case_variant_keepalive_is_not_answered() {
        let mut shouted = ping("UID", NOW);
        shouted.r#type = "T-X-C-T".to_owned();
        assert!(
            !is_ping(&shouted),
            "TAK Server consumes it but never replies"
        );
    }

    #[test]
    fn incognito_toggles_read_both_ways_and_ignore_everything_else() {
        let mut event = ping("UID", NOW);

        event.r#type = "t-x-c-i-e".to_owned();
        assert_eq!(incognito_toggle(&event), Some(true));

        event.r#type = "t-x-c-i-d".to_owned();
        assert_eq!(incognito_toggle(&event), Some(false));

        event.r#type = "t-x-c-t".to_owned();
        assert_eq!(incognito_toggle(&event), None);
    }

    #[test]
    fn every_message_carries_a_point_so_it_survives_ingest() {
        // A control message without `<point lat>` is dropped before it reaches
        // a handler, so this is load bearing rather than cosmetic.
        for event in [
            ping("UID", NOW),
            pong(NOW),
            disconnect("N", "UID", "a-f-G-U-C", NOW),
            group_change("N", NOW),
        ] {
            let text = rendered(&event);
            assert!(text.contains(r#"<point lat="0.0""#), "{text}");
            assert_eq!(event.point, Point::zero());
        }
    }

    #[test]
    fn the_bounce_is_the_senders_own_chat_with_its_type_changed() {
        // Golden: a client reads this off the wire and has to find the line it
        // just sent, so every byte except the type is the sender's own.
        let bounce = chat_delivery_failure(&undelivered_chat(), SERVER_ID);

        assert_eq!(bounce.r#type, cot_type::CHAT_FAILED);
        assert_eq!(
            rendered(&bounce),
            format!(
                "{}\n{}",
                xml::DECLARATION,
                concat!(
                    r#"<event version="2.0" uid="GeoChat.ANDROID-rustak-alpha.ANDROID-rustak-bravo.MSG-0002" "#,
                    r#"type="b-t-f-s" how="h-g-i-g-o" time="2026-09-17T12:00:40.000Z" "#,
                    r#"start="2026-09-17T12:00:40.000Z" stale="2026-09-18T12:00:40.000Z">"#,
                    r#"<point lat="51.5074" lon="-0.1278" hae="9999999.0" ce="9999999.0" le="9999999.0"/>"#,
                    r#"<detail>"#,
                    r#"<__chat id="ANDROID-rustak-bravo" chatroom="BRAVO" senderCallsign="ALPHA" groupOwner="false" messageId="MSG-0002">"#,
                    r#"<chatgrp id="ANDROID-rustak-bravo" uid0="ANDROID-rustak-alpha" uid1="ANDROID-rustak-bravo"/>"#,
                    r#"</__chat>"#,
                    r#"<link uid="ANDROID-rustak-alpha" type="a-f-G-U-C" relation="p-p"/>"#,
                    r#"<remarks source="BAO.F.ATAK.ANDROID-rustak-alpha" to="ANDROID-rustak-bravo" time="2026-09-17T12:00:40.000Z">moving to the RV</remarks>"#,
                    r#"<_flow-tags_/>"#,
                    r#"</detail></event>"#,
                )
            )
        );
    }

    #[test]
    fn the_bounce_carries_the_conversation_the_client_matches_on() {
        // `__chat@id` is how ATAK decides which conversation a `b-t-f…` belongs
        // to; a bounce that lost it would be filed under nothing.
        let chat = undelivered_chat();
        let bounce = chat_delivery_failure(&chat, SERVER_ID);

        let original: Chat = chat.detail.get().expect("the chat we built");
        let echoed: Chat = bounce.detail.get().expect("the chat the bounce echoes");

        assert_eq!(echoed, original);
        assert_eq!(echoed.id.as_deref(), Some("ANDROID-rustak-bravo"));
        assert_eq!(
            echoed.participants(),
            vec!["ANDROID-rustak-alpha", "ANDROID-rustak-bravo"]
        );
        assert_eq!(bounce.uid, chat.uid, "the uid is the sender's own");
        assert_eq!(links(&bounce.detail), links(&chat.detail));
    }

    #[test]
    fn the_bounce_drops_the_address_list_and_only_our_own_flow_tag() {
        let mut chat = undelivered_chat();
        chat.detail.push(
            Element::new(flow_tags::ELEMENT)
                .attr(flow_tags::flow_tag_name("somebody-else"), NOW.to_string()),
        );

        let bounce = chat_delivery_failure(&chat, SERVER_ID);

        assert!(
            bounce.detail.find("marti").is_none(),
            "the list named the people who could not be reached"
        );
        assert!(!flow_tags::has_flow_tag(&bounce.detail, SERVER_ID));
        assert!(
            flow_tags::has_flow_tag(&bounce.detail, "somebody-else"),
            "another server's tag is not ours to remove"
        );
    }

    #[test]
    fn a_bounce_is_the_one_chat_type_that_never_bounces() {
        // Without the exception a sender that disconnected between its chat and
        // the bounce would have the bounce bounce, and so on.
        for bounces in [
            cot_type::CHAT,
            cot_type::CHAT_DELIVERED,
            cot_type::CHAT_READ,
            cot_type::CHAT_PENDING,
        ] {
            let mut event = undelivered_chat();
            event.r#type = bounces.to_owned();
            assert!(bounces_when_undeliverable(&event), "{bounces}");
        }

        for silent in [cot_type::CHAT_FAILED, "b-f-t-r", "a-f-G-U-C", "t-x-c-t"] {
            let mut event = undelivered_chat();
            event.r#type = silent.to_owned();
            assert!(!bounces_when_undeliverable(&event), "{silent}");
        }
    }

    #[test]
    fn every_message_survives_a_round_trip_over_the_wire() {
        for event in [
            ping("UID", NOW),
            pong(NOW),
            disconnect("N", "UID", "a-f-G-U-C", NOW),
            group_change("N", NOW),
        ] {
            assert_eq!(xml::parse(&xml::write(&event)).expect("our own XML"), event);
        }
    }
}
