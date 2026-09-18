//! The three mission roles and the eight permissions they grant.
//!
//! A mission role is not an account right. It answers "what may *this* caller
//! do to *this* mission", and the same person may own one mission, read another
//! and have no role at all on a third. That is why it is resolved per request
//! — from a mission token, from a subscription, or by falling back to the
//! mission's default — rather than carried on the [`Principal`].
//!
//! [`Principal`]: rustak_core::identity::Principal
//!
//! # Why the permission set is fixed
//!
//! ATAK and CloudTAK both render a role by listing its permissions, and both
//! compare the strings. The eight names below are TAK Server's, the mapping
//! from role to permissions is TAK Server's, and neither is ours to tidy: a
//! ninth permission would be rendered by clients that have never heard of it.

use crate::auth::{MissionClaims, TokenType};
use crate::marti::{MartiError, MartiPrincipal};
use crate::prelude::*;

use super::model::Mission;
use super::service::MissionService;

/// What a caller may do to a mission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    Read,
    Write,
    Delete,
    SetRole,
    SetPassword,
    UpdateGroups,
    ManageFeeds,
    ManageLayers,
}

impl Permission {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "MISSION_READ",
            Self::Write => "MISSION_WRITE",
            Self::Delete => "MISSION_DELETE",
            Self::SetRole => "MISSION_SET_ROLE",
            Self::SetPassword => "MISSION_SET_PASSWORD",
            Self::UpdateGroups => "MISSION_UPDATE_GROUPS",
            Self::ManageFeeds => "MISSION_MANAGE_FEEDS",
            Self::ManageLayers => "MISSION_MANAGE_LAYERS",
        }
    }
}

/// Every permission, in the order a role reports them.
const ALL: &[Permission] = &[
    Permission::Read,
    Permission::Write,
    Permission::Delete,
    Permission::SetRole,
    Permission::SetPassword,
    Permission::UpdateGroups,
    Permission::ManageFeeds,
    Permission::ManageLayers,
];

/// What a subscriber may do.
const SUBSCRIBER: &[Permission] = &[Permission::Read, Permission::Write];

/// What a read-only subscriber may do.
const READONLY: &[Permission] = &[Permission::Read];

/// One of the three roles a mission knows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    /// Everything, including deleting the mission and changing its password.
    Owner,
    /// Read and write; the default a mission is created with, and the default
    /// role a mission hands an unlisted caller.
    #[default]
    Subscriber,
    /// Read only.
    ReadonlySubscriber,
}

impl Role {
    /// The wire spelling, which is also what the database stores.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "MISSION_OWNER",
            Self::Subscriber => "MISSION_SUBSCRIBER",
            Self::ReadonlySubscriber => "MISSION_READONLY_SUBSCRIBER",
        }
    }

    /// The role a stored or requested spelling names.
    ///
    /// Case-insensitive, because ATAK sends `MISSION_SUBSCRIBER` and some
    /// tooling sends it lower-cased, and refusing the second would be a
    /// difference nobody could see in a log.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "MISSION_OWNER" => Some(Self::Owner),
            "MISSION_SUBSCRIBER" => Some(Self::Subscriber),
            "MISSION_READONLY_SUBSCRIBER" => Some(Self::ReadonlySubscriber),
            _ => None,
        }
    }

    /// What this role grants.
    pub fn permissions(self) -> &'static [Permission] {
        match self {
            Self::Owner => ALL,
            Self::Subscriber => SUBSCRIBER,
            Self::ReadonlySubscriber => READONLY,
        }
    }

    /// Whether this role grants a permission.
    pub fn allows(self, permission: Permission) -> bool {
        self.permissions().contains(&permission)
    }
}

/// A role as one request resolved it.
///
/// `using_default` records that nothing named this caller specifically — they
/// got the mission's default role. It is what tells an audit line "this was a
/// public mission" apart from "this device is a subscriber".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissionRole {
    /// The role itself.
    pub kind: Role,
    /// Whether it came from the mission's default rather than from a
    /// subscription, an invitation or an administrator's rights.
    pub using_default: bool,
}

impl MissionRole {
    /// A role a subscription, invitation or administrator granted.
    pub fn granted(kind: Role) -> Self {
        Self {
            kind,
            using_default: false,
        }
    }

    /// A role the mission's default granted.
    pub fn default_role(kind: Role) -> Self {
        Self {
            kind,
            using_default: true,
        }
    }

    /// Whether this role grants a permission.
    pub fn allows(self, permission: Permission) -> bool {
        self.kind.allows(permission)
    }
}

/// Refuses a request whose role does not carry a permission.
///
/// A caller with no role at all and a caller whose role is too narrow are both
/// `403`: the first has already been identified (or deliberately not), and a
/// `401` would send a client that holds a perfectly good certificate back to
/// re-authenticate for no reason.
///
/// # Errors
///
/// [`MartiError::Forbidden`] naming the permission that was missing.
pub fn require(
    role: Option<MissionRole>,
    permission: Permission,
) -> Result<MissionRole, MartiError> {
    match role {
        Some(role) if role.allows(permission) => Ok(role),
        _ => Err(MartiError::Forbidden(format!(
            "{} is required on this mission",
            permission.as_str()
        ))),
    }
}

