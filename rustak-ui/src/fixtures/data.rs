//! The sample installation demo mode renders.
//!
//! It is written to look like a small but real deployment — a handful of people,
//! a suspended account, an audit trail with something concerning in it — because
//! a fixture set where everything is fine is a fixture set that cannot show what
//! the pages do when something is not.
//!
//! None of it is a captured payload from anywhere: every value here is ours.

use chrono::{DateTime, Duration, Utc};
use rustak_api::{
    AuditCategory, AuditOutcome, AuditRecord, AuthMetadata, AuthMode, AuthVia, CaSummary,
    ComponentStatus, Direction, GroupMembership, GroupName, Health, Me, MembershipSource,
    PasskeyChallenge, PasskeyId, PasskeySummary, ServerSettings, SetupStatus, TokenResponse, User,
    UserId, UserKind, UserSource, Username,
};

/// A timestamp relative to now, so the fixtures never look stale.
pub fn ago(minutes: i64) -> DateTime<Utc> {
    Utc::now() - Duration::minutes(minutes)
}

fn username(value: &str) -> Username {
    Username::from_storage(value)
}

/// The bearer token demo mode pretends to hold. It is not a token and could not
/// be mistaken for one — it names itself.
pub fn demo_token() -> String {
    "demo-mode-not-a-real-token".to_string()
}

/// A stand-in for what the browser hands back after a ceremony, for the demo
/// build where no authenticator is involved.
pub fn demo_credential() -> serde_json::Value {
    serde_json::json!({
        "id": "demo-credential",
        "rawId": "ZGVtby1jcmVkZW50aWFs",
        "type": "public-key",
        "response": { "clientDataJSON": "e30", "attestationObject": "oA" },
        "extensions": {},
    })
}

pub fn passkey_challenge() -> PasskeyChallenge {
    PasskeyChallenge {
        challenge_id: "demo-challenge".to_string(),
        options: serde_json::json!({ "publicKey": { "challenge": "ZGVtbw" } }),
    }
}

pub fn auth_metadata() -> AuthMetadata {
    AuthMetadata {
        mode: AuthMode::Oidc {
            authorization_endpoint: "https://id.example.com/authorize".to_string(),
            client_id: "rustak".to_string(),
            scopes: vec!["openid".into(), "profile".into(), "email".into()],
            pkce: true,
        },
        passkeys_enabled: true,
    }
}

pub fn token_response() -> TokenResponse {
    TokenResponse::new(demo_token(), "demo-mode-refresh-token", 3600)
}

fn memberships() -> Vec<GroupMembership> {
    vec![
        GroupMembership {
            group: GroupName::anon(),
            direction: Direction::Both,
            source: Some(MembershipSource::Manual),
        },
        GroupMembership {
            group: GroupName::from_storage("Blue Team"),
            direction: Direction::Both,
            source: Some(MembershipSource::Oidc),
        },
        GroupMembership {
            group: GroupName::from_storage("Command"),
            direction: Direction::Out,
            source: Some(MembershipSource::Manual),
        },
    ]
}

pub fn me() -> Me {
    Me {
        username: username("avery"),
        display_name: Some("Avery Quinn".to_string()),
        email: Some("avery@example.com".to_string()),
        kind: UserKind::Person,
        is_admin: true,
        via: AuthVia::Bearer,
        groups: memberships(),
    }
}

pub fn users() -> Vec<User> {
    vec![
        User {
            id: UserId::new(1),
            username: username("avery"),
            kind: UserKind::Person,
            source: UserSource::Local,
            display_name: Some("Avery Quinn".to_string()),
            email: Some("avery@example.com".to_string()),
            is_admin: true,
            admin_override: Some(true),
            disabled: false,
            created_at: ago(60 * 24 * 30),
            last_seen_at: Some(ago(3)),
        },
        User {
            id: UserId::new(2),
            username: username("bhavna"),
            kind: UserKind::Person,
            source: UserSource::Oidc,
            display_name: Some("Bhavna Rao".to_string()),
            email: Some("bhavna@example.com".to_string()),
            is_admin: false,
            admin_override: None,
            disabled: false,
            created_at: ago(60 * 24 * 12),
            last_seen_at: Some(ago(47)),
        },
        User {
            id: UserId::new(3),
            username: username("cormac"),
            kind: UserKind::Person,
            source: UserSource::Oidc,
            display_name: Some("Cormac Doyle".to_string()),
            email: Some("cormac@example.com".to_string()),
            is_admin: false,
            admin_override: None,
            disabled: true,
            created_at: ago(60 * 24 * 9),
            last_seen_at: Some(ago(60 * 26)),
        },
        User {
            id: UserId::new(4),
            username: username("service-weather"),
            kind: UserKind::Service,
            source: UserSource::Service,
            display_name: Some("Weather sidecar".to_string()),
            email: None,
            is_admin: false,
            admin_override: None,
            disabled: false,
            created_at: ago(60 * 24 * 4),
            last_seen_at: Some(ago(1)),
        },
    ]
}

pub fn ca_summary() -> CaSummary {
    CaSummary {
        subject: "CN=rustak Demo CA, O=Example".to_string(),
        fingerprint: "9f2c41a7e5b83d0c6148ab29fe7d3506c81b4af92e0d7c35619ab84fd2703e5c".to_string(),
        not_before: ago(60 * 24 * 30),
        not_after: ago(-60 * 24 * 365 * 10),
    }
}

