//! Turning `<marti><dest …/>` into a list of connections.
//!
//! Each `<dest>` is matched against **one** attribute, in the order
//! `compat/streaming.md` §8 fixes: callsign, publish, uid, mission,
//! mission-guid, group. First attribute wins; the rest of the element is
//! ignored. A message with no usable destination is an implicit broadcast to
//! everyone the sender can reach, minus the sender itself.
//!
//! # `All Streaming` is not a callsign
//!
//! ATAK's "post to all" writes a `<dest callsign="All Streaming"/>` into the
//! list. Treating it as a callsign would deliver to nobody, because nothing is
//! called that; the rule is that its presence **discards the whole callsign
//! list** and the message degrades to a broadcast. That is what makes "post to
//! all" work on a client that also had two people selected.
//!
//! # Explicit addressing is not a way around the channels
//!
//! Naming a callsign still asks the reachability question. A `<dest>` is a
//! *narrowing* of who receives a message, never a widening of who may be
//! reached — otherwise every membership in the system could be bypassed by
//! typing a callsign.

use std::sync::Arc;

use rustak_cot::codec::EncodedEvent;
use rustak_cot::detail::marti::{ALL_STREAMING, Dest, DestKind};

use crate::db::Database;
use crate::prelude::*;

use super::hub::Hub;
use super::mission_hook::{MissionIngest, MissionRef};
use super::subscription::{ConnHandle, ConnId};

/// Why a message reached nobody.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// It already carries this server's flow tag: it has been here before.
    DuplicateFlowTag,
    /// The sender is incognito and named nobody explicitly.
    Incognito,
    /// It would not parse, or carried no `<point>`.
    Unreadable,
    /// `<dest group>` named a channel the sender may not publish into.
    GroupNotMember(String),
    /// `<dest group>` named a channel that does not exist.
    NoSuchGroup(String),
    /// Nobody was allowed to receive it.
    NoRecipients,
}

impl DropReason {
    /// How the reason is written in a log line and a metric.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DuplicateFlowTag => "duplicate-flow-tag",
            Self::Incognito => "incognito",
            Self::Unreadable => "unreadable",
            Self::GroupNotMember(_) => "group-not-member",
            Self::NoSuchGroup(_) => "no-such-group",
            Self::NoRecipients => "no-recipients",
        }
    }
}

/// Who a message goes to, and whether it was addressed or broadcast.
#[derive(Debug, Default)]
pub struct Selection {
    /// The connections to deliver to.
    pub handles: Vec<ConnHandle>,
    /// Whether the sender named its recipients.
    pub explicit: bool,
    /// Whether the sender named *people* — `<dest callsign>` or `<dest uid>` —
    /// and nothing that keeps a message for later.
    ///
    /// This is the case an undeliverable GeoChat bounces on, and the reason it
    /// is narrower than [`explicit`](Self::explicit). A broadcast reaching
    /// nobody means nobody is connected; a channel or mission reaching nobody
    /// means nobody is *reading* it, which for a mission is ordinary because
    /// the write was still stored. Only somebody typing at a named person has
    /// been told something that did not happen.
    pub direct: bool,
}

/// The destination kinds a message carries, already partitioned.
#[derive(Debug, Default)]
struct Addresses<'a> {
    callsigns: Vec<String>,
    uids: Vec<String>,
    groups: Vec<&'a str>,
    missions: Vec<MissionRef<'a>>,
    /// A `<dest publish>` was present, which addresses nobody but is still an
    /// address — a message carrying only one must not fall back to a broadcast.
    publish: bool,
    /// A `<dest callsign="All Streaming">` was present, so the callsign list is
    /// discarded.
    all_streaming: bool,
}

/// Works out who should receive a message.
///
/// # Errors
///
/// A [`DropReason`] when the message reaches nobody for a reason worth counting
/// separately; an empty [`Selection`] is not an error, because a broadcast on a
/// server with one client connected is ordinary.
pub async fn select_recipients(
    hub: &Hub,
    db: &Database,
    missions: &dyn MissionIngest,
    from: ConnId,
    sender: &Principal,
    dests: &[Dest],
    encoded: &Arc<EncodedEvent>,
) -> Result<Selection, DropReason> {
    let addresses = partition(dests);

    if !addresses.has_any() {
        return Ok(Selection {
            handles: hub.reachable_from(from, true),
            explicit: false,
            direct: false,
        });
    }

    let mut handles: Vec<ConnHandle> = Vec::new();

    if !addresses.callsigns.is_empty() {
        extend(
            &mut handles,
            hub.resolve_callsigns(from, &addresses.callsigns),
        );
    }

    if !addresses.uids.is_empty() {
        extend(&mut handles, hub.resolve_uids(from, &addresses.uids));
    }

    for group in &addresses.groups {
        extend(
            &mut handles,
            group_recipients(hub, db, from, sender, group).await?,
        );
    }

    for mission in &addresses.missions {
        extend(
            &mut handles,
            mission_recipients(hub, missions, from, sender, *mission, encoded).await,
        );
    }

    Ok(Selection {
        handles,
        explicit: true,
        direct: addresses.names_people(),
    })
}

