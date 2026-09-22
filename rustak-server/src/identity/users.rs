//! Accounts: how a stored row becomes a principal, and how a sign-in creates
//! one.
//!
//! rustak has no local passwords, so an account is created in exactly two
//! ways: an identity provider vouched for somebody for the first time, or the
//! first-run wizard made the first administrator. Both end up here, because
//! both then need the same things — a default channel, the administrative flag
//! resolved, and a [`Principal`] the rest of the request can be answered from.

use std::sync::Arc;

use rustak_api::{AuthVia, Me, User, UserKind, UserSource};
use rustak_core::identity::{AuthMethod, Principal, PrincipalKind};
use rustak_core::prelude::*;

use crate::config::OidcConfig;
use crate::db::{
    Database,
    repos::{NewUser, OidcProfile, UserRow},
};
use crate::identity::{groups, members};
use crate::services::{AppContext, Services};

/// What an identity provider told us about somebody, once verified.
#[derive(Debug, Clone)]
pub struct VerifiedIdentity {
    /// The provider's issuer, which is half of the account's identity.
    pub issuer: String,
    /// The provider's subject identifier, which is the other half. This rather
    /// than the username, because a directory that lets somebody rename
    /// themselves must not strand their account or walk them into another one.
    pub subject: String,
    /// The name the account is stored under.
    pub username: Username,
    pub display_name: Option<String>,
    pub email: Option<String>,
    /// The raw `groups` claim, before any of it is interpreted.
    pub groups: Vec<String>,
    /// Every claim the access-control expressions may look at, kept on the
    /// account so that later requests are judged by what the provider last
    /// said (the sessions we issue carry none of them).
    pub claims: serde_json::Map<String, serde_json::Value>,
}

impl VerifiedIdentity {
    /// What of this identity the account stores.
    fn profile(&self, is_admin: bool) -> OidcProfile {
        OidcProfile {
            display_name: self.display_name.clone(),
            email: self.email.clone(),
            is_admin,
            claims: serde_json::to_string(&self.claims).ok(),
        }
    }
}

/// Creates or refreshes the account behind a verified identity.
///
/// `is_admin` is what the `admin_acl` expression decided about this sign-in; it
/// is stored so that the Marti and stream listeners, which never see the
/// provider's claims, can still tell who administers the installation.
///
/// Takes the context rather than the database because a sign-in is one of the
/// ways channels and memberships change, and both the routing cache and the
/// account's live connections have to be told — see
/// [`groups::apply_claims`] and [`members::channels_changed`].
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the account name is already held
/// by somebody this provider did not vouch for, and a
/// [`human_errors::Kind::System`] error if a read or write fails.
#[instrument("identity.users.provision", skip_all, fields(username = %identity.username), err(Display))]
pub async fn provision(
    context: &AppContext,
    oidc: &OidcConfig,
    identity: &VerifiedIdentity,
    is_admin: bool,
    anon_by_default: bool,
) -> Result<UserRow, Error> {
    let db = context.db();
    let profile = identity.profile(is_admin);
    let known = db
        .users()
        .get_by_oidc(&identity.issuer, &identity.subject)
        .await?
        .is_some();

    let (row, created) = if known {
        (upsert(db, identity, profile).await?, false)
    } else {
        // The subject is new to us, so the name is the question: whose is it?
        match db.users().get_by_username(&identity.username).await? {
            // Somebody's, and the operator said the provider may have it. An
            // upsert would insert a second row under the same name and trip the
            // unique index; the account is handed over instead.
            Some(existing) if oidc.link_by_username => {
                (adopt(db, &existing, identity, profile).await?, false)
            }
            Some(existing) => return Err(takeover_refused(&existing, identity)),
            None => (upsert(db, identity, profile).await?, true),
        }
    };

    if created {
        info!(username = %row.username, "Created an account from an identity provider sign-in.");

        if anon_by_default {
            groups::join_default(db, row.id).await?;
        }
    }

    claims_applied(context, oidc, &row, &identity.groups).await?;

    Ok(row)
}

