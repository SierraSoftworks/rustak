//! `<marti><dest mission="…">`: the stream's way into a Data Sync.
//!
//! A `<dest>` naming a mission is a **write**, not a delivery. The message is
//! filed under the mission, an `ADD_CONTENT` change is appended, the connected
//! subscribers are told about the change with a `t-x-m-c`, and — separately —
//! the raw CoT is relayed to those same subscribers by the router, which is
//! what the uids returned from [`publish`](MissionIngest::publish) are for.
//! Both happen; neither replaces the other.
//!
//! # Refusing is silent
//!
//! A sender with no subscription, or one whose role cannot write, has its
//! message dropped *for that mission* and nothing else: the connection is not
//! disturbed and any other `<dest>` on the same message still applies. That is
//! TAK Server's behaviour and it is the right one — a client addressing a
//! mission it was removed from should stop being heard, not be disconnected
//! mid-exercise.
//!
//! # Who the sender is
//!
//! Looked up by `clientUid` first, then by account name. TAK Server has a third
//! fallback to the certificate's common name, which rustak does not need
//! separately: the stream resolver already derives the principal's username
//! from that common name, so the second lookup covers the same case.

use std::sync::Arc;

use async_trait::async_trait;
use rustak_cot::codec::EncodedEvent;
use uuid::Uuid;

use crate::marti::{MartiError, MissionRef as MissionIdent};
use crate::prelude::*;
use crate::stream::{MissionIngest, MissionRef};

use super::dto::MissionContentBody;
use super::model::Mission;
use super::roles::{Permission, Role};
use super::service::MissionService;

/// A mission failure, as the router's error type spells one.
///
/// The ingest seam predates the Marti error type and answers
/// [`human_errors::Error`], which is the right shape for it: the router's only
/// response to a failure here is to route the message nowhere, and there is no
/// HTTP status for it to carry.
fn ingest_failure(err: MartiError) -> Error {
    human_errors::system(
        format!("A mission destination could not be written: {err}"),
        crate::db::ADVICE_REPORT_DEV,
    )
}

/// The real `<dest mission>` publisher, installed in place of the M1 stub.
#[derive(Clone)]
pub struct MissionPublisher {
    service: MissionService,
}

impl MissionPublisher {
    /// A publisher over one application context.
    pub fn new(context: AppContext) -> Self {
        Self {
            service: MissionService::new(context),
        }
    }

    /// The publisher, behind the `Arc` the router holds.
    pub fn shared(context: AppContext) -> Arc<dyn MissionIngest> {
        Arc::new(Self::new(context))
    }

    /// The mission a `<dest>` names, by name first and then by guid.
    ///
    /// Name first because that is how every client addresses a mission on the
    /// stream, even the ones that prefer guids for REST.
    async fn resolve(&self, dest: MissionRef<'_>) -> Option<Mission> {
        if let Some(name) = dest.name.filter(|name| !name.trim().is_empty())
            && let Ok(Some(mission)) = self.service.by_name(name.trim()).await
        {
            return Some(mission);
        }

        let guid = Uuid::parse_str(dest.guid?.trim()).ok()?;

        self.service
            .resolve(&MissionIdent::Guid(guid))
            .await
            .map_err(|err| {
                debug!(mission = dest.label(), error = %err, "A mission destination named nothing.");
            })
            .ok()
    }

    /// The sender's role on this mission, by device uid then by account.
    async fn role_of_sender(
        &self,
        mission: &Mission,
        sender: &Principal,
    ) -> Result<Option<(String, Role)>, Error> {
        if let Some(uid) = sender.device.as_ref().map(ToString::to_string)
            && let Some(role) = self
                .service
                .role_of(mission, &uid)
                .await
                .map_err(ingest_failure)?
        {
            return Ok(Some((uid, role)));
        }

        let name = sender.username.to_string();
        let subscriptions = self
            .service
            .subscriptions(mission)
            .await
            .map_err(ingest_failure)?;

        Ok(subscriptions
            .into_iter()
            .find(|held| {
                held.username
                    .as_deref()
                    .is_some_and(|held| held.eq_ignore_ascii_case(&name))
            })
            .map(|held| (held.client_uid, held.role)))
    }
}

impl std::fmt::Debug for MissionPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MissionPublisher").finish_non_exhaustive()
    }
}

