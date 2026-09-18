//! A mutable, in-memory stand-in for the server.
//!
//! Demo mode is far more useful if it behaves like the application rather than
//! like a screenshot: suspending a user, walking the wizard, or registering a
//! passkey all have to *stick*, or the pages that exist to do those things
//! cannot be reviewed at all. So the fixtures are loaded into a store which the
//! demo branches of [`crate::api`] read and write, and which lives as long as
//! the tab does.
//!
//! It is single-threaded because WebAssembly is, and it forgets everything on
//! reload, because a demo that accumulates state is a demo that stops being
//! reproducible.

use std::cell::RefCell;

use rustak_api::{
    ActiveGroup, AdminCreated, AuditCategory, AuditRecord, AuthMetadata, CaSummary,
    CreateAdminRequest, CreateCredentialRequest, CreateGroupRequest, CreateUserRequest, Credential,
    CredentialCreated, CredentialId, Device, DeviceUid, ENROLL_URL, EnrollTemplate, Group, GroupId,
    GroupMembership, GroupName, GroupPatch, GroupSource, Health, InitCaRequest, Me,
    MembershipSource, PasskeyChallenge, PasskeyId, PasskeySummary, ServerSettings,
    ServerSettingsRequest, SetupStatus, TokenResponse, User, UserId, UserKind, UserPatch,
    UserSource, Username,
};

use super::data;
use crate::api::ApiError;

/// How short a setup token demo mode will refuse, so the wizard's error path can
/// be seen without anybody having to know a magic value.
const MIN_SETUP_TOKEN: usize = 8;

struct State {
    signed_in: bool,
    setup: SetupStatus,
    settings: ServerSettings,
    ca: Option<CaSummary>,
    users: Vec<User>,
    passkeys: Vec<PasskeySummary>,
    audit: Vec<AuditRecord>,
    groups: Vec<Group>,
    memberships: Vec<(Username, Vec<GroupMembership>)>,
    devices: Vec<Device>,
    credentials: Vec<Credential>,
    /// What each device has switched on, which is a preference rather than a
    /// right — so it is keyed by device and not by account.
    active: Vec<(DeviceUid, Vec<ActiveGroup>)>,
    /// Distinguishes rows created during this session from the fixtures.
    next_id: i64,
}

impl State {
    fn new() -> Self {
        Self {
            signed_in: true,
            setup: data::setup_status(),
            settings: data::server_settings(),
            ca: Some(data::ca_summary()),
            users: data::users(),
            passkeys: data::passkeys(),
            audit: data::audit(),
            groups: data::groups(),
            memberships: data::all_memberships(),
            devices: data::devices(),
            credentials: data::credentials(),
            active: Vec::new(),
            next_id: 500,
        }
    }

    fn take_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
}

fn with<R>(action: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| action(&mut state.borrow_mut()))
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

pub fn demo_token() -> String {
    data::demo_token()
}

pub fn demo_credential() -> serde_json::Value {
    data::demo_credential()
}

pub fn auth_metadata() -> AuthMetadata {
    data::auth_metadata()
}

pub fn me() -> Option<Me> {
    with(|state| state.signed_in.then(data::me))
}

pub fn sign_out() {
    with(|state| state.signed_in = false);
}

pub fn sign_in_with_passkey() -> TokenResponse {
    with(|state| state.signed_in = true);
    data::token_response()
}

pub fn passkey_challenge() -> PasskeyChallenge {
    data::passkey_challenge()
}

/// Registers a passkey and signs the browser in, which is what the real
/// ceremony amounts to from the wizard's point of view.
pub fn register_passkey(label: Option<&str>) -> TokenResponse {
    with(|state| {
        let id = state.take_id();
        state.passkeys.push(PasskeySummary {
            id: PasskeyId::new(id),
            label: label.unwrap_or("This device").to_string(),
            created_at: data::ago(0),
            last_used_at: None,
        });
        state.signed_in = true;
    });
    data::token_response()
}

pub fn passkeys() -> Vec<PasskeySummary> {
    with(|state| state.passkeys.clone())
}

pub fn delete_passkey(id: i64) {
    with(|state| state.passkeys.retain(|passkey| passkey.id.get() != id));
}

// ---------------------------------------------------------------------------
// The wizard
// ---------------------------------------------------------------------------

pub fn setup_status() -> SetupStatus {
    with(|state| state.setup.clone())
}

