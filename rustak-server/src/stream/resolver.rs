//! From a verified client certificate to a [`Principal`].
//!
//! The handshake has already proved the certificate chains to this
//! installation's authority and has not been revoked; what is left is to decide
//! *who that is* and *what they may reach*. The answer comes from three rows:
//! the `certificates` row the fingerprint names, the `users` row it belongs to,
//! and the channel memberships narrowed by whatever the device has switched off
//! (`identity::members::effective_for_device`).
//!
//! # The fingerprint decides, not the common name
//!
//! A certificate's CN is the account it was issued to, and reading it is how a
//! log line stays legible. But the *authority* for who a connection is comes
//! from the `certificates` row, because that is the row revocation, device
//! binding and enrolment all write. A certificate whose CN and row disagree is
//! refused rather than resolved either way round — there is no benign reason
//! for them to differ, and picking one would be picking which of two
//! contradictory facts to trust.
//!
//! # A selection is read at registration, not only at the endpoint
//!
//! Which channels are switched on is a preference, held per device
//! (`device_group_state`) with the account's own selection
//! (`user_group_state`) as the default a device inherits. Both are read *here*,
//! as the connection is registered, rather than only where
//! `/Marti/api/groups/*` answers — otherwise a device enrolled after an
//! account-level change would route on every channel it is entitled to until it
//! called the endpoint itself, which is the permissive direction and the one
//! that matters.
//!
//! # A connection with no channels is still a connection
//!
//! An account with no memberships can reach nobody and be reached by nobody,
//! which looks exactly like a routing bug from the client's side. It is
//! accepted and logged rather than refused: refusing would leave an operator
//! debugging a TLS failure when the problem is a membership list.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;

use crate::db::Database;
use crate::db::repos::DeviceSeen;
use crate::identity::{devices, members, users};
use crate::pki::tls::PeerCertificate;
use crate::prelude::*;

/// Everything the stream needs to know about who is on a connection.
#[derive(Clone, Debug)]
pub struct StreamPrincipal {
    /// Who they are and what they may reach.
    pub principal: Arc<Principal>,
    /// The certificate's sha256, so revoking it can close this connection.
    pub fingerprint: String,
    /// The device row, when the certificate named one.
    pub device_id: Option<DeviceId>,
    /// The channels they receive from, by name, for the contact listings.
    pub groups: Vec<GroupName>,
    /// Whether the device asked to be invisible last time it was here.
    pub incognito: bool,
}

/// Resolving a verified certificate to a principal.
#[async_trait]
pub trait CertPrincipalResolver: Send + Sync + std::fmt::Debug {
    /// Who is on this connection.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error naming what is wrong with the
    /// certificate — no matching row, an account that has been switched off —
    /// and a [`human_errors::Kind::System`] error when a read fails. Either way
    /// the connection is closed; neither is ever sent to the client, because a
    /// stream has no error channel.
    async fn resolve(
        &self,
        certificate: &PeerCertificate,
        peer: SocketAddr,
    ) -> Result<StreamPrincipal, Error>;
}

/// The resolver that reads the installation's own tables.
#[derive(Clone, Debug)]
pub struct DbPrincipalResolver {
    db: Database,
    anon_by_default: bool,
}

impl DbPrincipalResolver {
    /// Builds a resolver over a database.
    pub fn new(db: Database, anon_by_default: bool) -> Self {
        Self {
            db,
            anon_by_default,
        }
    }
}

