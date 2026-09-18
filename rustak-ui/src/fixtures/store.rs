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
    AdminCreated, AuditCategory, AuditRecord, AuthMetadata, CaSummary, CreateAdminRequest, Health,
    InitCaRequest, Me, PasskeyChallenge, PasskeyId, PasskeySummary, ServerSettings,
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