/// Puts the demo installation back to how a freshly unpacked server looks, so
/// the wizard can be walked again without reloading the tab.
pub fn reset_setup() {
    with(|state| {
        state.setup = data::fresh_setup_status();
        state.ca = None;
        state.signed_in = false;
        state.settings = ServerSettings {
            name: String::new(),
            domains: Vec::new(),
            base_url: None,
            node_id: None,
            setup_completed_at: None,
        };
        state.users.retain(|user| user.kind == UserKind::Service);
        state.passkeys.clear();
    });
}

pub fn create_admin(request: &CreateAdminRequest) -> Result<AdminCreated, ApiError> {
    if request.setup_token.trim().len() < MIN_SETUP_TOKEN {
        return Err(ApiError::Server(
            "That setup token was not accepted. In demo mode any token of eight \
             characters or more will do."
                .to_string(),
        ));
    }

    with(|state| {
        if state.setup.setup_completed {
            return Err(ApiError::Gone);
        }

        let id = state.take_id();
        state.users.insert(
            0,
            User {
                id: UserId::new(id),
                username: request.username.clone(),
                kind: UserKind::Person,
                source: UserSource::Local,
                display_name: request.display_name.clone(),
                email: request.email.clone(),
                is_admin: true,
                admin_override: Some(true),
                disabled: false,
                created_at: data::ago(0),
                last_seen_at: None,
            },
        );
        state.setup.has_admin = true;

        Ok(AdminCreated {
            username: request.username.clone(),
            registration_token: "demo-mode-registration-token".to_string(),
            expires_in: 300,
        })
    })
}

pub fn set_server_settings(request: &ServerSettingsRequest) -> ServerSettings {
    with(|state| {
        state.settings.name = request.name.clone();
        state.settings.domains = request.domains.clone();
        state.settings.base_url = request.base_url.clone();
        state.settings.node_id = Some("0a9f4c21d8e34b7f".to_string());
        state.setup.has_server_name = true;
        state.settings.clone()
    })
}

/// The demo installation's authority, if the wizard has got that far.
pub fn ca() -> Option<CaSummary> {
    with(|state| state.ca.clone())
}

pub fn init_ca(request: &InitCaRequest) -> CaSummary {
    // Idempotent, like the server's: an authority that exists is adopted rather
    // than replaced by whatever the form happened to say.
    if let Some(existing) = ca() {
        return existing;
    }

    let summary = CaSummary {
        subject: match &request.organization {
            Some(organization) => format!("CN={}, O={organization}", request.common_name),
            None => format!("CN={}", request.common_name),
        },
        ..data::ca_summary()
    };

    with(|state| {
        state.ca = Some(summary.clone());
        state.setup.has_ca = true;
    });
    summary
}

pub fn complete_setup() {
    with(|state| {
        state.setup.setup_completed = true;
        state.setup.needs_setup = false;
        state.settings.setup_completed_at = Some(data::ago(0));
    });
}

// ---------------------------------------------------------------------------
// Administration
// ---------------------------------------------------------------------------

pub fn users() -> Vec<User> {
    with(|state| state.users.clone())
}

pub fn patch_user(username: &Username, patch: &UserPatch) -> Option<User> {
    with(|state| {
        let user = state
            .users
            .iter_mut()
            .find(|user| user.username.as_str() == username.as_str())?;

        if let Some(display_name) = &patch.display_name {
            user.display_name = Some(display_name.clone());
        }
        if let Some(is_admin) = patch.is_admin {
            user.is_admin = is_admin;
            user.admin_override = Some(is_admin);
        }
        if let Some(disabled) = patch.disabled {
            user.disabled = disabled;
        }
        Some(user.clone())
    })
}

pub fn audit(category: Option<AuditCategory>, limit: usize) -> Vec<AuditRecord> {
    with(|state| {
        state
            .audit
            .iter()
            .filter(|record| category.is_none_or(|wanted| record.category == wanted))
            .take(limit)
            .cloned()
            .collect()
    })
}

pub fn server_settings() -> ServerSettings {
    with(|state| state.settings.clone())
}

pub fn health() -> Health {
    data::health()
}

// ---------------------------------------------------------------------------
// Accounts, channels, devices and credentials
// ---------------------------------------------------------------------------