/// Maps the `groups` claim onto memberships and makes the result take effect.
///
/// The mapping is [`groups::apply_claims`], which invalidates the routing cache
/// itself when a claim created a channel. What it cannot do is the *person*
/// half: a membership this sign-in added or took away has to reach whatever
/// that account already has connected, because a live stream connection holds
/// the rights it authenticated with and would otherwise keep them until it
/// reconnected. That is exactly what the channels API does after an
/// administrator's change, so it is the same call rather than a second one.
///
/// `None` as the originating device: a sign-in is not one connected device's
/// action, so every device the account has open is told.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a channel cannot be read or
/// created, or the write fails. Telling the connections never fails.
async fn claims_applied(
    context: &AppContext,
    oidc: &OidcConfig,
    row: &UserRow,
    claimed: &[String],
) -> Result<(), Error> {
    if groups::apply_claims(context, oidc, row.id, claimed).await? {
        members::channels_changed(context, row.id, &row.username, None).await;
    }

    Ok(())
}

/// Binds the caller's own account to the identity the provider just vouched
/// for — the self-service half of a migration from passkeys to single sign-on,
/// which needs no `link_by_username` because the person has proved they hold
/// both: they are signed in here, and they just signed in there.
///
/// From then on the account is the provider's: its username follows the
/// provider's, as does every other provider-backed account's. The passkeys,
/// credentials and channels it had are kept.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the account is a service's, is
/// already linked to a different identity, when that identity already signs in
/// as somebody else, or when the provider's name for the person is another
/// account here; a [`human_errors::Kind::System`] error if a read or write
/// fails.
#[instrument("identity.users.link", skip_all, fields(username = %current.username), err(Display))]
pub async fn link_identity(
    context: &AppContext,
    oidc: &OidcConfig,
    current: &UserRow,
    identity: &VerifiedIdentity,
    is_admin: bool,
) -> Result<UserRow, Error> {
    let db = context.db();

    if current.kind == UserKind::Service {
        return Err(human_errors::user(
            "A service account cannot be linked to an identity provider.",
            &["Sign in as a person to link that account."],
        ));
    }

    let profile = identity.profile(is_admin);
    let same_identity = current.oidc_issuer.as_deref() == Some(&identity.issuer)
        && current.oidc_subject.as_deref() == Some(&identity.subject);

    if same_identity {
        // Already linked to exactly this identity: refresh it, as a sign-in would.
        let row = upsert(db, identity, profile).await?;
        claims_applied(context, oidc, &row, &identity.groups).await?;
        return Ok(row);
    }

    if current.oidc_subject.is_some() {
        return Err(human_errors::user(
            "Your account is already linked to a different identity at your provider.",
            &["Ask an administrator to remove the existing link before making another."],
        ));
    }

    if let Some(other) = db
        .users()
        .get_by_oidc(&identity.issuer, &identity.subject)
        .await?
    {
        return Err(human_errors::user(
            format!(
                "That identity already signs in here as '{}'.",
                other.username
            ),
            &["Sign in as that account instead, or ask an administrator to remove it."],
        ));
    }

    if identity.username != current.username
        && db
            .users()
            .get_by_username(&identity.username)
            .await?
            .is_some()
    {
        return Err(human_errors::user(
            format!(
                "Your identity provider knows you as '{}', which is already another account here.",
                identity.username
            ),
            &["Ask an administrator to rename or remove that account, then try again."],
        ));
    }

    let row = adopt(db, current, identity, profile).await?;
    claims_applied(context, oidc, &row, &identity.groups).await?;

    Ok(row)
}

/// The ordinary write: insert a stranger, refresh somebody known by subject.
async fn upsert(
    db: &Database,
    identity: &VerifiedIdentity,
    profile: OidcProfile,
) -> Result<UserRow, Error> {
    db.users()
        .upsert_oidc(
            &identity.issuer,
            &identity.subject,
            &identity.username,
            profile,
        )
        .await
}

/// Hands an existing account to the provider, keeping what it had.
///
/// The administrative flag is about to become what `admin_acl` decides on every
/// sign-in. A standing granted *here* — by the wizard, or by an administrator —
/// must not silently end because the directory started vouching for the name,
/// so it is recorded as the explicit override that outranks the expression.
/// The Users page shows that as "set here", and an administrator can undo it.
async fn adopt(
    db: &Database,
    existing: &UserRow,
    identity: &VerifiedIdentity,
    profile: OidcProfile,
) -> Result<UserRow, Error> {
    if existing.is_admin && existing.admin_override.is_none() {
        db.users()
            .set_admin_override(existing.id, Some(true))
            .await?;
    }

    let row = db
        .users()
        .link_oidc(
            existing.id,
            &identity.issuer,
            &identity.subject,
            &identity.username,
            profile,
        )
        .await?;

    info!(username = %row.username, "Linked an existing account to an identity provider.");

    Ok(row)
}