#[async_trait]
impl MissionIngest for MissionPublisher {
    async fn publish(
        &self,
        dest: MissionRef<'_>,
        sender: &Principal,
        event: &EncodedEvent,
    ) -> Result<Vec<String>, Error> {
        let Some(mission) = self.resolve(dest).await else {
            debug!(
                mission = dest.label(),
                sender = %sender.username,
                "A message named a mission this server does not have."
            );

            return Ok(Vec::new());
        };

        let Some((client_uid, role)) = self.role_of_sender(&mission, sender).await? else {
            info!(
                mission = mission.name,
                sender = %sender.username,
                "A message named a mission its sender is not subscribed to."
            );

            return Ok(Vec::new());
        };

        if !role.allows(Permission::Write) {
            info!(
                mission = mission.name,
                sender = %sender.username,
                role = role.as_str(),
                "A message named a mission its sender may only read."
            );

            return Ok(Vec::new());
        }

        let recipients = self
            .service
            .subscribers_for(&mission, Some(&client_uid))
            .await
            .map_err(ingest_failure)?;

        let body = MissionContentBody {
            uids: vec![event.event().uid.clone()],
            paths: dest.path.map(|path| {
                [(
                    path.to_string(),
                    vec![MissionContentBody {
                        uids: vec![event.event().uid.clone()],
                        after: dest.after.map(ToOwned::to_owned),
                        ..MissionContentBody::default()
                    }],
                )]
                .into_iter()
                .collect()
            }),
            after: dest.after.map(ToOwned::to_owned),
            ..MissionContentBody::default()
        };

        // A `paths` body already carries the uid under its layer; carrying it
        // at the top level too would file the same item twice.
        let body = match body.paths {
            Some(_) => MissionContentBody {
                uids: Vec::new(),
                ..body
            },
            None => body,
        };

        let at = event
            .event()
            .time
            .to_datetime()
            .unwrap_or_else(chrono::Utc::now);
        // `add_content` emits the `t-x-m-c` itself, to the same subscribers the
        // raw relay reaches — both deliveries happen, and neither is the other.
        self.service
            .add_content(&mission, &body, Some(&client_uid), at)
            .await
            .map_err(ingest_failure)?;

        debug!(
            mission = mission.name,
            uid = event.event().uid,
            subscribers = recipients.len(),
            "Filed a streamed message under a mission."
        );

        Ok(recipients)
    }
}

#[cfg(test)]
mod tests {
    use rustak_cot::Event;

    use super::*;

    fn principal(name: &str, device: Option<&str>) -> Principal {
        let base = Principal::new(
            UserId::from(1),
            Username::parse(name).unwrap(),
            PrincipalKind::Person,
            AuthMethod::SetupToken,
        );

        match device {
            Some(uid) => base.with_device(DeviceUid::parse(uid).unwrap()),
            None => base,
        }
    }

    async fn publisher() -> (AppContext, MissionPublisher) {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let publisher = MissionPublisher::new(context.clone());

        (context, publisher)
    }

    fn event(uid: &str) -> EncodedEvent {
        EncodedEvent::new(Event::builder("a-f-G-U-C", uid).point(51.5, -0.12).build())
    }

    #[tokio::test]
    async fn a_message_for_a_mission_that_is_not_here_reaches_nobody() {
        // Accepted rather than refused: a client addressing a mission this
        // server has never had must not have its connection disturbed.
        let (_context, publisher) = publisher().await;

        let recipients = publisher
            .publish(
                MissionRef {
                    name: Some("Nowhere"),
                    guid: None,
                    path: None,
                    after: None,
                },
                &principal("alice", Some("ANDROID-1")),
                &event("UID-A"),
            )
            .await
            .expect("an unknown mission is not an error");

        assert!(recipients.is_empty());
    }

    #[tokio::test]
    async fn a_sender_with_no_subscription_is_ignored() {
        let (context, publisher) = publisher().await;
        context
            .db()
            .missions()
            .create(crate::db::repos::NewMission::new(
                "Kettle",
                Role::Subscriber.as_str(),
            ))
            .await
            .unwrap();

        let recipients = publisher
            .publish(
                MissionRef {
                    name: Some("Kettle"),
                    guid: None,
                    path: None,
                    after: None,
                },
                &principal("alice", Some("ANDROID-1")),
                &event("UID-A"),
            )
            .await
            .expect("an unsubscribed sender is not an error");

        assert!(recipients.is_empty());
        assert!(
            context
                .db()
                .mission_contents()
                .uids(1)
                .await
                .unwrap()
                .is_empty(),
            "nothing was filed",
        );
    }