pub fn create_user(request: &CreateUserRequest) -> Result<User, ApiError> {
    with(|state| {
        if state
            .users
            .iter()
            .any(|user| user.username == request.username)
        {
            return Err(ApiError::Server(
                "There is already an account by that name.".to_string(),
            ));
        }

        let id = state.take_id();
        let user = User {
            id: UserId::new(id),
            username: request.username.clone(),
            kind: request.kind,
            source: match request.kind {
                UserKind::Service => UserSource::Service,
                UserKind::Person => UserSource::Local,
            },
            display_name: request.display_name.clone(),
            email: request.email.clone(),
            is_admin: false,
            admin_override: None,
            disabled: false,
            created_at: data::ago(0),
            last_seen_at: None,
        };

        state.users.push(user.clone());
        state
            .memberships
            .push((request.username.clone(), anon_only()));
        Ok(user)
    })
}

/// The grant every new account starts with while the installation's default
/// channel is switched on, which is what the server puts back for itself.
fn anon_only() -> Vec<GroupMembership> {
    vec![GroupMembership {
        group: GroupName::anon(),
        direction: rustak_api::Direction::Both,
        source: Some(MembershipSource::Manual),
    }]
}

pub fn groups() -> Vec<Group> {
    with(|state| state.groups.clone())
}

pub fn create_group(request: &CreateGroupRequest) -> Result<Group, ApiError> {
    with(|state| {
        if state.groups.iter().any(|group| group.name == request.name) {
            return Err(ApiError::Server(
                "There is already a channel by that name.".to_string(),
            ));
        }

        let id = state.take_id();
        // Bit positions are allocated and never reused, so the next one is one
        // past the highest ever handed out rather than the first free slot.
        let bitpos = state
            .groups
            .iter()
            .map(|group| group.bitpos + 1)
            .max()
            .unwrap_or(0);

        let group = Group {
            id: GroupId::new(id),
            name: request.name.clone(),
            bitpos,
            description: request.description.clone(),
            source: GroupSource::Manual,
        };
        state.groups.push(group.clone());
        Ok(group)
    })
}

pub fn patch_group(name: &GroupName, change: &GroupPatch) -> Result<Group, ApiError> {
    with(|state| {
        let group = state
            .groups
            .iter_mut()
            .find(|group| &group.name == name)
            .ok_or_else(|| ApiError::Server("There is no channel by that name.".to_string()))?;

        if let Some(description) = &change.description {
            group.description = (!description.is_empty()).then(|| description.clone());
        }
        Ok(group.clone())
    })
}

pub fn delete_group(name: &GroupName) -> Result<(), ApiError> {
    if name.is_anon() {
        return Err(ApiError::Server(
            "The default channel cannot be deleted.".to_string(),
        ));
    }

    with(|state| {
        state.groups.retain(|group| &group.name != name);
        for (_, held) in state.memberships.iter_mut() {
            held.retain(|grant| &grant.group != name);
        }
        Ok(())
    })
}

pub fn memberships_of(username: &Username) -> Vec<GroupMembership> {
    with(|state| {
        state
            .memberships
            .iter()
            .find(|(who, _)| who == username)
            .map(|(_, held)| held.clone())
            .unwrap_or_default()
    })
}

/// Replaces the manual grants, leaving the ones the identity provider owns —
/// exactly as `PUT /api/v1/users/{username}/groups` does, so a page cannot look
/// right here and lose somebody's channels against a real server.
pub fn set_memberships(
    username: &Username,
    wanted: &[GroupMembership],
) -> Result<Vec<GroupMembership>, ApiError> {
    with(|state| {
        let held = state
            .memberships
            .iter_mut()
            .find(|(who, _)| who == username)
            .map(|(_, held)| held)
            .ok_or_else(|| ApiError::Server("There is no account by that name.".to_string()))?;

        let mut next: Vec<GroupMembership> = held
            .iter()
            .filter(|grant| grant.source == Some(MembershipSource::Oidc))
            .cloned()
            .collect();

        for grant in wanted {
            if grant.source == Some(MembershipSource::Oidc) {
                return Err(ApiError::Server(format!(
                    "'{}' is granted by your identity provider, so setting it here would \
                     not last.",
                    grant.group
                )));
            }
            next.push(GroupMembership {
                source: Some(MembershipSource::Manual),
                ..grant.clone()
            });
        }

        *held = next.clone();
        Ok(next)
    })
}

pub fn devices(username: Option<&Username>) -> Vec<Device> {
    with(|state| {
        state
            .devices
            .iter()
            .filter(|device| username.is_none_or(|wanted| &device.username == wanted))
            .cloned()
            .collect()
    })
}

