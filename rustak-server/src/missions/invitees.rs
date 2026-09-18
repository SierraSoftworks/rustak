//! Who an invitation names, and who it reaches.
//!
//! Split from [`invitations`](super::invitations) because matching is a
//! different question from storage: a row says `callsign=ALPHA`, and answering
//! whether *this* caller is ALPHA means asking the live registry what the
//! device is calling itself right now. The five vocabularies are checked at
//! subscribe time for exactly that reason — three of them are facts about a
//! connection rather than about an account.

use crate::marti::{MartiError, MartiPrincipal};

use super::invitations::MissionInvitation;
use super::model::Mission;
use super::roles::Role;
use super::service::MissionService;

/// Whether a list holds a value, ignoring case.
fn contains(values: &[String], wanted: &str) -> bool {
    values.iter().any(|held| held.eq_ignore_ascii_case(wanted))
}

impl MissionInvitation {
    /// Whether this invitation names `client_uid`, `username` or one of the
    /// caller's callsigns, channels or teams.
    pub(super) fn matches(&self, target: &InviteTarget) -> bool {
        match self.kind.as_str() {
            "clientUid" => target.client_uid.as_deref() == Some(self.invitee.as_str()),
            "callsign" => contains(&target.callsigns, &self.invitee),
            "userName" => target
                .username
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(&self.invitee)),
            "group" => contains(&target.groups, &self.invitee),
            "team" => contains(&target.teams, &self.invitee),
            _ => false,
        }
    }
}

/// Everything an invitation could be matched against for one caller.
///
/// Assembled once per request rather than per invitation, because three of the
/// five vocabularies are answered by the live registry and one by the database.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InviteTarget {
    pub client_uid: Option<String>,
    pub username: Option<String>,
    pub callsigns: Vec<String>,
    pub groups: Vec<String>,
    pub teams: Vec<String>,
}

impl MissionService {
    /// Everything the caller could be invited as.
    ///
    /// # Errors
    ///
    /// Whatever reading the caller's channels reported.
    pub async fn invite_target(
        &self,
        who: &MartiPrincipal,
        client_uid: Option<&str>,
    ) -> Result<InviteTarget, MartiError> {
        let viewer = self.viewer(who).await?;
        let mut target = InviteTarget {
            client_uid: client_uid.map(ToOwned::to_owned),
            username: who.username().map(ToOwned::to_owned),
            groups: viewer.held_groups,
            ..InviteTarget::default()
        };

        if !self.context.has_live() {
            return Ok(target);
        }

        let Ok(live) = self.context.live() else {
            return Ok(target);
        };

        for endpoint in live.snapshot() {
            let is_caller = Some(endpoint.uid.as_str()) == target.client_uid.as_deref()
                || target
                    .username
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&endpoint.username));

            if !is_caller {
                continue;
            }

