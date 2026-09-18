//! Validated identity newtypes shared by the server, the admin UI and the
//! client library.
//!
//! These live in this crate rather than in `rustak-core` so that the browser
//! and the server agree on one definition of a username without the UI having
//! to compile the server's runtime. `rustak-core::identity` re-exports
//! everything here, so server-side code sees a single `identity` module.
//!
//! Each type validates on the way in, and each offers a `from_storage`
//! constructor that normalises without validating, so tightening a rule cannot
//! make an existing row unloadable.

pub(crate) mod newtype;

pub mod group;
pub mod ids;
pub mod uid;
pub mod username;

pub use group::{Direction, GroupName, GroupNameError};
pub use ids::{
    CertificateId, CredentialId, DeviceId, GroupId, MissionId, PasskeyId, ProfileId, ResourceId,
    ServiceId, UserId,
};
pub use uid::{DeviceUid, DeviceUidError, MissionGuid, ServiceName, ServiceNameError};
pub use username::{Username, UsernameError};
