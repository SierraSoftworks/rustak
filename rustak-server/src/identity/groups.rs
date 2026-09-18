//! Channels: what a `groups` claim means, and what a person is a member of.
//!
//! A thin layer over the `groups` and `group_members` repositories. It exists
//! so that the two places which need "this person's channels" — the admin API's
//! `/me` and the identity-provider sign-in — agree on the mapping rules rather
//! than each inventing their own.

use rustak_api::{Direction, GroupMembership, GroupName, GroupSource, MembershipSource, UserId};
use rustak_core::prelude::*;

use crate::config::OidcConfig;
use crate::db::{
    Database,
    repos::{GroupRow, NewGroup},
};

/// A person's channels, named, for the admin API.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if either read fails.
pub async fn memberships(db: &Database, user_id: UserId) -> Result<Vec<GroupMembership>, Error> {
    let held = db.members().list_for_user(user_id).await?;

    if held.is_empty() {
        return Ok(Vec::new());
    }

    let groups = db.groups().list().await?;
    let mut memberships: Vec<GroupMembership> = held
        .iter()
        .filter_map(|membership| {
            groups
                .iter()
                .find(|group| group.id == membership.group_id)
                .map(|group| GroupMembership {
                    group: group.name.clone(),
                    direction: membership.direction,
                    source: Some(membership.source),
                })
        })
        .collect();

    memberships.sort_by(|left, right| {
        (left.group.as_str(), left.direction.as_str())
            .cmp(&(right.group.as_str(), right.direction.as_str()))
    });

    Ok(memberships)
}

/// Puts a new account in the default channel, when the installation wants that.
///
/// Every TAK client expects to be able to talk on `__ANON__` the moment it
/// connects, so an installation that turns this off has to grant channels by
/// hand before anybody can send anything.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the read or the write fails.
pub async fn join_default(db: &Database, user_id: UserId) -> Result<(), Error> {
    let Some(anon) = db.groups().get_by_name(&GroupName::anon()).await? else {
        warn!("The default channel is missing, so a new account joined nothing.");
        return Ok(());
    };

    db.members()
        .grant(user_id, anon.id, Direction::Both, MembershipSource::Manual)
        .await
}

/// Replaces the memberships an identity provider granted.
///
/// Run at every sign-in, because a claim that stopped being sent has to stop
/// granting anything — and the repository does it in one transaction so nobody
/// is momentarily in no channels at all.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a channel cannot be read or
/// created, or the write fails.
pub async fn apply_claims(
    db: &Database,
    oidc: &OidcConfig,
    user_id: UserId,
    claimed: &[String],
) -> Result<(), Error> {
    let read_only = claimed
        .iter()
        .any(|claim| Some(claim.as_str()) == oidc.read_only_group.as_deref());

    let mut granted = Vec::new();

    for claim in claimed {
        let Some((name, direction)) = interpret(oidc, claim) else {
            continue;
        };

        // The read-only marker is a floor, not a preference: somebody in it
        // must not be able to write whatever their other claims say.
        let direction = if read_only { Direction::Out } else { direction };

        let Some(group) = lookup_or_create(db, oidc, &name).await? else {
            continue;
        };

        granted.push((group.id, direction));
    }

    db.members().replace_provider_grants(user_id, granted).await
}

/// Turns one claimed group into a channel name and a direction.
///
/// Returns [`None`] for a claim outside the configured prefix, or one whose
/// name is not something we can store — an identity provider is free to have
/// groups that are nothing to do with us, and refusing the sign-in over one
/// would be absurd.
fn interpret(oidc: &OidcConfig, claim: &str) -> Option<(GroupName, Direction)> {
    let claim = claim.strip_prefix(&oidc.group_prefix).map(|stripped| {
        if oidc.strip_group_prefix {
            stripped
        } else {
            claim
        }
    })?;

    // Checked longest-first only in the sense that the two suffixes are
    // disjoint; a group ending in neither grants both directions, which is what
    // a directory that does not model read and write separately means.
    let (name, direction) = match (
        claim.strip_suffix(&oidc.read_suffix),
        claim.strip_suffix(&oidc.write_suffix),
    ) {
        (Some(name), _) if !oidc.read_suffix.is_empty() => (name, Direction::Out),
        (_, Some(name)) if !oidc.write_suffix.is_empty() => (name, Direction::In),
        _ => (claim, Direction::Both),
    };

    GroupName::parse(name)
        .inspect_err(
            |err| debug!(claim = %claim, error = %err, "Ignoring an unusable group claim."),
        )
        .ok()
        .map(|name| (name, direction))
}

