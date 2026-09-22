//! The JSON contract between `rustak-server` and the people and programs that
//! talk to it, and the validated identity newtypes that both ends share.
//!
//! Two jobs, and they belong together:
//!
//! - **The admin API's data-transfer types.** Every `/api/v1` request and
//!   response body is defined here once, so the Yew admin UI and the server
//!   cannot drift apart about what a field is called or what it may hold.
//! - **Identity newtypes.** [`Username`], [`DeviceUid`], [`GroupName`] and the
//!   typed row identifiers validate on the way in, so a value that reached a
//!   handler has already been checked — and there is exactly one definition of
//!   what a username may contain, rather than one per crate that handles them.
//!
//! This crate is deliberately free of any web framework, database or runtime
//! dependency: only `serde`, `serde_json`, `chrono` and `uuid`. That is what
//! lets it compile for `wasm32-unknown-unknown` alongside the browser UI as
//! well as link into the server. No tokio, no tracing, no rusqlite.
//!
//! # What is not here
//!
//! Secrets. A credential's secret exists in exactly one response
//! ([`CredentialCreated`]) and nowhere else in this crate — no hashes, no
//! hints, no lengths, no key material. Several of the types below carry a test
//! asserting their field list, so adding one that could hold a secret breaks a
//! test named after the reason not to. The types that do carry a secret redact
//! it in their `Debug` rendering, so that logging a response cannot leak a
//! session.
//!
//! The TAK-facing wire formats are not here either. The Marti API's XML and its
//! JSON envelopes are TAK's shapes rather than ours, and they live in the
//! server beside the routes that have to emit them byte for byte.

pub mod audit;
pub mod auth;
pub mod certificate;
pub mod client;
pub mod cloudtak;
pub mod config_package;
pub mod cot;
pub mod credential;
pub mod device;
pub mod error;
pub mod event;
pub mod group;
pub mod health;
pub mod identity;
pub mod map;
pub mod mission;
pub mod package;
pub mod passkey;
pub mod profile;
pub mod service;
pub mod settings;
pub mod setup;
pub mod user;

pub use audit::{AuditCategory, AuditOutcome, AuditRecord};
pub use auth::{
    AuthMetadata, AuthMode, AuthVia, Me, TokenExchangeRequest, TokenRefreshRequest, TokenResponse,
};
pub use certificate::{
    Certificate, CertificateKind, CertificateSource, CertificateState, RevocationReason,
    RevokeCertificateRequest,
};
pub use client::{ClientHistoryEntry, ConnectedClient, IncognitoRequest, StreamStatus};
pub use cloudtak::{
    CloudTakOnboarding, CloudTakOnboardingRequest, CloudTakPorts, CloudTakUrls,
    OnboardingCredential,
};
pub use config_package::{ConfigPackageRequest, ConfigPackageVariant};
pub use cot::{CotDetail, CotSummary};
pub use credential::{
    CreateCredentialRequest, Credential, CredentialCreated, CredentialKind, ENROLL_URL,
    EnrollTemplate,
};
pub use device::Device;
pub use error::ApiErrorBody;
pub use event::{
    ChannelEvent, ClientEvent, MissionEvent, PackageEvent, ServerEvent, ServerEventPayload,
    ServiceEvent,
};
pub use group::{
    ActiveGroup, CreateGroupRequest, Group, GroupMember, GroupMembership, GroupPatch, GroupSource,
    MembershipSource,
};
pub use health::{ComponentStatus, Health};
pub use identity::{
    CertificateId, CredentialId, DeviceId, DeviceUid, DeviceUidError, Direction, GroupId,
    GroupName, GroupNameError, MissionGuid, MissionId, PasskeyId, ProfileId, ResourceId, ServiceId,
    ServiceName, ServiceNameError, UserId, Username, UsernameError,
};
pub use map::{MapFeature, MapPoint, MapShape, MapUpdate};
pub use mission::{
    MissionChangeKind, MissionChangeSummary, MissionDetail, MissionLayerSummary, MissionRoleKind,
    MissionRoleUpdate, MissionSubscriptionSummary, MissionSummary, UidDetails,
};
pub use package::{PackageSummary, PackageUpdate};
pub use passkey::{
    PasskeyChallenge, PasskeyLoginFinish, PasskeyLoginStart, PasskeyRegistrationFinish,
    PasskeyRegistrationStart, PasskeySummary,
};
pub use profile::{
    PrefCatalogEntry, PrefClass, PrefEntry, Profile, ProfileCreate, ProfileFile, ProfileUpdate,
};
pub use service::{
    Capability, CapabilityError, Heartbeat, ServiceDescriptor, ServiceEndpoints, ServiceState,
    ServiceStatus, ServiceSummary,
};
pub use settings::{
    FileSettings, MartiSettings, ServerSettings, TlsCertificateState, TlsSource, TlsStatus,
};
pub use setup::{
    AdminCreated, CaKeyType, CaSummary, CreateAdminRequest, InitCaRequest, ServerSettingsRequest,
    SetupStatus,
};
pub use user::{CreateUserRequest, User, UserKind, UserPatch, UserSource};