#[async_trait]
impl CertPrincipalResolver for DbPrincipalResolver {
    #[instrument("stream.auth.resolve", skip_all, fields(fingerprint = %certificate.fingerprint), err(Display))]
    async fn resolve(
        &self,
        certificate: &PeerCertificate,
        peer: SocketAddr,
    ) -> Result<StreamPrincipal, Error> {
        let row = self
            .db
            .certificates()
            .get_by_fingerprint(&certificate.fingerprint)
            .await?
            .ok_or_else(|| refused("this server has no record of that certificate"))?;

        if row.revoked_at.is_some() {
            return Err(refused("that certificate has been revoked"));
        }

        let user = self.account(&row).await?;

        // The CN and the row must agree. They are written by the same act — the
        // issuance — so a disagreement means one of them has been tampered with
        // or the database has been restored over a live installation.
        if let Some(common_name) = &certificate.common_name
            && !user.username.eq_ignore_case(common_name)
        {
            warn!(
                common_name,
                username = %user.username,
                "A client certificate's subject disagrees with the account it was issued to."
            );

            return Err(refused(
                "that certificate does not match the account it names",
            ));
        }

        let device = self.device(&row, user.id, peer).await?;
        let groups = match device.as_ref() {
            Some(device) => {
                members::effective_for_device(&self.db, user.id, device.id, self.anon_by_default)
                    .await?
            }
            None => members::effective_for_account(&self.db, user.id, self.anon_by_default).await?,
        };

        if groups.is_empty() {
            info!(
                username = %user.username,
                "A client connected with no channel memberships; it can neither see nor be seen."
            );
        }

        let names = self.group_names(&groups).await?;

        let mut principal = users::principal(
            &self.db,
            &user,
            AuthMethod::ClientCert {
                fingerprint: certificate.fingerprint.clone(),
                serial: certificate.serial_hex.clone(),
            },
            false,
        )
        .await?
        .with_groups(Arc::new(groups));

        if let Some(device) = &device {
            principal = principal.with_device(device.uid.clone());
        }

        // Best effort: a certificate whose last-seen stamp did not update is
        // not a reason to refuse a connection that is otherwise good.
        if let Err(err) = self.db.certificates().touch_last_seen(row.id).await {
            debug!(error = %err, "Could not record a certificate's use.");
        }

        Ok(StreamPrincipal {
            principal: Arc::new(principal),
            fingerprint: certificate.fingerprint.clone(),
            device_id: device.as_ref().map(|device| device.id),
            groups: names,
            incognito: device.is_some_and(|device| device.incognito),
        })
    }
}

impl DbPrincipalResolver {
    /// The account a certificate belongs to.
    async fn account(
        &self,
        row: &crate::db::repos::CertificateRow,
    ) -> Result<crate::db::repos::UserRow, Error> {
        let user = match row.user_id {
            Some(user_id) => self.db.users().get(user_id).await?,
            None => {
                let username = Username::parse(&row.subject_cn)
                    .map_err(|_| refused("that certificate names no account we recognise"))?;

                self.db.users().get_by_username(&username).await?
            }
        };

        let user =
            user.ok_or_else(|| refused("the account that certificate belongs to is gone"))?;

        if user.disabled {
            return Err(refused("that account has been switched off"));
        }

        Ok(user)
    }

    /// The device row a certificate was enrolled for, recording the connection.
    async fn device(
        &self,
        row: &crate::db::repos::CertificateRow,
        user_id: UserId,
        peer: SocketAddr,
    ) -> Result<Option<crate::db::repos::DeviceRow>, Error> {
        let Some(client_uid) = &row.client_uid else {
            return Ok(None);
        };

        let Ok(uid) = DeviceUid::parse(client_uid) else {
            debug!(
                client_uid,
                "A certificate names a device uid we cannot use."
            );

            return Ok(None);
        };

        let seen = DeviceSeen {
            last_ip: Some(peer.ip()),
            ..DeviceSeen::default()
        };

        Ok(Some(
            devices::upsert_seen(&self.db, &uid, user_id, seen).await?,
        ))
    }

    /// The names of the channels a principal receives from.
    async fn group_names(&self, groups: &GroupSet) -> Result<Vec<GroupName>, Error> {
        let index = self.db.groups().index().await?;

        Ok(groups.names(&index, Direction::Out))
    }
}

/// The one refusal shape, so a client learns nothing it did not already know.
///
/// A stream has no error channel — the socket simply closes — so this text
/// reaches the operator's log and never the device. It is still written as
/// something a person can act on, because that log is where somebody debugging
/// a device that will not connect is looking.
fn refused(why: &str) -> Error {
    human_errors::user(
        format!("A stream connection was refused because {why}."),
        &[
            "Check that the device has enrolled against this server and has not been revoked.",
            "Re-enrol the device if its certificate was issued by a previous installation.",
        ],
    )
}