    #[tokio::test]
    async fn a_read_only_subscriber_files_nothing() {
        let (context, publisher) = publisher().await;
        let mission = context
            .db()
            .missions()
            .create(crate::db::repos::NewMission::new(
                "Kettle",
                Role::Subscriber.as_str(),
            ))
            .await
            .unwrap();

        context
            .db()
            .mission_subscriptions()
            .upsert(crate::db::repos::NewSubscription {
                mission_id: mission.id,
                subscription_uid: Uuid::new_v4().to_string(),
                client_uid: "ANDROID-1".to_string(),
                username: Some("alice".to_string()),
                role: Role::ReadonlySubscriber.as_str().to_string(),
                token_jti: None,
            })
            .await
            .unwrap();

        let recipients = publisher
            .publish(
                MissionRef {
                    name: Some("Kettle"),
                    guid: None,
                    path: None,
                    after: None,
                },
                &principal("alice", Some("ANDROID-1")),
                &event("UID-A"),
            )
            .await
            .unwrap();

        assert!(recipients.is_empty());
        assert!(
            context
                .db()
                .mission_contents()
                .uids(mission.id)
                .await
                .unwrap()
                .is_empty(),
        );
    }

    #[tokio::test]
    async fn a_subscriber_that_may_write_files_the_message_and_reaches_nobody_offline() {
        // The recipients are the *connected* subscribers; with no stream
        // listener there are none, and the write still happens.
        let (context, publisher) = publisher().await;
        let mission = context
            .db()
            .missions()
            .create(crate::db::repos::NewMission::new(
                "Kettle",
                Role::Subscriber.as_str(),
            ))
            .await
            .unwrap();

        context
            .db()
            .mission_subscriptions()
            .upsert(crate::db::repos::NewSubscription {
                mission_id: mission.id,
                subscription_uid: Uuid::new_v4().to_string(),
                client_uid: "ANDROID-1".to_string(),
                username: Some("alice".to_string()),
                role: Role::Subscriber.as_str().to_string(),
                token_jti: None,
            })
            .await
            .unwrap();

        let recipients = publisher
            .publish(
                MissionRef {
                    name: Some("Kettle"),
                    guid: None,
                    path: None,
                    after: None,
                },
                &principal("alice", Some("ANDROID-1")),
                &event("UID-A"),
            )
            .await
            .unwrap();

        assert!(recipients.is_empty(), "nobody is connected");
        let filed = context
            .db()
            .mission_contents()
            .uids(mission.id)
            .await
            .unwrap();
        assert_eq!(filed.len(), 1);
        assert_eq!(filed[0].uid, "UID-A");
        assert_eq!(
            context
                .db()
                .mission_changes()
                .window(
                    mission.id,
                    chrono::DateTime::UNIX_EPOCH,
                    chrono::Utc::now() + chrono::Duration::hours(1),
                )
                .await
                .unwrap()
                .len(),
            1,
            "one ADD_CONTENT was appended",
        );
    }

    #[tokio::test]
    async fn a_sender_is_matched_by_account_when_its_device_is_not_subscribed() {
        let (context, publisher) = publisher().await;
        let mission = context
            .db()
            .missions()
            .create(crate::db::repos::NewMission::new(
                "Kettle",
                Role::Subscriber.as_str(),
            ))
            .await
            .unwrap();

        context
            .db()
            .mission_subscriptions()
            .upsert(crate::db::repos::NewSubscription {
                mission_id: mission.id,
                subscription_uid: Uuid::new_v4().to_string(),
                client_uid: "ANDROID-OTHER".to_string(),
                username: Some("Alice".to_string()),
                role: Role::Subscriber.as_str().to_string(),
                token_jti: None,
            })
            .await
            .unwrap();

        publisher
            .publish(
                MissionRef {
                    name: Some("Kettle"),
                    guid: None,
                    path: None,
                    after: None,
                },
                &principal("alice", Some("ANDROID-1")),
                &event("UID-A"),
            )
            .await
            .unwrap();

        let filed = context
            .db()
            .mission_contents()
            .uids(mission.id)
            .await
            .unwrap();

        assert_eq!(filed.len(), 1, "the account's subscription carried it");
        assert_eq!(filed[0].creator_uid.as_deref(), Some("ANDROID-OTHER"));
    }
}
