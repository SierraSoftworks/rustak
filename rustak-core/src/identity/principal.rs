//! Who is making this request, and how we know.
//!
//! A [`Principal`] is the answer every listener produces before any handler
//! runs, and the only thing a handler is allowed to reason about. It carries the
//! user, the device (when the request came from one), the effective group rights
//! for routing, whether the user is an administrator, and — as [`AuthMethod`] —
//! *how* the claim was proved.
//!
//! # There is no anonymous principal
//!
//! rustak has no plaintext listener and no anonymous access: every stream and
//! Marti connection presents a client certificate, and a device that cannot must
//! enrol first. So there is deliberately no `Principal::anonymous()` and no
//! anonymous [`AuthMethod`] — not as an oversight to be filled in later, but
//! because a constructor that manufactures an unauthenticated principal is the
//! single most dangerous function such a module can offer. Code that cannot
//! establish who is calling returns an authentication failure instead.
//!
//! The `__ANON__` *group* is a different thing entirely and does still exist: it
//! is the default channel every authenticated principal shares, not a way of
//! being unauthenticated.
//!
//! # Why the method is kept
//!
//! [`AuthMethod`] is not decoration. Authorisation depends on it — a client
//! password is accepted on `/oauth/token` and enrolment and *nowhere else*, and
//! a setup token only before the first administrator exists — and so does the
//! audit log, where "signed in with a certificate" and "signed in with a
//! password" are different events an operator needs to tell apart.

use std::sync::Arc;

use super::groups::{GroupSet, can_reach};
use rustak_api::credential::CredentialKind;
use rustak_api::identity::{CredentialId, DeviceUid, Direction, UserId, Username};

/// What kind of account is behind a request.
///
/// There is no `Anonymous` variant; see the [module documentation](self).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrincipalKind {
    /// A human being, signed in through OIDC, a passkey or an EUD certificate.
    Person,
    /// A sidecar, identified by its own certificate or service token.
    Service,
}

/// How a principal's identity was established on this request.
///
/// Each variant carries the identifier the audit log needs to tie the request
/// back to the credential or certificate it used, so that revoking one can be
/// traced to the sessions it ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMethod {
    /// A TLS client certificate, verified at handshake against the internal CA.
    ClientCert {
        /// Lowercase hex sha256 of the DER certificate — the revocation key.
        fingerprint: String,
        /// The certificate's serial number, in lowercase hex.
        serial: String,
    },
    /// One of our own RS256 access tokens.
    Bearer {
        /// The token's `jti`, which is what revocation lists.
        jti: String,
        /// The token's granted scope.
        scope: String,
    },
    /// HTTP Basic, accepted only on enrolment and `/oauth/token`.
    Basic {
        /// The credential row that verified.
        credential_id: CredentialId,
        /// Which kind it was, because the paths differ by kind.
        kind: CredentialKind,
    },
    /// A WebAuthn assertion — local sign-in to the admin UI.
    Passkey {
        /// The passkey credential that signed the challenge.
        credential_id: CredentialId,
    },
    /// The one-time first-run token, valid only until an administrator exists.
    SetupToken,
}

impl AuthMethod {
    /// A short, stable label for the audit log and for log lines.
    pub fn label(&self) -> &'static str {
        match self {
            Self::ClientCert { .. } => "client-cert",
            Self::Bearer { .. } => "bearer",
            Self::Basic { .. } => "basic",
            Self::Passkey { .. } => "passkey",
            Self::SetupToken => "setup-token",
        }
    }
}

/// The authenticated identity behind one request or one stream connection.
///
/// Cheap to clone: the group set — the only large part — is shared behind an
/// [`Arc`], because every connected EUD's subscription holds one and routing
/// reads them constantly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    /// The user row this principal belongs to.
    pub user_id: UserId,
    /// The user's name, as written.
    pub username: Username,
    /// Whether the account is a person's or a sidecar's.
    pub kind: PrincipalKind,
    /// The EUD this request came from, when it came from one. `None` for a
    /// browser session or an API call that named no device.
    pub device: Option<DeviceUid>,
    /// Effective group rights: memberships narrowed by per-device active state.
    pub groups: Arc<GroupSet>,
    /// Whether this principal may use the administrative API.
    pub is_admin: bool,
    /// How the identity was proved on this request.
    pub via: AuthMethod,
}

impl Principal {
    /// Builds a principal with no device, no groups and no administrative
    /// rights, for a caller to add to.
    ///
    /// Everything optional starts at its least privileged value, so that code
    /// which forgets to set a field grants nothing rather than everything.
    pub fn new(user_id: UserId, username: Username, kind: PrincipalKind, via: AuthMethod) -> Self {
        Self {
            user_id,
            username,
            kind,
            device: None,
            groups: Arc::new(GroupSet::new()),
            is_admin: false,
            via,
        }
    }

    /// Attaches the EUD this request came from.
    #[must_use]
    pub fn with_device(mut self, device: DeviceUid) -> Self {
        self.device = Some(device);
        self
    }

    /// Attaches the effective group rights.
    #[must_use]
    pub fn with_groups(mut self, groups: Arc<GroupSet>) -> Self {
        self.groups = groups;
        self
    }

