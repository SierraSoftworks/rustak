//! Where `<marti><dest mission=…>` goes, once there are missions to put it in.
//!
//! A `<dest>` naming a mission is not a delivery, it is a *write*: the message
//! is appended to a Data Sync feed and the people subscribed to that feed are
//! told about it. None of that exists in M1, so the seam is a trait with a stub
//! implementation, and the router calls it exactly as it will when M4 fills it
//! in — including handling the uids it hands back.
//!
//! # Why a trait rather than a `TODO`
//!
//! The alternative is an `if` in the router that M4 deletes, which means the
//! routing path M1 tests is not the routing path that ships. With the trait,
//! the only thing that changes is which implementation is installed.

use std::sync::Arc;

use async_trait::async_trait;
use rustak_cot::codec::EncodedEvent;

use crate::prelude::*;

/// A mission a `<dest>` names, however it names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissionRef<'a> {
    /// The mission's name, when addressed by name.
    pub name: Option<&'a str>,
    /// The mission's GUID, when addressed by GUID.
    pub guid: Option<&'a str>,
    /// The layer path the message should be filed under.
    pub path: Option<&'a str>,
    /// The sibling to insert after, meaningful only with a path.
    pub after: Option<&'a str>,
}

impl MissionRef<'_> {
    /// How the mission is written in a log line.
    pub fn label(&self) -> &str {
        self.name.or(self.guid).unwrap_or("(unnamed)")
    }
}

/// Publishing into a Data Sync feed.
#[async_trait]
pub trait MissionIngest: Send + Sync + std::fmt::Debug {
    /// Files `event` into the mission `dest` names.
    ///
    /// Returns the `clientUid`s of the subscribers that should also be sent the
    /// message directly, which is how a mission write reaches the people
    /// watching it without the router knowing anything about missions.
    ///
    /// # Errors
    ///
    /// Whatever the mission store reports. A failure here drops the message
    /// rather than the connection: a mission the sender may not write to is a
    /// routing decision, not a protocol violation.
    async fn publish(
        &self,
        dest: MissionRef<'_>,
        sender: &Principal,
        event: &EncodedEvent,
    ) -> Result<Vec<String>, Error>;
}

/// The M1 stub: mission destinations are accepted and reach nobody.
///
/// Accepted rather than refused because a client addressing a mission this
/// server does not have yet must not have its connection disturbed — TAK
/// Server's own behaviour for an unknown mission is to route it nowhere.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoMissions;

#[async_trait]
impl MissionIngest for NoMissions {
    async fn publish(
        &self,
        dest: MissionRef<'_>,
        sender: &Principal,
        _event: &EncodedEvent,
    ) -> Result<Vec<String>, Error> {
        debug!(
            mission = dest.label(),
            sender = %sender.username,
            "Dropped a message addressed to a mission; Data Sync is not implemented yet."
        );

        Ok(Vec::new())
    }
}

/// The stub, behind the `Arc` the router holds.
pub fn no_missions() -> Arc<dyn MissionIngest> {
    Arc::new(NoMissions)
}

#[cfg(test)]
mod tests {
    use rustak_cot::Event;

    use super::*;

    fn principal() -> Principal {
        Principal::new(
            UserId::from(1),
            Username::parse("alice").unwrap(),
            PrincipalKind::Person,
            AuthMethod::SetupToken,
        )
    }

    #[tokio::test]
    async fn the_stub_accepts_a_mission_destination_and_reaches_nobody() {
        let event = EncodedEvent::new(Event::builder("b-t-f", "UID-1").point(0.0, 0.0).build());
        let dest = MissionRef {
            name: Some("Operation Kettle"),
            guid: None,
            path: None,
            after: None,
        };

        let recipients = no_missions()
            .publish(dest, &principal(), &event)
            .await
            .expect("the stub never refuses");

        assert!(recipients.is_empty());
    }

    #[test]
    fn a_mission_is_labelled_by_whichever_identifier_it_carries() {
        assert_eq!(
            MissionRef {
                name: None,
                guid: Some("4d0f…"),
                path: None,
                after: None,
            }
            .label(),
            "4d0f…"
        );
        assert_eq!(
            MissionRef {
                name: None,
                guid: None,
                path: Some("layer"),
                after: None,
            }
            .label(),
            "(unnamed)"
        );
    }
}