            target.callsigns.push(endpoint.callsign);
            if !endpoint.team.is_empty() {
                target.teams.push(endpoint.team);
            }
        }

        Ok(target)
    }

    /// Everything a subscribing device could be invited as.
    ///
    /// Built from the device uid and account name a subscribe carries rather
    /// than from a [`MartiPrincipal`], because the two need not be the same
    /// person: a client may subscribe on behalf of a uid it is not itself.
    ///
    /// # Errors
    ///
    /// A system error if a read fails.
    pub async fn subscribe_target(
        &self,
        client_uid: &str,
        username: Option<&str>,
    ) -> Result<InviteTarget, MartiError> {
        let mut target = InviteTarget {
            client_uid: Some(client_uid.to_string()),
            username: username.map(ToOwned::to_owned),
            ..InviteTarget::default()
        };

        if username.is_some() {
            target.groups = crate::files::viewer_for(self.db(), username, None)
                .await?
                .held_groups;
        }

        let Ok(live) = self.context.live() else {
            return Ok(target);
        };

        for endpoint in live.snapshot() {
            if endpoint.uid != client_uid {
                continue;
            }

            target.callsigns.push(endpoint.callsign);
            if !endpoint.team.is_empty() {
                target.teams.push(endpoint.team);
            }
        }

        Ok(target)
    }

    /// Fills a subscribe request's `invited_role` from a standing invitation.
    ///
    /// Answers the target it matched against, which the caller needs again
    /// afterwards to spend the invitations that named this device.
    ///
    /// # Errors
    ///
    /// A system error if a read fails.
    pub(super) async fn apply_invitation(
        &self,
        mission: &Mission,
        request: &mut super::subscriptions::SubscribeReq,
    ) -> Result<InviteTarget, MartiError> {
        let target = self
            .subscribe_target(&request.client_uid, request.username.as_deref())
            .await?;

        if request.invited_role.is_none() {
            request.invited_role = self.invited_role(mission, &target).await?;
        }

        Ok(target)
    }

    /// Stores the subscription and spends the invitations that named it.
    ///
    /// The two go together: an invitation naming this device has done its job
    /// the moment the subscription exists, and leaving it standing would let a
    /// subscriber that had been removed re-join on the strength of it. An
    /// invitation naming a channel or a team is left alone — it stands for
    /// everybody else in it.
    ///
    /// # Errors
    ///
    /// A system error if a write fails.
    pub(super) async fn store_and_spend(
        &self,
        mission: &Mission,
        request: &super::subscriptions::SubscribeReq,
        role: Role,
        target: &InviteTarget,
    ) -> Result<super::subscriptions::MissionSubscription, MartiError> {
        let subscription = self
            .store_subscription(
                mission,
                &request.client_uid,
                request.username.as_deref(),
                role,
            )
            .await?;

        self.clear_invitations(mission, target).await?;

        Ok(subscription)
    }

    /// The role a standing invitation grants this caller on this mission.
    ///
    /// # Errors
    ///
    /// A system error if a read fails.
    pub async fn invited_role(
        &self,
        mission: &Mission,
        target: &InviteTarget,
    ) -> Result<Option<Role>, MartiError> {
        Ok(self
            .invitations(mission)
            .await?
            .into_iter()
            .find(|invitation| invitation.matches(target))
            .map(|invitation| invitation.role))
    }

    /// Withdraws the invitations a subscribe has just made redundant.
    ///
    /// Only the two that name a device: an invitation to a channel or a team
    /// stands for everyone else in it.
    ///
    /// # Errors
    ///
    /// A system error if the write fails.
    pub async fn clear_invitations(
        &self,
        mission: &Mission,
        target: &InviteTarget,
    ) -> Result<usize, MartiError> {
        let mut removed = 0;

        for invitation in self.invitations(mission).await? {
            let names_device = matches!(invitation.kind.as_str(), "clientUid" | "callsign");

            if names_device && invitation.matches(target) {
                removed += usize::from(
                    self.uninvite(mission, &invitation.kind, &invitation.invitee)
                        .await?,
                );
            }
        }

        Ok(removed)
    }

    /// The connected uids an invitation should be pushed at.
    ///
    /// Resolved at send time from the live registry, because four of the five
    /// vocabularies only exist there.
    pub fn invite_recipients(&self, kind: &str, invitee: &str) -> Vec<String> {
        if kind == "clientUid" {
            return vec![invitee.to_string()];
        }

        let Ok(live) = self.context.live() else {
            return Vec::new();
        };

        live.snapshot()
            .into_iter()
            .filter(|endpoint| match kind {
                "callsign" => endpoint.callsign.eq_ignore_ascii_case(invitee),
                "userName" => endpoint.username.eq_ignore_ascii_case(invitee),
                "team" => endpoint.team.eq_ignore_ascii_case(invitee),
                "group" => endpoint
                    .groups
                    .iter()
                    .any(|group| group.as_str().eq_ignore_ascii_case(invitee)),
                _ => false,
            })
            .map(|endpoint| endpoint.uid)
            .collect()
    }
}