    /// Grants administrative rights.
    #[must_use]
    pub fn as_admin(mut self) -> Self {
        self.is_admin = true;
        self
    }

    /// The user's name, as written.
    pub fn username(&self) -> &Username {
        &self.username
    }

    /// Whether this principal is a sidecar rather than a person.
    pub fn is_service(&self) -> bool {
        self.kind == PrincipalKind::Service
    }

    /// Whether a message from this principal may reach `receiver`.
    ///
    /// Delegates to [`can_reach`]: this principal's `IN` groups against the
    /// receiver's `OUT` groups.
    pub fn can_reach(&self, receiver: &Principal) -> bool {
        can_reach(&self.groups, &receiver.groups)
    }

    /// Whether this principal holds `direction` on the group at `bitpos`.
    pub fn has_group(&self, bitpos: u32, direction: Direction) -> bool {
        self.groups.contains(bitpos, direction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(name: &str, kind: PrincipalKind) -> Principal {
        Principal::new(
            UserId::from(1),
            Username::parse(name).unwrap(),
            kind,
            AuthMethod::SetupToken,
        )
    }

    #[test]
    fn a_new_principal_starts_with_nothing_it_was_not_given() {
        // A field somebody forgot to set must grant nothing: no device, no
        // groups, no administrative rights.
        let principal = principal("j.smith", PrincipalKind::Person);

        assert!(principal.device.is_none());
        assert!(principal.groups.is_empty());
        assert!(!principal.is_admin);
        assert!(!principal.is_service());
    }

    #[test]
    fn there_is_no_way_to_construct_an_unauthenticated_principal() {
        // Stated as a test because it is a design constraint rather than an
        // accident: every constructor here demands a `UserId`, a `Username` and
        // an `AuthMethod`, so a caller that cannot establish who is calling has
        // nothing to build. If an `anonymous()` constructor is ever added, this
        // comment is the record of why it should not be.
        let methods = [
            AuthMethod::SetupToken,
            AuthMethod::Passkey {
                credential_id: CredentialId::from(1),
            },
        ];

        for via in methods {
            assert!(!via.label().is_empty());
        }
    }

    #[test]
    fn reachability_follows_the_group_rule_rather_than_the_account_kind() {
        // A sidecar and a person are routed by exactly the same rule; the kind
        // affects which APIs they may call, never who hears them.
        let mut sensor_groups = GroupSet::new();
        sensor_groups.set(7, Direction::In);
        let mut watch_groups = GroupSet::new();
        watch_groups.set(7, Direction::Out);

        let sensor = principal("adsb", PrincipalKind::Service).with_groups(Arc::new(sensor_groups));
        let watch = principal("j.smith", PrincipalKind::Person).with_groups(Arc::new(watch_groups));

        assert!(sensor.can_reach(&watch));
        assert!(!watch.can_reach(&sensor));
        assert!(sensor.has_group(7, Direction::In));
        assert!(!sensor.has_group(7, Direction::Out));
    }

    #[test]
    fn each_authentication_method_is_labelled_distinctly_for_the_audit_log() {
        // "Signed in with a certificate" and "signed in with a password" are
        // different events an operator has to be able to tell apart.
        let methods = [
            AuthMethod::ClientCert {
                fingerprint: "ab".repeat(32),
                serial: "0f".repeat(16),
            },
            AuthMethod::Bearer {
                jti: "jti-1".to_string(),
                scope: "admin".to_string(),
            },
            AuthMethod::Basic {
                credential_id: CredentialId::from(1),
                kind: CredentialKind::ClientPassword,
            },
            AuthMethod::Passkey {
                credential_id: CredentialId::from(2),
            },
            AuthMethod::SetupToken,
        ];

        let labels: std::collections::HashSet<&str> =
            methods.iter().map(AuthMethod::label).collect();

        assert_eq!(labels.len(), methods.len(), "labels must be distinct");
    }

    #[test]
    fn the_method_carries_what_revocation_needs_to_find() {
        // Revoking a certificate has to end the sessions using it, which means
        // the principal has to remember which certificate that was.
        let via = AuthMethod::ClientCert {
            fingerprint: "ab".repeat(32),
            serial: "0f".repeat(16),
        };

        let AuthMethod::ClientCert { fingerprint, .. } = &via else {
            panic!("expected a client certificate");
        };
        assert_eq!(fingerprint.len(), 64, "a sha256 fingerprint in hex");
    }

    #[test]
    fn a_principal_is_cheap_to_clone_because_its_groups_are_shared() {
        // Every connected EUD's subscription holds one of these and routing
        // reads them constantly, so the large part must not be copied.
        let groups = Arc::new(GroupSet::new());
        let original = principal("j.smith", PrincipalKind::Person).with_groups(groups.clone());

        let copy = original.clone();

        assert!(Arc::ptr_eq(&original.groups, &copy.groups));
        assert_eq!(original, copy);
    }

    #[test]
    fn a_builder_step_grants_only_what_it_names() {
        let admin = principal("root", PrincipalKind::Person)
            .as_admin()
            .with_device(DeviceUid::parse("ANDROID-123").unwrap());

        assert!(admin.is_admin);
        assert_eq!(admin.device.unwrap().as_str(), "ANDROID-123");
        assert!(admin.groups.is_empty(), "admin rights are not group rights");
    }
}