/// Why a provider is not handed an account it did not create.
///
/// Turning `link_by_username` on means whoever controls the directory can take
/// over any account by claiming its name, which is a decision an operator makes
/// deliberately rather than something that happens by default.
fn takeover_refused(existing: &UserRow, identity: &VerifiedIdentity) -> Error {
    if existing.source == UserSource::Oidc
        && existing.oidc_issuer.as_deref() == Some(&identity.issuer)
    {
        // Same provider, different subject: the directory reissued the name.
        // That is a rename to sort out by hand, not a takeover.
        return human_errors::user(
            format!(
                "The account '{}' belongs to a different identity in your provider.",
                identity.username
            ),
            &[
                "Rename or remove the existing account, then sign in again.",
                "This usually means the account was recreated in your directory.",
            ],
        );
    }

    human_errors::user(
        format!(
            "The account '{}' already exists and was not created by your identity provider.",
            identity.username
        ),
        &[
            "Sign in with a passkey and link your single sign-on account from the Credentials page.",
            "Set 'link_by_username' under [auth.oidc] only if you intend your provider to take over existing accounts.",
        ],
    )
}

/// Builds the principal a request is answered under.
///
/// `acl_admin` is whatever `admin_acl` said about *this* request; it is ORed
/// with the stored decision, because an installation can grant administrative
/// access either way and taking the narrower of the two would silently ignore
/// whichever one the operator actually configured.
///
/// # The scope is a ceiling, not decoration
///
/// What the account *may* do and what the presented credential was *granted*
/// are two different questions, and the answer is the narrower of them. The
/// stored decision above answers the first; [`AuthMethod::Bearer`]'s `scope`
/// answers the second, and is ANDed in. Without that, a token deliberately
/// minted narrow — the password grant's, or one whose refresh family was capped
/// when its holder was an ordinary user — still carried everything its account
/// could do, and every "this scope is the ceiling" comment in the OAuth server
/// described a control that did not exist (R-01 H1).
///
/// Only a bearer token carries a scope. A client certificate, HTTP Basic, a
/// passkey assertion and the first-run token are not scoped grants, so they are
/// bounded by the account alone.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the channel read fails.
pub async fn principal(
    db: &Database,
    row: &UserRow,
    via: AuthMethod,
    acl_admin: bool,
) -> Result<Principal, Error> {
    let groups = db.members().group_set(row.id).await?;
    let kind = match row.kind {
        UserKind::Service => PrincipalKind::Service,
        UserKind::Person => PrincipalKind::Person,
    };

    let granted_admin = scope_grants_admin(&via);
    let mut principal =
        Principal::new(row.id, row.username.clone(), kind, via).with_groups(Arc::new(groups));

    // `admin_override` is an administrator's explicit decision and outranks
    // everything, including an ACL that would otherwise say yes.
    principal.is_admin = match row.admin_override {
        Some(decided) => decided,
        None => row.is_admin || acl_admin,
    } && !row.disabled
        && granted_admin;

    Ok(principal)
}

/// Whether the credential behind a request was granted administrative scope.
///
/// [`true`] for every credential that is not a scoped grant: the scope is a
/// ceiling over what the account may do, and a credential that carries none
/// lowers nothing.
fn scope_grants_admin(via: &AuthMethod) -> bool {
    match via {
        AuthMethod::Bearer { scope, .. } => crate::auth::tokens::grants_admin(scope),
        // A workload assertion enrols a sidecar and exchanges itself for a
        // service token; it is never an administrative grant, whatever the
        // account behind it is flagged as.
        AuthMethod::Workload { .. } => false,
        _ => true,
    }
}

