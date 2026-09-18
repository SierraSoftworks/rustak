//! One repository per aggregate.
//!
//! There is no ORM. Each file holds the row type for one table, the SQL that
//! reads and writes it, and the tests that hold both to the schema. A
//! repository is borrowed from a [`Database`] for the length of a call
//! (`db.users().get(id).await`), so nothing has to be threaded through a
//! constructor to reach one.
//!
//! Wire types are deliberately absent: turning a `UserRow` into a
//! `rustak_api::User` is the web layer's job, which keeps a repository from
//! acquiring opinions about what a response looks like.

pub mod certificates;
pub mod credentials;
pub mod devices;
pub mod groups;
pub mod members;
pub mod missions;
pub mod oauth_keys;
pub mod passkeys;
pub mod refresh_tokens;
pub mod resources;
pub mod revoked_jtis;
pub mod services;
pub mod settings;
pub mod stream_segments;
pub mod users;

use super::Database;

pub use certificates::list::CertificateFilter;
pub use certificates::{CertificateRow, CertificatesRepo, NewCertificate, RevocationDetails};
pub use credentials::{CredentialRow, CredentialsRepo, NewCredential};
pub use devices::{DeviceRow, DeviceSeen, DevicesRepo, NewDevice};
pub use groups::{GroupRow, GroupsRepo, NewGroup};
pub use members::{ActiveChannel, MembersRepo, Membership};
pub use missions::changes::{MissionChangeRow, MissionChangesRepo, NewChange};
pub use missions::contents::{MissionContentRow, MissionContentsRepo, MissionUidRow};
pub use missions::subscriptions::{
    MissionSubscriptionRow, MissionSubscriptionsRepo, NewSubscription,
};
pub use missions::{MissionFilter, MissionPatch, MissionRow, MissionsRepo, NewMission};
pub use oauth_keys::{KeyAlgorithm, KeyPurpose, NewOauthKey, OauthKeyRow, OauthKeysRepo};
pub use passkeys::{NewPasskey, PasskeyRow, PasskeysRepo};
pub use refresh_tokens::{Exchange, NewRefreshToken, RefreshTokenRow, RefreshTokensRepo};
pub use resources::{MutableField, NewResource, ResourceFilter, ResourceRow, ResourcesRepo};
pub use revoked_jtis::RevokedJtisRepo;
pub use services::{NewService, ServiceRow, ServicesRepo};
pub use settings::{SettingRow, SettingsRepo};
pub use stream_segments::{NewStreamSegment, StreamSegmentRow, StreamSegmentsRepo};
pub use users::profile::ProfileChange;
pub use users::{NewUser, OidcProfile, UserRow, UsersRepo};

/// A window over a listing.
///
/// Offset paging rather than a cursor: these listings are administrative, they
/// are ordered by a stable key, and none of them is large enough for the scan
/// cost to matter. The log, which *is* unbounded, pages by id instead — see
/// [`crate::db::AuditQuery`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    /// How many rows to return.
    pub limit: u32,
    /// How many rows to skip.
    pub offset: u32,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            limit: 100,
            offset: 0,
        }
    }
}

impl Page {
    /// The first `limit` rows.
    pub fn first(limit: u32) -> Self {
        Self { limit, offset: 0 }
    }

    /// A window starting at `offset`.
    pub fn at(offset: u32, limit: u32) -> Self {
        Self { limit, offset }
    }

    /// `LIMIT` as SQLite wants it bound.
    pub fn limit(self) -> i64 {
        i64::from(self.limit)
    }

    /// `OFFSET` as SQLite wants it bound.
    pub fn offset(self) -> i64 {
        i64::from(self.offset)
    }
}

impl Database {
    /// People and services who may connect.
    pub fn users(&self) -> UsersRepo<'_> {
        UsersRepo::new(self)
    }

    /// Channels.
    pub fn groups(&self) -> GroupsRepo<'_> {
        GroupsRepo::new(self)
    }

    /// Who is in which channel, and which of them a device has switched on.
    pub fn members(&self) -> MembersRepo<'_> {
        MembersRepo::new(self)
    }

    /// Enrolled clients.
    pub fn devices(&self) -> DevicesRepo<'_> {
        DevicesRepo::new(self)
    }

    /// Hashed secrets: enrolment tokens, client passwords, service tokens.
    pub fn credentials(&self) -> CredentialsRepo<'_> {
        CredentialsRepo::new(self)
    }

    /// WebAuthn credentials.
    pub fn passkeys(&self) -> PasskeysRepo<'_> {
        PasskeysRepo::new(self)
    }

    /// Issued certificates and their revocation state.
    pub fn certificates(&self) -> CertificatesRepo<'_> {
        CertificatesRepo::new(self)
    }

    /// Registered sidecars.
    pub fn services(&self) -> ServicesRepo<'_> {
        ServicesRepo::new(self)
    }

    /// Our token-signing keys.
    pub fn oauth_keys(&self) -> OauthKeysRepo<'_> {
        OauthKeysRepo::new(self)
    }

    /// Rotating refresh tokens.
    pub fn refresh_tokens(&self) -> RefreshTokensRepo<'_> {
        RefreshTokensRepo::new(self)
    }

    /// Access tokens disowned before their expiry.
    pub fn revoked_jtis(&self) -> RevokedJtisRepo<'_> {
        RevokedJtisRepo::new(self)
    }

    /// Settings the wizard and the admin UI own.
    pub fn settings(&self) -> SettingsRepo<'_> {
        SettingsRepo::new(self)
    }

    /// Enterprise Sync metadata: data packages, attachments and their keywords.
    pub fn resources(&self) -> ResourcesRepo<'_> {
        ResourcesRepo::new(self)
    }

    /// The index over the append-only stream segments.
    pub fn stream_segments(&self) -> StreamSegmentsRepo<'_> {
        StreamSegmentsRepo::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_binds_the_way_sqlite_wants_it() {
        assert_eq!(Page::default(), Page::first(100));
        assert_eq!(Page::at(20, 10).offset(), 20);
        assert_eq!(Page::at(20, 10).limit(), 10);
    }
}