impl Addresses<'_> {
    /// Whether anything at all narrowed the delivery.
    ///
    /// `all_streaming` is deliberately *not* counted: its whole purpose is to
    /// undo the callsign list it appears in, and a message left with nothing
    /// else falls back to the broadcast the operator asked for.
    fn has_any(&self) -> bool {
        !self.callsigns.is_empty()
            || !self.uids.is_empty()
            || !self.groups.is_empty()
            || !self.missions.is_empty()
            || self.publish
    }

    /// Whether the sender addressed individual people and nothing else.
    ///
    /// A mission on the list disqualifies the whole message: a Data Sync keeps
    /// what it is sent whether or not anybody is connected to read it, so a
    /// chat that also went to one is not undelivered even when no subscriber
    /// was reachable.
    fn names_people(&self) -> bool {
        self.missions.is_empty() && (!self.callsigns.is_empty() || !self.uids.is_empty())
    }
}

/// Sorts the destinations by what each one addresses.
fn partition(dests: &[Dest]) -> Addresses<'_> {
    let mut addresses = Addresses::default();

    for dest in dests {
        match dest.kind() {
            Some(DestKind::Callsign(callsign)) if callsign == ALL_STREAMING => {
                addresses.all_streaming = true;
            }
            Some(DestKind::Callsign(callsign)) => addresses.callsigns.push(callsign.to_owned()),
            Some(DestKind::Uid(uid)) => addresses.uids.push(uid.to_owned()),
            Some(DestKind::Group(group)) => addresses.groups.push(group),
            Some(DestKind::Mission { name, path, after }) => addresses.missions.push(MissionRef {
                name: Some(name),
                guid: None,
                path,
                after,
            }),
            Some(DestKind::MissionGuid { guid, path, after }) => {
                addresses.missions.push(MissionRef {
                    name: None,
                    guid: Some(guid),
                    path,
                    after,
                });
            }
            Some(DestKind::Publish(topic)) => {
                debug!(
                    topic,
                    "Ignored a <dest publish>; rustak has no publish topics."
                );
                addresses.publish = true;
            }
            None => {}
        }
    }

    if addresses.all_streaming {
        // "Post to all" wins over whoever else was selected, which is how the
        // option degrades on a client that had both.
        addresses.callsigns.clear();
    }

    addresses
}

/// The readers of one channel, when the sender may publish into it.
async fn group_recipients(
    hub: &Hub,
    db: &Database,
    from: ConnId,
    sender: &Principal,
    group: &str,
) -> Result<Vec<ConnHandle>, DropReason> {
    let Ok(name) = GroupName::parse(group) else {
        return Err(DropReason::NoSuchGroup(group.to_owned()));
    };

    let row = db
        .groups()
        .get_by_name(&name)
        .await
        .map_err(|err| {
            warn!(group, error = %err, "Could not resolve a <dest group>.");
            DropReason::NoSuchGroup(group.to_owned())
        })?
        .ok_or_else(|| DropReason::NoSuchGroup(group.to_owned()))?;

    if !sender.has_group(row.bitpos, Direction::In) {
        return Err(DropReason::GroupNotMember(group.to_owned()));
    }

    Ok(hub.reachable_in_group(from, row.bitpos))
}

/// The subscribers a mission write should also be pushed to.
///
/// A failure is logged and treated as "nobody", not as a reason to drop the
/// connection: a mission the sender may not write to is a routing decision.
///
/// # A subscription is not a way around the channels
///
/// Resolved through [`Hub::resolve_uids`], which applies `sender.IN ∩
/// receiver.OUT` per pair exactly as `<dest uid>` and `<dest callsign>` do.
/// This used to be a bare `handles_for_uid` index lookup, so a subscriber with
/// no channel overlap with the sender received the raw position and chat
/// traffic anyway — `compat/missions.md` §11 item 2 says the relay is "still
/// subject to the normal `IN`/`OUT` reachability check", and R-02 H3 found it
/// was not.
///
/// The `t-x-m-c` **notification** is a different delivery and is contractually
/// allowed to bypass the broker (§12); it goes out through `stream::notify`, not
/// through here, and is unaffected.
async fn mission_recipients(
    hub: &Hub,
    missions: &dyn MissionIngest,
    from: ConnId,
    sender: &Principal,
    dest: MissionRef<'_>,
    encoded: &Arc<EncodedEvent>,
) -> Vec<ConnHandle> {
    let uids = match missions.publish(dest, sender, encoded).await {
        Ok(uids) => uids,
        Err(err) => {
            debug!(mission = dest.label(), error = %err, "A mission destination reached nobody.");
            return Vec::new();
        }
    };

    hub.resolve_uids(from, &uids)
}