/// What `GET /api/v1/me` answers.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the channel read fails.
pub async fn me(db: &Database, row: &UserRow, principal: &Principal) -> Result<Me, Error> {
    Ok(Me {
        username: row.username.clone(),
        display_name: row.display_name.clone(),
        email: row.email.clone(),
        kind: row.kind,
        is_admin: principal.is_admin,
        via: via_of(&principal.via),
        source: row.source,
        identity_provider: row.oidc_issuer.clone(),
        groups: members::grants_for_user(db, row.id).await?,
        preferences: db.user_preferences().get(row.id).await?,
    })
}

/// How a principal's proof of identity is described to the UI.
///
/// A passkey assertion and the first-run token both end as one of our own
/// bearer tokens before anything asks `/me`, so they are reported as what the
/// request actually carried rather than as how the session began.
pub fn via_of(method: &AuthMethod) -> AuthVia {
    match method {
        AuthMethod::ClientCert { .. } => AuthVia::ClientCert,
        AuthMethod::Basic { .. } => AuthVia::Basic,
        AuthMethod::Bearer { .. }
        | AuthMethod::Passkey { .. }
        | AuthMethod::SetupToken
        // The assertion arrives in an `Authorization` header like any other
        // token, and what the UI shows is what the request actually carried.
        | AuthMethod::Workload { .. } => AuthVia::Bearer,
    }
}

/// A stored row as the admin API renders it.
pub fn to_dto(row: &UserRow) -> User {
    User {
        id: row.id,
        username: row.username.clone(),
        kind: row.kind,
        source: row.source,
        display_name: row.display_name.clone(),
        email: row.email.clone(),
        is_admin: row.is_effective_admin(),
        admin_override: row.admin_override,
        disabled: row.disabled,
        created_at: row.created_at,
        last_seen_at: row.last_seen_at,
    }
}