pub fn forget_device(uid: &DeviceUid) -> Result<(), ApiError> {
    with(|state| {
        state.devices.retain(|device| &device.uid != uid);
        state.active.retain(|(held, _)| held != uid);
        Ok(())
    })
}

pub fn set_active_groups(uid: &DeviceUid, wanted: &[ActiveGroup]) -> Vec<ActiveGroup> {
    with(|state| {
        let known: Vec<GroupName> = state
            .groups
            .iter()
            .map(|group| group.name.clone())
            .collect();
        // A channel deleted since the device cached it is dropped rather than
        // recreated, which is what the server does and what the response is for.
        let applied: Vec<ActiveGroup> = wanted
            .iter()
            .filter(|state| known.contains(&state.group))
            .cloned()
            .collect();

        match state.active.iter_mut().find(|(held, _)| held == uid) {
            Some((_, held)) => *held = applied.clone(),
            None => state.active.push((uid.clone(), applied.clone())),
        }
        applied
    })
}

pub fn credentials(username: Option<&Username>, include_revoked: bool) -> Vec<Credential> {
    let owner = username.cloned().unwrap_or_else(|| data::me().username);

    with(|state| {
        state
            .credentials
            .iter()
            .filter(|credential| credential.username.as_ref() == Some(&owner))
            .filter(|credential| include_revoked || !credential.is_revoked())
            .cloned()
            .collect()
    })
}

pub fn mint_credential(request: &CreateCredentialRequest) -> Result<CredentialCreated, ApiError> {
    let owner = request
        .username
        .clone()
        .unwrap_or_else(|| data::me().username);

    with(|state| {
        let id = state.take_id();
        // Not a secret and unmistakably not one: a demo build must not hand
        // anybody something that could be typed into a real client.
        let secret = format!("demo-mode-not-a-real-secret-{id}");

        let credential = Credential {
            id: CredentialId::new(id),
            kind: request.kind,
            label: request.label.clone(),
            username: Some(owner.clone()),
            created_at: data::ago(0),
            created_by: Some(data::me().username),
            expires_at: request
                .expires_in_days
                .map(|days| data::ago(-(i64::from(days) * 24 * 60)))
                .or_else(|| default_expiry(request.kind)),
            max_uses: request
                .max_uses
                .or_else(|| request.kind.is_single_use().then_some(1)),
            uses: 0,
            last_used_at: None,
            revoked_at: None,
        };

        state.credentials.push(credential.clone());

        Ok(CredentialCreated {
            enroll_url: matches!(request.kind, rustak_api::CredentialKind::EnrollmentToken).then(
                || {
                    ENROLL_URL
                        .replace("{host}", data::DEMO_HOST)
                        .replace("{username}", owner.as_str())
                        .replace("{token}", &secret)
                },
            ),
            credential,
            secret,
        })
    })
}

/// What the server would have chosen when the request said nothing.
fn default_expiry(kind: rustak_api::CredentialKind) -> Option<chrono::DateTime<chrono::Utc>> {
    match kind {
        rustak_api::CredentialKind::EnrollmentToken => Some(data::ago(-15)),
        rustak_api::CredentialKind::ClientPassword => Some(data::ago(-90 * 24 * 60)),
        rustak_api::CredentialKind::ServiceToken => None,
    }
}

pub fn revoke_credential(id: CredentialId) -> Result<(), ApiError> {
    with(|state| {
        let credential = state
            .credentials
            .iter_mut()
            .find(|credential| credential.id == id)
            .ok_or_else(|| ApiError::Server("There is no such credential.".to_string()))?;

        if credential.is_revoked() {
            return Err(ApiError::Server(
                "That credential has already gone.".to_string(),
            ));
        }

        credential.revoked_at = Some(data::ago(0));
        Ok(())
    })
}

pub fn enroll_template(id: CredentialId) -> Result<EnrollTemplate, ApiError> {
    with(|state| {
        let credential = state
            .credentials
            .iter()
            .find(|credential| credential.id == id)
            .ok_or_else(|| ApiError::Server("There is no such credential.".to_string()))?;

        let username = credential
            .username
            .clone()
            .unwrap_or_else(|| data::me().username);

        Ok(EnrollTemplate {
            credential: credential.id,
            host: data::DEMO_HOST.to_string(),
            url_template: ENROLL_URL
                .replace("{host}", data::DEMO_HOST)
                .replace("{username}", username.as_str()),
            username,
        })
    })
}