/// Appends handles that are not already in the list.
///
/// A client that is both named by callsign and a member of an addressed channel
/// receives one copy, not two.
fn extend(into: &mut Vec<ConnHandle>, more: Vec<ConnHandle>) {
    for handle in more {
        if !into.iter().any(|held| held.id() == handle.id()) {
            into.push(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_with_no_usable_destination_is_a_broadcast() {
        let dests = [Dest::default()];
        let addresses = partition(&dests);

        assert!(
            !addresses.has_any(),
            "a <dest> with no routing attribute names nobody"
        );
    }

    #[test]
    fn all_streaming_discards_the_list_it_appears_in() {
        // ATAK's "post to all" on a client that also had somebody selected: the
        // option has to win, or "post to all" would quietly post to one person.
        let dests = [
            Dest::callsign("BRAVO"),
            Dest::callsign(ALL_STREAMING),
            Dest::callsign("ECHO"),
        ];
        let addresses = partition(&dests);

        assert!(addresses.callsigns.is_empty());
        assert!(
            !addresses.has_any(),
            "so the message falls back to the broadcast it asked for",
        );
    }

    #[test]
    fn a_publish_topic_is_an_address_even_though_it_reaches_nobody() {
        // Not a broadcast: a message carrying only a publish topic must reach
        // nobody rather than everybody.
        let dests = [Dest {
            publish: Some("topic".into()),
            ..Dest::default()
        }];
        let addresses = partition(&dests);

        assert!(addresses.publish);
        assert!(addresses.has_any());
    }

    #[test]
    fn naming_a_person_is_what_an_undeliverable_chat_bounces_on() {
        assert!(partition(&[Dest::callsign("BRAVO")]).names_people());
        assert!(partition(&[Dest::uid("UID-B")]).names_people());

        // A channel or a mission reaching nobody is ordinary: the first means
        // nobody is listening, the second means nobody is subscribed, and the
        // write was kept either way.
        assert!(!partition(&[Dest::group("blue")]).names_people());
        assert!(!partition(&[Dest::mission("Kettle")]).names_people());
        assert!(!partition(&[Dest::callsign("BRAVO"), Dest::mission("Kettle")]).names_people());

        // And neither "post to all" nor a publish topic is a person.
        assert!(!partition(&[Dest::callsign(ALL_STREAMING)]).names_people());
        assert!(
            !partition(&[Dest {
                publish: Some("topic".into()),
                ..Dest::default()
            }])
            .names_people()
        );
    }

    #[test]
    fn first_attribute_wins_in_the_order_the_contract_fixes() {
        // A `<dest>` carrying both is matched on the callsign, because that is
        // the first attribute in the order — not on whichever the parser
        // happened to read last.
        let dests = [Dest {
            callsign: Some("BRAVO".into()),
            uid: Some("UID-B".into()),
            ..Dest::default()
        }];
        let addresses = partition(&dests);

        assert_eq!(addresses.callsigns, vec!["BRAVO".to_string()]);
        assert!(addresses.uids.is_empty());
    }

    #[test]
    fn a_mission_is_read_by_either_identifier() {
        let dests = [Dest::mission("Kettle"), Dest::mission_guid("4d0f")];
        let addresses = partition(&dests);

        assert_eq!(addresses.missions.len(), 2);
        assert_eq!(addresses.missions[0].name, Some("Kettle"));
        assert_eq!(addresses.missions[1].guid, Some("4d0f"));
    }

    #[test]
    fn every_drop_reason_is_named_for_a_metric() {
        for reason in [
            DropReason::DuplicateFlowTag,
            DropReason::Incognito,
            DropReason::Unreadable,
            DropReason::GroupNotMember("blue".into()),
            DropReason::NoSuchGroup("blue".into()),
            DropReason::NoRecipients,
        ] {
            assert!(!reason.as_str().is_empty());
        }
    }
}
