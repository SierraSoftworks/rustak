//! Who is watching a mission, what role they hold, and how that is proved.
//!
//! # Where a role comes from
//!
//! In order: a mission token, this installation's administrators, the caller's
//! own subscription, and finally the mission's default role. The third of those
//! is ours rather than TAK Server's — TAK insists on the token — and it is what
//! lets a client create a mission over an ordinary session and then manage it
//! without replaying the token it was handed. It grants nothing a token would
//! not: the subscription it reads was created for that account in the first
//! place.
//!
//! A mission that is password-protected or invite-only has **no** default role,
//! so a caller with none of the first three gets nothing at all rather than
//! read access — which is the whole point of those two flags.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rustak_core::identity::password;
use uuid::Uuid;

use crate::auth::{MissionTokens, TokenType};
use crate::db::repos::{MissionPatch, MissionSubscriptionRow, NewSubscription};
use crate::marti::MartiError;
use crate::prelude::*;

use super::model::Mission;
use super::roles::{MissionRole, Role};
use super::service::MissionService;

/// What a subscribe was asked for.
#[derive(Debug, Clone, Default)]
pub struct SubscribeReq {
    /// The device subscribing. Required; `topic` is accepted as a spelling of
    /// it by the route.
    pub client_uid: String,
    /// The account behind the device, when the request carried one.
    pub username: Option<String>,
    /// The mission password, when the caller presented one.
    pub password: Option<String>,
    /// The role a mission token in the request resolved to.
    pub token_role: Option<MissionRole>,
    /// The role a standing invitation grants. Filled by M4-02.
    pub invited_role: Option<Role>,
}

/// One device's subscription, as the service reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionSubscription {
    pub client_uid: String,
    pub username: Option<String>,
    pub role: Role,
    pub create_time: DateTime<Utc>,
    /// Present on a subscribe and on the singular read; never on the plural
    /// role listing.
    pub token: Option<String>,
}

impl MissionSubscription {
    /// Reads a stored row, with no token attached.
    fn from_row(row: MissionSubscriptionRow) -> Self {
        Self {
            client_uid: row.client_uid,
            username: row.username,
            role: Role::parse(&row.role).unwrap_or(Role::ReadonlySubscriber),
            create_time: row.created_at,
            token: None,
        }
    }
}

impl MissionService {
    /// The mission-token issuer, loaded from the sealed secret.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the secret cannot be read.
    pub async fn tokens(&self) -> Result<MissionTokens, MartiError> {
        Ok(MissionTokens::load(&self.context).await?)
    }

    /// Subscribes a device, minting the token it will replay.
    ///
    /// # Errors
    ///
    /// [`MartiError::Forbidden`] with TAK Server's own wording for a wrong or
    /// unexpected password, for a protected mission with no role to offer, and
    /// for an invite-only mission the caller was not invited to.
    pub async fn subscribe(
        &self,
        mission: &Mission,
        request: SubscribeReq,
    ) -> Result<MissionSubscription, MartiError> {
        // A standing invitation counts wherever a token would, so a device
        // invited while it was offline can subscribe when it comes back.
        let mut request = request;
        let target = self.apply_invitation(mission, &mut request).await?;
        let presented = request.password.as_deref().filter(|pw| !pw.is_empty());

        if mission.is_password_protected() {
            match presented {
                Some(password) => self.check_password(mission, password).await?,
                None if request.token_role.is_none() && request.invited_role.is_none() => {
                    return Err(MartiError::Forbidden("No token role provided.".to_string()));
                }
                None => {}
            }
        } else if presented.is_some() {
            // Parity: a password on a mission that has none is an error rather
            // than a no-op, and both real servers and CloudTAK treat it so.
            return Err(MartiError::Forbidden("No password provided.".to_string()));
        }

        if mission.invite_only && request.token_role.is_none() && request.invited_role.is_none() {
            return Err(MartiError::Forbidden(
                "this mission is invitation only".to_string(),
            ));
        }

        let role = request
            .token_role
            .map(|role| role.kind)
            .or(request.invited_role)
            .unwrap_or(mission.default_role);

        self.store_and_spend(mission, &request, role, &target).await
    }