/// Creates the first administrator, which the wizard is the only caller of.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the write fails, including when
/// the name is already taken.
pub async fn create_admin(
    db: &Database,
    username: Username,
    display_name: Option<String>,
    email: Option<String>,
    anon_by_default: bool,
) -> Result<UserRow, Error> {
    let row = db
        .users()
        .create(NewUser {
            display_name,
            email,
            is_admin: true,
            ..NewUser::person(username)
        })
        .await?;

    if anon_by_default {
        groups::join_default(db, row.id).await?;
    }

    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn context() -> AppContext {
        AppContext::new_mock(|_| {}).await.unwrap()
    }

    fn oidc() -> OidcConfig {
        OidcConfig {
            endpoint: "https://id.example.com".to_string(),
            client_id: "rustak".to_string(),
            client_secret: "secret".to_string(),
            scopes: Vec::new(),
            username_claim: "preferred_username".to_string(),
            groups_claim: "groups".to_string(),
            group_prefix: String::new(),
            strip_group_prefix: true,
            read_suffix: "_READ".to_string(),
            write_suffix: "_WRITE".to_string(),
            read_only_group: None,
            auto_create_groups: true,
            link_by_username: false,
            display_name: None,
        }
    }

    fn identity(username: &str, subject: &str) -> VerifiedIdentity {
        VerifiedIdentity {
            issuer: "https://id.example.com".to_string(),
            subject: subject.to_string(),
            username: Username::parse(username).unwrap(),
            display_name: Some("Ada Lovelace".to_string()),
            email: Some("ada@example.com".to_string()),
            groups: vec!["ops_WRITE".to_string()],
            claims: serde_json::json!({ "groups": ["ops_WRITE"] })
                .as_object()
                .cloned()
                .unwrap(),
        }
    }

    #[tokio::test]
    async fn a_first_sign_in_creates_the_account_and_its_channels() {
        let context = context().await;
        let db = context.db().clone();
        let row = provision(
            &context,
            &oidc(),
            &identity("ada", "subject-1"),
            false,
            true,
        )
        .await
        .unwrap();

        assert_eq!(row.username.as_str(), "ada");
        assert_eq!(row.source, UserSource::Oidc);
        assert_eq!(row.display_name.as_deref(), Some("Ada Lovelace"));

        let held = members::grants_for_user(&db, row.id).await.unwrap();
        assert!(held.iter().any(|held| held.group.is_anon()));
        assert!(held.iter().any(|held| held.group.as_str() == "ops"));
    }

    #[tokio::test]
    async fn a_rename_in_the_directory_follows_the_subject_rather_than_the_name() {
        let context = context().await;
        let first = provision(
            &context,
            &oidc(),
            &identity("ada", "subject-1"),
            false,
            true,
        )
        .await
        .unwrap();
        let renamed = provision(
            &context,
            &oidc(),
            &identity("ada.l", "subject-1"),
            false,
            true,
        )
        .await
        .unwrap();

        assert_eq!(first.id, renamed.id, "the same person keeps the same row");
        assert_eq!(renamed.username.as_str(), "ada.l");
    }

    #[tokio::test]
    async fn a_provider_cannot_walk_into_an_account_it_did_not_create() {
        let context = context().await;
        let db = context.db().clone();
        create_admin(&db, Username::parse("ada").unwrap(), None, None, true)
            .await
            .unwrap();

        let refused = provision(
            &context,
            &oidc(),
            &identity("ada", "subject-1"),
            false,
            true,
        )
        .await;

        assert!(refused.is_err(), "{refused:?}");
    }

    #[tokio::test]
    async fn linking_by_name_hands_the_existing_account_to_the_provider() {
        let context = context().await;
        let db = context.db().clone();
        let before = create_admin(&db, Username::parse("ada").unwrap(), None, None, true)
            .await
            .unwrap();

        let oidc = OidcConfig {
            link_by_username: true,
            ..oidc()
        };

        let linked = provision(&context, &oidc, &identity("ada", "subject-1"), false, true)
            .await
            .unwrap();

        assert_eq!(linked.id, before.id, "the same row, not a second one");
        assert_eq!(linked.source, UserSource::Oidc);
        assert_eq!(linked.oidc_subject.as_deref(), Some("subject-1"));
        assert!(
            linked
                .oidc_claims
                .as_deref()
                .unwrap_or("")
                .contains("ops_WRITE"),
            "the claims it was judged by are kept: {:?}",
            linked.oidc_claims
        );
        assert_eq!(
            linked.admin_override,
            Some(true),
            "the standing the wizard granted outlives the link"
        );
        assert!(linked.is_effective_admin());

        let held = members::grants_for_user(&db, linked.id).await.unwrap();
        assert!(held.iter().any(|held| held.group.as_str() == "ops"));
    }

    #[tokio::test]
    async fn linking_by_name_does_not_invent_a_standing_the_account_never_had() {
        let context = context().await;
        let db = context.db().clone();
        let before = db
            .users()
            .create(NewUser::person(Username::parse("grace").unwrap()))
            .await
            .unwrap();

        let oidc = OidcConfig {
            link_by_username: true,
            ..oidc()
        };

        let linked = provision(
            &context,
            &oidc,
            &identity("grace", "subject-2"),
            false,
            true,
        )
        .await
        .unwrap();

        assert_eq!(linked.id, before.id);
        assert_eq!(linked.admin_override, None);
        assert!(!linked.is_effective_admin());
    }

    #[tokio::test]
    async fn a_linked_account_is_found_by_subject_on_the_next_sign_in() {
        let context = context().await;
        let db = context.db().clone();
        create_admin(&db, Username::parse("ada").unwrap(), None, None, true)
            .await
            .unwrap();

        let oidc = OidcConfig {
            link_by_username: true,
            ..oidc()
        };

        let first = provision(&context, &oidc, &identity("ada", "subject-1"), false, true)
            .await
            .unwrap();
        let again = provision(&context, &oidc, &identity("ada", "subject-1"), false, true)
            .await
            .unwrap();

        assert_eq!(first.id, again.id);
    }

    #[tokio::test]
    async fn a_person_can_link_their_own_account_without_the_flag() {
        let context = context().await;
        let db = context.db().clone();
        let before = create_admin(&db, Username::parse("ada").unwrap(), None, None, true)
            .await
            .unwrap();

        let linked = link_identity(
            &context,
            &oidc(),
            &before,
            &identity("ada", "subject-1"),
            false,
        )
        .await
        .unwrap();

        assert_eq!(linked.id, before.id);
        assert_eq!(linked.source, UserSource::Oidc);
        assert_eq!(linked.admin_override, Some(true));

        let again = link_identity(
            &context,
            &oidc(),
            &linked,
            &identity("ada", "subject-1"),
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            again.id, before.id,
            "linking the same identity again is a refresh"
        );
    }

    #[tokio::test]
    async fn an_account_follows_the_providers_name_for_the_person() {
        let context = context().await;
        let db = context.db().clone();
        let before = create_admin(&db, Username::parse("ada").unwrap(), None, None, true)
            .await
            .unwrap();

        let linked = link_identity(
            &context,
            &oidc(),
            &before,
            &identity("ada.l", "subject-1"),
            false,
        )
        .await
        .unwrap();

        assert_eq!(linked.id, before.id);
        assert_eq!(linked.username.as_str(), "ada.l");
    }

    #[tokio::test]
    async fn an_account_cannot_be_linked_to_a_second_identity() {
        let context = context().await;
        let db = context.db().clone();
        let before = create_admin(&db, Username::parse("ada").unwrap(), None, None, true)
            .await
            .unwrap();
        let linked = link_identity(
            &context,
            &oidc(),
            &before,
            &identity("ada", "subject-1"),
            false,
        )
        .await
        .unwrap();

        let refused = link_identity(
            &context,
            &oidc(),
            &linked,
            &identity("ada", "subject-2"),
            false,
        )
        .await;

        assert!(refused.is_err(), "{refused:?}");
    }

    #[tokio::test]
    async fn an_identity_that_already_signs_in_as_somebody_else_is_refused() {
        let context = context().await;
        let db = context.db().clone();
        provision(
            &context,
            &oidc(),
            &identity("grace", "subject-1"),
            false,
            true,
        )
        .await
        .unwrap();
        let ada = create_admin(&db, Username::parse("ada").unwrap(), None, None, true)
            .await
            .unwrap();

        let refused = link_identity(
            &context,
            &oidc(),
            &ada,
            &identity("grace", "subject-1"),
            false,
        )
        .await;

        assert!(refused.is_err(), "{refused:?}");
        let still = db.users().get(ada.id).await.unwrap().unwrap();
        assert_eq!(still.source, UserSource::Local, "nothing was written");
    }

    #[tokio::test]
    async fn a_providers_name_that_is_another_account_here_is_refused() {
        let context = context().await;
        let db = context.db().clone();
        db.users()
            .create(NewUser::person(Username::parse("grace").unwrap()))
            .await
            .unwrap();
        let ada = create_admin(&db, Username::parse("ada").unwrap(), None, None, true)
            .await
            .unwrap();

        let refused = link_identity(
            &context,
            &oidc(),
            &ada,
            &identity("grace", "subject-9"),
            false,
        )
        .await;

        assert!(refused.is_err(), "{refused:?}");
    }

    #[tokio::test]
    async fn a_service_account_cannot_be_linked() {
        let context = context().await;
        let db = context.db().clone();
        let bot = db
            .users()
            .create(NewUser::service(Username::parse("relay").unwrap()))
            .await
            .unwrap();

        let refused = link_identity(
            &context,
            &oidc(),
            &bot,
            &identity("relay", "subject-1"),
            false,
        )
        .await;

        assert!(refused.is_err(), "{refused:?}");
    }

    #[tokio::test]
    async fn the_stored_flag_and_the_expression_each_grant_administration() {
        let context = context().await;
        let db = context.db().clone();
        let row = provision(
            &context,
            &oidc(),
            &identity("ada", "subject-1"),
            false,
            true,
        )
        .await
        .unwrap();

        let ordinary = principal(&db, &row, AuthMethod::SetupToken, false)
            .await
            .unwrap();
        assert!(!ordinary.is_admin);

        let by_acl = principal(&db, &row, AuthMethod::SetupToken, true)
            .await
            .unwrap();
        assert!(by_acl.is_admin);
    }

    #[tokio::test]
    async fn an_administrators_explicit_decision_outranks_the_expression() {
        let context = context().await;
        let db = context.db().clone();
        let row = provision(&context, &oidc(), &identity("ada", "subject-1"), true, true)
            .await
            .unwrap();

        db.users()
            .set_admin_override(row.id, Some(false))
            .await
            .unwrap();
        let row = db.users().get(row.id).await.unwrap().unwrap();

        let refused = principal(&db, &row, AuthMethod::SetupToken, true)
            .await
            .unwrap();

        assert!(
            !refused.is_admin,
            "an override is the one lever that does not need the configuration file editing",
        );
    }

    #[tokio::test]
    async fn a_disabled_account_administers_nothing() {
        let context = context().await;
        let db = context.db().clone();
        let row = provision(&context, &oidc(), &identity("ada", "subject-1"), true, true)
            .await
            .unwrap();

        db.users().set_disabled(row.id, true).await.unwrap();
        let row = db.users().get(row.id).await.unwrap().unwrap();

        assert!(
            !principal(&db, &row, AuthMethod::SetupToken, true)
                .await
                .unwrap()
                .is_admin
        );
    }

    #[tokio::test]
    async fn a_token_granted_no_administrative_scope_administers_nothing() {
        // R-01 H1. The account may administer; the credential presented on this
        // request was not granted it, and the narrower of the two is what the
        // request is answered under.
        let context = context().await;
        let db = context.db().clone();
        let row = provision(&context, &oidc(), &identity("ada", "subject-1"), true, true)
            .await
            .unwrap();

        let narrow = principal(
            &db,
            &row,
            AuthMethod::Bearer {
                jti: "a-token".to_string(),
                scope: "api".to_string(),
            },
            true,
        )
        .await
        .unwrap();

        assert!(
            !narrow.is_admin,
            "a token minted narrow must not carry everything its account may do",
        );

        let wide = principal(
            &db,
            &row,
            AuthMethod::Bearer {
                jti: "a-token".to_string(),
                scope: "api admin".to_string(),
            },
            true,
        )
        .await
        .unwrap();

        assert!(wide.is_admin);
    }

    #[tokio::test]
    async fn a_scope_that_merely_starts_with_admin_is_not_the_admin_scope() {
        let context = context().await;
        let db = context.db().clone();
        let row = provision(&context, &oidc(), &identity("ada", "subject-1"), true, true)
            .await
            .unwrap();

        let refused = principal(
            &db,
            &row,
            AuthMethod::Bearer {
                jti: "a-token".to_string(),
                scope: "api administrator-readonly".to_string(),
            },
            true,
        )
        .await
        .unwrap();

        assert!(!refused.is_admin);
    }

    #[tokio::test]
    async fn a_credential_that_carries_no_scope_is_bounded_by_the_account_alone() {
        // A client certificate, Basic and a passkey assertion are not scoped
        // grants; treating their absent scope as "not admin" would lock the
        // first-run wizard and every certificate-authenticated sidecar out.
        let context = context().await;
        let db = context.db().clone();
        let row = provision(&context, &oidc(), &identity("ada", "subject-1"), true, true)
            .await
            .unwrap();

        assert!(
            principal(
                &db,
                &row,
                AuthMethod::ClientCert {
                    fingerprint: "ab".to_string(),
                    serial: "01".to_string(),
                },
                true,
            )
            .await
            .unwrap()
            .is_admin
        );
    }

    #[tokio::test]
    async fn me_reports_the_channels_and_how_the_request_arrived() {
        let context = context().await;
        let db = context.db().clone();
        let row = provision(
            &context,
            &oidc(),
            &identity("ada", "subject-1"),
            false,
            true,
        )
        .await
        .unwrap();
        let principal = principal(
            &db,
            &row,
            AuthMethod::Bearer {
                jti: "a-token".to_string(),
                scope: "admin".to_string(),
            },
            false,
        )
        .await
        .unwrap();

        let me = me(&db, &row, &principal).await.unwrap();

        assert_eq!(me.username.as_str(), "ada");
        assert_eq!(me.via, AuthVia::Bearer);
        assert_eq!(me.display(), "Ada Lovelace");
        assert!(!me.groups.is_empty());
    }

    #[test]
    fn a_passkey_sign_in_is_reported_as_the_bearer_it_becomes() {
        assert_eq!(
            via_of(&AuthMethod::Passkey {
                credential_id: rustak_api::identity::CredentialId::new(1)
            }),
            AuthVia::Bearer
        );
        assert_eq!(
            via_of(&AuthMethod::ClientCert {
                fingerprint: "ab".to_string(),
                serial: "01".to_string()
            }),
            AuthVia::ClientCert
        );
    }
}