pub fn passkeys() -> Vec<PasskeySummary> {
    vec![
        PasskeySummary {
            id: PasskeyId::new(1),
            label: "MacBook Touch ID".to_string(),
            created_at: ago(60 * 24 * 30),
            last_used_at: Some(ago(3)),
        },
        PasskeySummary {
            id: PasskeyId::new(2),
            label: "YubiKey (spare)".to_string(),
            created_at: ago(60 * 24 * 28),
            last_used_at: None,
        },
    ]
}

pub fn server_settings() -> ServerSettings {
    ServerSettings {
        name: "rustak demo".to_string(),
        domains: vec!["tak.example.com".to_string()],
        base_url: Some("https://tak.example.com:8446".to_string()),
        node_id: Some("0a9f4c21d8e34b7f".to_string()),
        setup_completed_at: Some(ago(60 * 24 * 30)),
    }
}

pub fn setup_status() -> SetupStatus {
    SetupStatus {
        needs_setup: false,
        has_admin: true,
        has_ca: true,
        has_server_name: true,
        setup_completed: true,
        version: Some("0.1.0-demo".to_string()),
    }
}

/// The wizard as a freshly installed server sees it.
pub fn fresh_setup_status() -> SetupStatus {
    SetupStatus {
        needs_setup: true,
        has_admin: false,
        has_ca: false,
        has_server_name: false,
        setup_completed: false,
        version: Some("0.1.0-demo".to_string()),
    }
}

pub fn health() -> Health {
    Health {
        status: ComponentStatus::Ok,
        version: "0.1.0-demo".to_string(),
        uptime_seconds: 96_240,
        database: ComponentStatus::Ok,
        message: None,
    }
}

/// One audit record, as a table entry rather than an eight-argument call.
struct Entry {
    id: i64,
    /// How long ago it happened, in minutes.
    minutes: i64,
    category: AuditCategory,
    action: &'static str,
    outcome: AuditOutcome,
    /// What it was done to.
    subject: Option<&'static str>,
    /// Who did it, when that is not the same thing.
    actor: Option<&'static str>,
    message: &'static str,
}

impl Entry {
    fn build(&self) -> AuditRecord {
        AuditRecord {
            id: self.id,
            occurred_at: ago(self.minutes),
            category: self.category,
            action: self.action.to_string(),
            outcome: self.outcome,
            subject: self.subject.map(str::to_string),
            actor: self.actor.map(str::to_string),
            message: Some(self.message.to_string()),
            detail: None,
        }
    }
}

/// The trail, newest first — with a refusal and a failure in it, because a log
/// where everything succeeded cannot show what the page does with one that did
/// not.
const ENTRIES: &[Entry] = &[
    Entry {
        id: 101,
        minutes: 3,
        category: AuditCategory::Authentication,
        action: "login",
        outcome: AuditOutcome::Success,
        subject: Some("avery"),
        actor: Some("avery"),
        message: "Signed in with a passkey.",
    },
    Entry {
        id: 100,
        minutes: 9,
        category: AuditCategory::Enrollment,
        action: "issue-token",
        outcome: AuditOutcome::Success,
        subject: Some("bhavna"),
        actor: Some("avery"),
        message: "Issued a one-time enrolment token, valid for 15 minutes.",
    },
    Entry {
        id: 99,
        minutes: 26,
        category: AuditCategory::Authentication,
        action: "login",
        outcome: AuditOutcome::Denied,
        subject: Some("cormac"),
        actor: None,
        message: "Refused by the admin access-control policy.",
    },
    Entry {
        id: 98,
        minutes: 47,
        category: AuditCategory::Stream,
        action: "connect",
        outcome: AuditOutcome::Success,
        subject: Some("bhavna (ETL)"),
        actor: Some("bhavna"),
        message: "Client negotiated protobuf and subscribed to 2 channels.",
    },
    Entry {
        id: 97,
        minutes: 61,
        category: AuditCategory::Pki,
        action: "sign-client",
        outcome: AuditOutcome::Success,
        subject: Some("ANDROID-2f1c9a7b4e0d"),
        actor: Some("avery"),
        message: "Issued a client certificate valid for 365 days.",
    },
    Entry {
        id: 96,
        minutes: 140,
        category: AuditCategory::Administration,
        action: "disable-user",
        outcome: AuditOutcome::Success,
        subject: Some("cormac"),
        actor: Some("avery"),
        message: "Account suspended pending a device audit.",
    },
    Entry {
        id: 95,
        minutes: 260,
        category: AuditCategory::System,
        action: "startup",
        outcome: AuditOutcome::Success,
        subject: None,
        actor: None,
        message: "Listening on :8446 (TLS, internal CA), :8443 and :8089.",
    },
    Entry {
        id: 94,
        minutes: 300,
        category: AuditCategory::Enrollment,
        action: "consume-token",
        outcome: AuditOutcome::Failure,
        subject: Some("unknown"),
        actor: None,
        message: "An enrolment token was presented after it had expired.",
    },
];

pub fn audit() -> Vec<AuditRecord> {
    ENTRIES.iter().map(Entry::build).collect()
}