    /// Subscribes the account that just created a mission, as its owner.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub(super) async fn subscribe_owner(
        &self,
        mission: &Mission,
        client_uid: &str,
        username: Option<&str>,
    ) -> Result<String, MartiError> {
        let subscription = self
            .store_subscription(mission, client_uid, username, Role::Owner)
            .await?;

        subscription.token.ok_or_else(|| {
            MartiError::Internal("an owner subscription was created without a token".to_string())
        })
    }

    /// Upserts a subscription and mints its token.
    pub(super) async fn store_subscription(
        &self,
        mission: &Mission,
        client_uid: &str,
        username: Option<&str>,
        role: Role,
    ) -> Result<MissionSubscription, MartiError> {
        let tokens = self.tokens().await?;
        let subscription_uid = Uuid::new_v4().to_string();
        let token = tokens.issue(
            &subscription_uid,
            TokenType::Subscription,
            &mission.name,
            mission.guid,
            None,
        )?;

        let row = self
            .db()
            .mission_subscriptions()
            .upsert(NewSubscription {
                mission_id: mission.id,
                subscription_uid,
                client_uid: client_uid.to_string(),
                username: username.map(str::to_string),
                role: role.as_str().to_string(),
                token_jti: tokens.verify(&token).ok().map(|claims| claims.jti),
            })
            .await?;

        Ok(MissionSubscription {
            token: Some(token),
            ..MissionSubscription::from_row(row)
        })
    }

    /// Unsubscribes a device, reporting whether it was subscribed.
    ///
    /// `disconnectOnly` is accepted and ignored: we have no separate "still
    /// subscribed but not receiving" state, and keeping a row that receives
    /// nothing would be a subscription that lies about itself.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn unsubscribe(
        &self,
        mission: &Mission,
        client_uid: &str,
    ) -> Result<bool, MartiError> {
        Ok(self
            .db()
            .mission_subscriptions()
            .delete(mission.id, client_uid.to_string())
            .await?)
    }

    /// One device's subscription, with a freshly minted token attached.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn subscription(
        &self,
        mission: &Mission,
        client_uid: &str,
    ) -> Result<Option<MissionSubscription>, MartiError> {
        let Some(row) = self
            .db()
            .mission_subscriptions()
            .by_client(mission.id, client_uid.to_string())
            .await?
        else {
            return Ok(None);
        };

        let token = self.tokens().await?.issue(
            &row.subscription_uid,
            TokenType::Subscription,
            &mission.name,
            mission.guid,
            None,
        )?;

        Ok(Some(MissionSubscription {
            token: Some(token),
            ..MissionSubscription::from_row(row)
        }))
    }

    /// Every subscription to a mission, without tokens.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn subscriptions(
        &self,
        mission: &Mission,
    ) -> Result<Vec<MissionSubscription>, MartiError> {
        Ok(self
            .db()
            .mission_subscriptions()
            .list(mission.id)
            .await?
            .into_iter()
            .map(MissionSubscription::from_row)
            .collect())
    }

    /// The client uids of the subscribers that are connected right now.
    ///
    /// This is who a `t-x-m-*` notice goes to; M4-02 renders and sends it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn subscribers_for(
        &self,
        mission: &Mission,
        except: Option<&str>,
    ) -> Result<Vec<String>, MartiError> {
        let subscribed = self.subscriptions(mission).await?;

        if !self.context.has_live() {
            return Ok(Vec::new());
        }

        let connected: Vec<String> = self
            .context
            .live()?
            .snapshot()
            .into_iter()
            .map(|endpoint| endpoint.uid)
            .collect();

        Ok(subscribed
            .into_iter()
            .map(|subscription| subscription.client_uid)
            .filter(|uid| Some(uid.as_str()) != except)
            .filter(|uid| connected.contains(uid))
            .collect())
    }

    /// Every subscription this installation holds, keyed by mission.
    ///
    /// One single-entry map per subscription rather than one map of lists,
    /// which is the shape TAK Server emits and the shape an administrative
    /// client reads.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a read fails.
    pub async fn all_subscriptions(
        &self,
        by_guid: bool,
    ) -> Result<Vec<BTreeMap<String, String>>, MartiError> {
        let rows = self.db().mission_subscriptions().all().await?;
        let mut found = Vec::with_capacity(rows.len());

        for row in rows {
            if let Some(mission) = self.db().missions().by_id(row.mission_id).await? {
                let key = match by_guid {
                    true => mission.guid.to_string(),
                    false => mission.name,
                };

                found.push(BTreeMap::from([(key, row.client_uid)]));
            }
        }

        Ok(found)
    }

    /// Sets the role of one device, or of every device an account holds.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_role(
        &self,
        mission: &Mission,
        client_uid: Option<&str>,
        username: Option<&str>,
        role: Role,
    ) -> Result<usize, MartiError> {
        let changed = self
            .db()
            .mission_subscriptions()
            .set_role(
                mission.id,
                client_uid.map(str::to_string),
                username.map(str::to_string),
                role.as_str().to_string(),
            )
            .await?;

        // Told to the affected devices only: a role change is nobody else's
        // business, and every one of them has to re-read what it may do.
        for uid in self
            .role_change_recipients(mission, client_uid, username)
            .await?
        {
            self.notify_role_changed(mission, None, role, &uid);
        }

        Ok(changed)
    }

    /// Mints an `ACCESS` token for a caller who knows the password.
    ///
    /// # Errors
    ///
    /// [`MartiError::Forbidden`] when the password does not match, and when the
    /// mission has none — a token that granted the default role to anyone who
    /// asked would be a hole rather than a convenience.
    pub async fn access_token(
        &self,
        mission: &Mission,
        presented: &str,
    ) -> Result<String, MartiError> {
        self.check_password(mission, presented).await?;

        Ok(self.tokens().await?.issue(
            &mission.guid.to_string(),
            TokenType::Access,
            &mission.name,
            mission.guid,
            None,
        )?)
    }

    /// Sets or clears a mission's password.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_password(
        &self,
        mission: &Mission,
        password: Option<&str>,
    ) -> Result<(), MartiError> {
        let hashed = match password.filter(|value| !value.is_empty()) {
            Some(value) => Some(super::service::hash_password(value).await?),
            None => None,
        };

        self.db()
            .missions()
            .update(
                mission.id,
                MissionPatch {
                    password_hash: Some(hashed),
                    ..MissionPatch::default()
                },
            )
            .await?;

        self.notify_broadcast(mission, crate::stream::ChangeKind::Metadata, None);

        Ok(())
    }

    /// Sets or clears a mission's expiry.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_expiration(
        &self,
        mission: &Mission,
        expiration: Option<i64>,
    ) -> Result<(), MartiError> {
        self.db()
            .missions()
            .update(
                mission.id,
                MissionPatch {
                    expiration: Some(expiration.filter(|seconds| *seconds > 0)),
                    ..MissionPatch::default()
                },
            )
            .await?;

        Ok(())
    }

    /// Verifies a presented password against the stored hash.
    async fn check_password(&self, mission: &Mission, presented: &str) -> Result<(), MartiError> {
        let Some(stored) = mission.password_hash.clone() else {
            return Err(MartiError::Forbidden("No password provided.".to_string()));
        };

        let hashed = password::PasswordHash::parse(stored).map_err(|_| {
            MartiError::Internal("a stored mission password is corrupt".to_string())
        })?;

        match password::verify_blocking(Secret::new(presented), hashed).await? {
            true => Ok(()),
            false => Err(MartiError::Forbidden("Password did not match.".to_string())),
        }
    }
}
