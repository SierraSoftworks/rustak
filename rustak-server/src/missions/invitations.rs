//! Standing invitations: who may join a mission they cannot otherwise see.
//!
//! An invitation is a row saying "whoever matches this may subscribe, with this
//! role", plus a `t-x-m-i` pushed at whoever matches it *right now*. The two
//! halves are independent: a device that was offline when the invitation was
//! written still finds it, because `GET /Marti/api/missions/invitations` is
//! what CloudTAK calls on every list page and ATAK calls on connect.
//!
//! # Five ways to name somebody
//!
//! `clientUid`, `callsign`, `userName`, `group` and `team` — spelled exactly
//! that way, capital `N` included, because the type is a path segment a client
//! writes. Only the first and third are stable identities; the other three are
//! whatever the device is calling itself at the moment it asks, which is why
//! matching happens at subscribe time rather than being resolved to a uid when
//! the invitation is written.
//!
//! # The listing must never be a 404
//!
//! CloudTAK fetches the invitation list in parallel with the mission list and
//! treats a failure as a failure of the whole page. A caller with no
//! invitations gets an empty array.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;

use crate::auth::TokenType;
use crate::db::row::{Timestamp, ts};
use crate::marti::MartiError;

use super::invitees::InviteTarget;
use super::model::Mission;
use super::roles::Role;
use super::service::MissionService;

/// The five spellings `PUT …/invite/{type}/{invitee}` accepts.
pub const INVITEE_TYPES: &[&str] = &["clientUid", "callsign", "userName", "group", "team"];

/// Every column [`MissionInvitation::from_row`] reads, in order.
const COLUMNS: &str = "id, mission_id, invitee_type, invitee, creator_uid, role, created_at, token";

/// One standing invitation.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionInvitation {
    pub id: i64,
    pub mission_id: i64,
    /// One of [`INVITEE_TYPES`].
    pub kind: String,
    /// The value matched against, in the vocabulary the type names.
    pub invitee: String,
    pub creator_uid: Option<String>,
    pub role: Role,
    pub create_time: DateTime<Utc>,
    /// The whole `INVITATION` JWT, which is what an invited client replays and
    /// what `role_from_token` matches against.
    pub token: Option<String>,
}

impl MissionInvitation {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let role: String = row.get(5)?;

        Ok(Self {
            id: row.get(0)?,
            mission_id: row.get(1)?,
            kind: row.get(2)?,
            invitee: row.get(3)?,
            creator_uid: row.get(4)?,
            role: Role::parse(&role).unwrap_or(Role::ReadonlySubscriber),
            create_time: ts(row, 6)?,
            token: row.get(7)?,
        })
    }
}

impl MissionService {
    /// Writes an invitation, replacing one that already names the same target.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] for a type outside [`INVITEE_TYPES`], and
    /// a system error if the write fails.
    pub async fn invite(
        &self,
        mission: &Mission,
        kind: &str,
        invitee: &str,
        creator_uid: Option<&str>,
        role: Role,
    ) -> Result<MissionInvitation, MartiError> {
        if !INVITEE_TYPES.contains(&kind) {
            return Err(MartiError::InvalidRequest(format!(
                "invitee type must be one of {}",
                INVITEE_TYPES.join(", ")
            )));
        }

        let (kind, invitee) = (kind.to_string(), invitee.to_string());
        let creator = creator_uid.map(ToOwned::to_owned);
        let role_name = role.as_str().to_string();
        let mission_id = mission.id;

        let mut stored = self
            .db()
            .write(move |tx| {
                tx.query_one(
                    &format!(
                        "INSERT INTO mission_invitations \
                           (mission_id, invitee_type, invitee, creator_uid, role, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                         ON CONFLICT (mission_id, invitee_type, invitee) DO UPDATE SET \
                           creator_uid = excluded.creator_uid, role = excluded.role \
                         RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        mission_id,
                        kind,
                        invitee,
                        creator,
                        role_name,
                        Timestamp::now()
                    ],
                    MissionInvitation::from_row,
                )
            })
            .await?;

        // Minted after the insert because the token names the row, and stored
        // whole because that is what an invited client replays verbatim.
        let token = self.invitation_token(mission, &stored).await?;
        let (id, held) = (stored.id, token.clone());

        self.db()
            .write(move |tx| {
                tx.execute(
                    "UPDATE mission_invitations SET token = ?2 WHERE id = ?1",
                    rusqlite::params![id, held],
                )
            })
            .await?;

        stored.token = Some(token);

        Ok(stored)
    }