/// Resolving the role one request carries on one mission.
///
/// Four sources, in order: a mission token, this installation's
/// administrators, the caller's own subscription, and the mission's default.
/// The third is ours rather than TAK Server's — TAK insists on the token — and
/// it is what lets a client create a mission over an ordinary session and then
/// manage it without replaying the token it was handed. It grants nothing a
/// token would not: the subscription it reads was created for that account.
///
/// A password-protected or invite-only mission has **no** default role, so a
/// caller matching none of the first three gets nothing at all rather than read
/// access, which is the whole point of those two flags.
impl MissionService {
    /// The role a mission token proves, when it proves one.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a read fails.
    pub async fn role_from_token(
        &self,
        mission: &Mission,
        allowed: &[TokenType],
        who: &MartiPrincipal,
        claims: Option<&MissionClaims>,
    ) -> Result<Option<MissionRole>, MartiError> {
        if who.is_admin() {
            return Ok(Some(MissionRole::granted(Role::Owner)));
        }

        let Some(claims) = claims.filter(|claims| allowed.contains(&claims.kind)) else {
            return Ok(None);
        };

        // Either spelling identifies the mission, so that renaming one does not
        // invalidate the tokens already issued for it.
        if claims.mission_name != mission.name && claims.mission_guid != mission.guid {
            return Ok(None);
        }

        match claims.kind {
            TokenType::Access => Ok(Some(MissionRole::default_role(mission.default_role))),
            TokenType::Subscription => Ok(self
                .db()
                .mission_subscriptions()
                .by_subscription_uid(claims.id.clone())
                .await?
                .filter(|row| row.mission_id == mission.id)
                .and_then(|row| Role::parse(&row.role))
                .map(MissionRole::granted)),
            // Matched by the row the token names rather than by the token's
            // bytes: the stored copy and the one a client replays are the same
            // JWT, but comparing identifiers is what survives a re-issue.
            TokenType::Invitation => {
                let Ok(id) = claims.id.parse::<i64>() else {
                    return Ok(None);
                };

                Ok(self
                    .invitation(id)
                    .await?
                    .filter(|invitation| invitation.mission_id == mission.id)
                    .map(|invitation| MissionRole::granted(invitation.role)))
            }
        }
    }

    /// The role this request carries on this mission, if any.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a read fails.
    pub async fn role_for_request(
        &self,
        mission: &Mission,
        who: &MartiPrincipal,
        claims: Option<&MissionClaims>,
    ) -> Result<Option<MissionRole>, MartiError> {
        if let Some(role) = self
            .role_from_token(
                mission,
                &[TokenType::Access, TokenType::Subscription],
                who,
                claims,
            )
            .await?
        {
            return Ok(Some(role));
        }

        if let Some(role) = self.role_of_caller(mission, who).await? {
            return Ok(Some(MissionRole::granted(role)));
        }

        if mission.is_password_protected() || mission.invite_only {
            return Ok(None);
        }

        Ok(Some(MissionRole::default_role(mission.default_role)))
    }

    /// The role one device's subscription carries.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn role_of(
        &self,
        mission: &Mission,
        client_uid: &str,
    ) -> Result<Option<Role>, MartiError> {
        Ok(self
            .db()
            .mission_subscriptions()
            .by_client(mission.id, client_uid.to_string())
            .await?
            .and_then(|row| Role::parse(&row.role)))
    }

    /// The role the caller's own subscription carries, by device or account.
    ///
    /// The **most permissive** of them when there are several: one person may
    /// hold an owner subscription from the device they created the mission on
    /// and an ordinary one from the device they are asking with, and the second
    /// is not a demotion.
    async fn role_of_caller(
        &self,
        mission: &Mission,
        who: &MartiPrincipal,
    ) -> Result<Option<Role>, MartiError> {
        let Some(principal) = who.principal() else {
            return Ok(None);
        };

        let device = principal.device.as_ref().map(ToString::to_string);
        let username = who.username().map(str::to_string);

        Ok(self
            .db()
            .mission_subscriptions()
            .list(mission.id)
            .await?
            .into_iter()
            .filter(|row| {
                device.as_deref() == Some(row.client_uid.as_str())
                    || matches!(
                        (&username, &row.username),
                        (Some(ours), Some(theirs)) if ours.eq_ignore_ascii_case(theirs)
                    )
            })
            .filter_map(|row| Role::parse(&row.role))
            .max_by_key(|role| role.permissions().len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_owner_holds_every_permission_and_a_reader_holds_one() {
        assert_eq!(Role::Owner.permissions().len(), 8);
        assert_eq!(
            Role::Subscriber.permissions(),
            &[Permission::Read, Permission::Write]
        );
        assert_eq!(Role::ReadonlySubscriber.permissions(), &[Permission::Read]);
    }

    #[test]
    fn the_stored_spelling_round_trips() {
        for role in [Role::Owner, Role::Subscriber, Role::ReadonlySubscriber] {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }

        assert_eq!(Role::parse("mission_owner"), Some(Role::Owner));
        assert_eq!(Role::parse("MISSION_ADMIN"), None);
    }

    #[test]
    fn a_missing_role_and_a_narrow_one_are_both_refused() {
        let narrow = MissionRole::default_role(Role::ReadonlySubscriber);

        assert!(require(None, Permission::Read).is_err());
        assert!(require(Some(narrow), Permission::Write).is_err());
        assert!(require(Some(narrow), Permission::Read).is_ok());
    }

    #[test]
    fn a_default_role_says_so() {
        assert!(MissionRole::default_role(Role::Subscriber).using_default);
        assert!(!MissionRole::granted(Role::Subscriber).using_default);
    }
}