/// Finds a channel, creating it when the installation lets the provider define
/// the channel list.
async fn lookup_or_create(
    db: &Database,
    oidc: &OidcConfig,
    name: &GroupName,
) -> Result<Option<GroupRow>, Error> {
    if let Some(existing) = db.groups().get_by_name(name).await? {
        return Ok(Some(existing));
    }

    if !oidc.auto_create_groups {
        debug!(group = %name, "A claimed channel does not exist and auto-creation is off.");
        return Ok(None);
    }

    let created = db
        .groups()
        .create(NewGroup {
            name: name.clone(),
            description: None,
            source: GroupSource::Oidc,
        })
        .await?;

    info!(group = %name, "Created a channel an identity provider claimed.");

    Ok(Some(created))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::NewUser;

    async fn database() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    fn oidc() -> OidcConfig {
        OidcConfig {
            endpoint: "https://id.example.com".to_string(),
            client_id: "rustak".to_string(),
            client_secret: "secret".to_string(),
            scopes: Vec::new(),
            username_claim: "preferred_username".to_string(),
            groups_claim: "groups".to_string(),
            group_prefix: "tak-".to_string(),
            strip_group_prefix: true,
            read_suffix: "_READ".to_string(),
            write_suffix: "_WRITE".to_string(),
            read_only_group: None,
            auto_create_groups: true,
            link_by_username: false,
            display_name: None,
        }
    }

    async fn user(db: &Database) -> UserId {
        db.users()
            .create(NewUser::person(Username::parse("ada").unwrap()))
            .await
            .unwrap()
            .id
    }

    #[test]
    fn the_suffixes_decide_which_way_a_channel_runs() {
        let oidc = oidc();

        assert_eq!(
            interpret(&oidc, "tak-ops_WRITE"),
            Some((GroupName::parse("ops").unwrap(), Direction::In))
        );
        assert_eq!(
            interpret(&oidc, "tak-ops_READ"),
            Some((GroupName::parse("ops").unwrap(), Direction::Out))
        );
        assert_eq!(
            interpret(&oidc, "tak-ops"),
            Some((GroupName::parse("ops").unwrap(), Direction::Both))
        );
    }

    #[test]
    fn a_claim_outside_the_prefix_is_none_of_our_business() {
        assert_eq!(interpret(&oidc(), "engineering"), None);
    }

    #[test]
    fn the_prefix_is_kept_when_the_installation_asks_for_it() {
        let oidc = OidcConfig {
            strip_group_prefix: false,
            ..oidc()
        };

        assert_eq!(
            interpret(&oidc, "tak-ops"),
            Some((GroupName::parse("tak-ops").unwrap(), Direction::Both))
        );
    }

    #[tokio::test]
    async fn claimed_channels_are_created_and_granted() {
        let db = database().await;
        let user = user(&db).await;

        apply_claims(
            &db,
            &oidc(),
            user,
            &["tak-ops_WRITE".to_string(), "tak-weather_READ".to_string()],
        )
        .await
        .unwrap();

        let held = memberships(&db, user).await.unwrap();

        assert_eq!(held.len(), 2);
        assert_eq!(held[0].group.as_str(), "ops");
        assert_eq!(held[0].direction, Direction::In);
        assert_eq!(held[0].source, Some(MembershipSource::Oidc));
        assert_eq!(held[1].group.as_str(), "weather");
        assert_eq!(held[1].direction, Direction::Out);
    }

    #[tokio::test]
    async fn a_claim_that_stopped_being_sent_stops_granting_anything() {
        let db = database().await;
        let user = user(&db).await;

        apply_claims(&db, &oidc(), user, &["tak-ops".to_string()])
            .await
            .unwrap();
        apply_claims(&db, &oidc(), user, &["tak-weather".to_string()])
            .await
            .unwrap();

        let names: Vec<_> = memberships(&db, user)
            .await
            .unwrap()
            .into_iter()
            .map(|held| held.group.as_str().to_string())
            .collect();

        assert!(!names.contains(&"ops".to_string()));
        assert!(names.contains(&"weather".to_string()));
    }

    #[tokio::test]
    async fn the_read_only_group_outranks_every_other_claim() {
        let db = database().await;
        let user = user(&db).await;
        let oidc = OidcConfig {
            read_only_group: Some("observers".to_string()),
            ..oidc()
        };

        apply_claims(
            &db,
            &oidc,
            user,
            &["observers".to_string(), "tak-ops_WRITE".to_string()],
        )
        .await
        .unwrap();

        let held = memberships(&db, user).await.unwrap();

        assert_eq!(held.len(), 1);
        assert_eq!(
            held[0].direction,
            Direction::Out,
            "somebody marked read-only must not be granted write by another claim",
        );
    }

    #[tokio::test]
    async fn a_claimed_channel_is_ignored_when_the_list_is_curated_here() {
        let db = database().await;
        let user = user(&db).await;
        let oidc = OidcConfig {
            auto_create_groups: false,
            ..oidc()
        };

        apply_claims(&db, &oidc, user, &["tak-ops".to_string()])
            .await
            .unwrap();

        assert!(memberships(&db, user).await.unwrap().is_empty());
        assert!(
            db.groups()
                .get_by_name(&GroupName::parse("ops").unwrap())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_new_account_joins_the_default_channel() {
        let db = database().await;
        let user = user(&db).await;

        join_default(&db, user).await.unwrap();

        let held = memberships(&db, user).await.unwrap();

        assert_eq!(held.len(), 2, "the default channel is granted both ways");
        assert!(held.iter().all(|held| held.group.is_anon()));
    }

    #[tokio::test]
    async fn a_manual_grant_survives_a_provider_sign_in() {
        // The two sources are kept apart on purpose: an administrator's grant
        // is not something a directory gets to revoke.
        let db = database().await;
        let user = user(&db).await;

        join_default(&db, user).await.unwrap();
        apply_claims(&db, &oidc(), user, &["tak-ops".to_string()])
            .await
            .unwrap();

        let held = memberships(&db, user).await.unwrap();

        assert!(held.iter().any(|held| held.group.is_anon()));
        assert!(held.iter().any(|held| held.group.as_str() == "ops"));
    }
}