    /// Withdraws an invitation, reporting whether there was one.
    ///
    /// # Errors
    ///
    /// A system error if the write fails.
    pub async fn uninvite(
        &self,
        mission: &Mission,
        kind: &str,
        invitee: &str,
    ) -> Result<bool, MartiError> {
        let (kind, invitee) = (kind.to_string(), invitee.to_string());
        let mission_id = mission.id;

        Ok(self
            .db()
            .write(move |tx| {
                Ok(tx.execute(
                    "DELETE FROM mission_invitations \
                     WHERE mission_id = ?1 AND invitee_type = ?2 AND invitee = ?3",
                    rusqlite::params![mission_id, kind, invitee],
                )? > 0)
            })
            .await?)
    }

    /// Every invitation standing against one mission.
    ///
    /// # Errors
    ///
    /// A system error if the read fails.
    pub async fn invitations(
        &self,
        mission: &Mission,
    ) -> Result<Vec<MissionInvitation>, MartiError> {
        let mission_id = mission.id;

        Ok(self
            .db()
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM mission_invitations \
                     WHERE mission_id = ?1 ORDER BY id"
                ))?;

                statement
                    .query_map(rusqlite::params![mission_id], MissionInvitation::from_row)?
                    .collect()
            })
            .await?)
    }

    /// One invitation by its row identifier, which is what a token names.
    ///
    /// # Errors
    ///
    /// A system error if the read fails.
    pub async fn invitation(&self, id: i64) -> Result<Option<MissionInvitation>, MartiError> {
        Ok(self
            .db()
            .read(move |c| {
                c.query_row(
                    &format!("SELECT {COLUMNS} FROM mission_invitations WHERE id = ?1"),
                    rusqlite::params![id],
                    MissionInvitation::from_row,
                )
                .optional()
            })
            .await?)
    }

    /// Every invitation the caller matches, across every mission.
    ///
    /// # Errors
    ///
    /// A system error if a read fails.
    pub async fn invitations_matching(
        &self,
        target: &InviteTarget,
    ) -> Result<Vec<(Mission, MissionInvitation)>, MartiError> {
        let rows: Vec<MissionInvitation> = self
            .db()
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM mission_invitations ORDER BY id"
                ))?;

                statement
                    .query_map([], MissionInvitation::from_row)?
                    .collect()
            })
            .await?;

        let mut matched = Vec::new();

        for invitation in rows.into_iter().filter(|row| row.matches(target)) {
            let Some(row) = self.db().missions().by_id(invitation.mission_id).await? else {
                continue;
            };

            if row.deleted_at.is_none() {
                matched.push((Mission::from_row(row), invitation));
            }
        }

        Ok(matched)
    }

    /// The `INVITATION` token an invitation hands out.
    ///
    /// # Errors
    ///
    /// A system error if the token cannot be signed.
    pub async fn invitation_token(
        &self,
        mission: &Mission,
        invitation: &MissionInvitation,
    ) -> Result<String, MartiError> {
        Ok(self.tokens().await?.issue(
            &invitation.id.to_string(),
            TokenType::Invitation,
            &mission.name,
            mission.guid,
            None,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    use super::*;

    fn invitation(kind: &str, invitee: &str) -> MissionInvitation {
        MissionInvitation {
            id: 1,
            mission_id: 1,
            kind: kind.to_string(),
            invitee: invitee.to_string(),
            creator_uid: None,
            role: Role::Subscriber,
            create_time: DateTime::UNIX_EPOCH,
            token: None,
        }
    }

    fn target() -> InviteTarget {
        InviteTarget {
            client_uid: Some("ANDROID-1".to_string()),
            username: Some("alice".to_string()),
            callsigns: vec!["ALPHA".to_string()],
            groups: vec!["Blue".to_string()],
            teams: vec!["Cyan".to_string()],
        }
    }

    #[test]
    fn each_vocabulary_matches_only_its_own_field() {
        let target = target();

        assert!(invitation("clientUid", "ANDROID-1").matches(&target));
        assert!(!invitation("clientUid", "ANDROID-2").matches(&target));
        assert!(invitation("callsign", "alpha").matches(&target));
        assert!(invitation("userName", "ALICE").matches(&target));
        assert!(invitation("group", "blue").matches(&target));
        assert!(invitation("team", "cyan").matches(&target));
        assert!(
            !invitation("clientUid", "ALPHA").matches(&target),
            "a callsign is not a device uid",
        );
    }

    #[test]
    fn an_unknown_vocabulary_matches_nothing() {
        // The column has a check constraint, so this only happens if somebody
        // writes around the schema — and then the safe reading is "no".
        assert!(!invitation("email", "alice@example.com").matches(&target()));
    }

    #[test]
    fn the_five_spellings_are_the_ones_a_client_writes() {
        assert_eq!(
            INVITEE_TYPES,
            &["clientUid", "callsign", "userName", "group", "team"],
            "userName carries a capital N on the wire",
        );
    }

    #[tokio::test]
    async fn an_invitation_replaces_the_one_it_repeats_rather_than_duplicating_it() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let service = MissionService::new(context.clone());
        let row = context
            .db()
            .missions()
            .create(crate::db::repos::NewMission::new(
                "Kettle",
                Role::Subscriber.as_str(),
            ))
            .await
            .unwrap();
        let mission = Mission::from_row(row);

        service
            .invite(&mission, "clientUid", "ANDROID-1", None, Role::Subscriber)
            .await
            .unwrap();
        let second = service
            .invite(
                &mission,
                "clientUid",
                "ANDROID-1",
                Some("ANDROID-9"),
                Role::ReadonlySubscriber,
            )
            .await
            .unwrap();

        let standing = service.invitations(&mission).await.unwrap();

        assert_eq!(standing.len(), 1);
        assert_eq!(standing[0].role, Role::ReadonlySubscriber);
        assert_eq!(standing[0].id, second.id);
        assert_eq!(
            service.invitation(second.id).await.unwrap().unwrap(),
            second
        );
    }

    #[tokio::test]
    async fn an_unknown_invitee_type_is_refused_before_it_reaches_the_schema() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let service = MissionService::new(context.clone());
        let row = context
            .db()
            .missions()
            .create(crate::db::repos::NewMission::new(
                "Kettle",
                Role::Subscriber.as_str(),
            ))
            .await
            .unwrap();

        let refused = service
            .invite(
                &Mission::from_row(row),
                "email",
                "alice@example.com",
                None,
                Role::Subscriber,
            )
            .await;

        assert!(matches!(refused, Err(MartiError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn subscribing_clears_the_invitations_that_named_the_device() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let service = MissionService::new(context.clone());
        let row = context
            .db()
            .missions()
            .create(crate::db::repos::NewMission::new(
                "Kettle",
                Role::Subscriber.as_str(),
            ))
            .await
            .unwrap();
        let mission = Mission::from_row(row);

        for (kind, invitee) in [
            ("clientUid", "ANDROID-1"),
            ("callsign", "ALPHA"),
            ("group", "Blue"),
        ] {
            service
                .invite(&mission, kind, invitee, None, Role::Subscriber)
                .await
                .unwrap();
        }

        let cleared = service
            .clear_invitations(&mission, &target())
            .await
            .unwrap();

        assert_eq!(cleared, 2);
        let left = service.invitations(&mission).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].kind, "group", "a channel invitation stands");
    }
}
