//! Channels themselves: creating them, describing them, and what a `groups`
//! claim means.
//!
//! A thin layer over the `groups` repository. It exists so that the places
//! which need "which channels are there" — the admin API, the
//! identity-provider sign-in, the Marti groups endpoints — agree on the naming
//! and mapping rules rather than each inventing their own. Who is *in* a
//! channel is [`super::members`]; this module is about the channels.

use rustak_api::{
    CreateGroupRequest, Direction, Group, GroupName, GroupPatch, GroupSource, MembershipSource,
    UserId,
};
use rustak_core::prelude::*;

use crate::config::OidcConfig;
use crate::db::{
    Database,
    repos::{GroupRow, NewGroup},
};
use crate::services::{AppContext, Services};

/// Every channel that has not been deleted.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the read fails.
pub async fn list(db: &Database) -> Result<Vec<GroupRow>, Error> {
    db.groups().list().await
}

/// Creates a channel an administrator asked for.
///
/// The bit position is the repository's to allocate: it is what the router
/// indexes on, and a reused one would hand an existing channel's traffic to a
/// new set of members.
///
/// Takes the context rather than the database because a new channel is a
/// change to the *routing* table as well as to the stored one: it invalidates
/// the stream's cached channel map so the new channel is a usable
/// `<dest group>` at once rather than within a second.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the name is already taken, when it
/// is not a name we can store, or when every bit position is in use; a
/// [`human_errors::Kind::System`] error if a read or write fails.
#[instrument("identity.groups.create", skip_all, fields(group = %request.name), err(Display))]
pub async fn create(context: &AppContext, request: &CreateGroupRequest) -> Result<GroupRow, Error> {
    let db = context.db();

    if db.groups().get_by_name(&request.name).await?.is_some() {
        return Err(human_errors::user(
            format!("There is already a channel called '{}'.", request.name),
            &["Channel names are case sensitive, so check for one that differs only by case."],
        ));
    }

    let created = db
        .groups()
        .create(NewGroup {
            name: request.name.clone(),
            description: description(request.description.as_deref()),
            source: GroupSource::Manual,
        })
        .await?;

    info!(group = %created.name, bitpos = created.bitpos, "Created a channel.");

    routing_changed(context);

    Ok(created)
}

/// Changes a channel's description.
///
/// The name is not changeable: it is what every membership, every `groups`
/// claim and every client's cached selection refers to, so renaming a channel
/// is deleting it and making another.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a read or write fails.
#[instrument("identity.groups.patch", skip_all, fields(group = %name), err(Display))]
pub async fn patch(
    context: &AppContext,
    name: &GroupName,
    change: &GroupPatch,
) -> Result<Option<GroupRow>, Error> {
    let db = context.db();

    let Some(group) = db.groups().get_by_name(name).await? else {
        return Ok(None);
    };

    if let Some(text) = change.description.as_deref() {
        db.groups()
            .set_description(group.id, description(Some(text)))
            .await?;
    }

    // Nothing here moves a bit position today, and the invalidation is here
    // anyway: this is the function a rename or a re-allocation would be added
    // to, and a cache that is only refreshed on two of the three ways the
    // table can change is the kind of thing that is found a year later.
    routing_changed(context);

    db.groups().get(group.id).await
}

/// Deletes a channel, keeping its bit position reserved.
///
/// Soft rather than hard: a live subscription holds bit positions rather than
/// names, so freeing one before every subscription has been refreshed would
/// hand this channel's traffic to whichever channel took the bit next.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when asked to delete the default
/// channel, and a [`human_errors::Kind::System`] error if a read or write
/// fails.
#[instrument("identity.groups.delete", skip_all, fields(group = %name), err(Display))]
pub async fn delete(context: &AppContext, name: &GroupName) -> Result<bool, Error> {
    let db = context.db();

    let Some(group) = db.groups().get_by_name(name).await? else {
        return Ok(false);
    };

    let deleted = db.groups().soft_delete(group.id).await?;

    if deleted {
        info!(group = %name, "Deleted a channel; its bit position stays reserved.");

        routing_changed(context);
    }

    Ok(deleted)
}

/// Tells the routing path that the channel table has changed.
///
/// `<dest group="…">` resolves a name to a bit position through the stream's
/// [`GroupCache`](crate::stream::GroupCache), which is cached because reading
/// the table per destination element was a denial of service (R-03 H2). It
/// refreshes itself within a second either way, so this is not correctness —
/// it is the second an administrator who has just created a channel and told a
/// client to send to it would otherwise spend watching nothing happen.
///
/// Never fails and never logs at a level anybody has to read: an installation
/// with no stream listener has no cache to invalidate, and a channel that was
/// written is not un-written by a notification that could not be delivered.
fn routing_changed(context: &AppContext) {
    if !context.has_live() {
        return;
    }

    match context.live() {
        Ok(live) => live.channels_changed(),
        Err(err) => {
            debug!(error = %err, "Could not reach the stream after a channel change.");
        }
    }
}

/// A channel as the API describes it.
pub fn to_dto(row: &GroupRow) -> Group {
    Group {
        id: row.id,
        name: row.name.clone(),
        bitpos: row.bitpos,
        description: row.description.clone(),
        source: row.source,
    }
}

/// Trims a description, treating an empty one as absent.
fn description(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
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
    //
    // The *suffix* decides which rule applies and the **first** occurrence
    // decides where the name ends — TAK Server's `LdapAuthenticator` is
    // `endsWith(suffix)` followed by `substring(0, indexOf(suffix))` (research
    // `06` line 580), so `A_READ_B_READ` is channel `A`, not `A_READ_B`. R-02
    // M2: this used to use `strip_suffix`, which cuts at the last one, and a
    // directory group like that landed in a differently named channel here than
    // it does against TAK Server.
    let (name, direction) = match (
        truncate_at_first(claim, &oidc.read_suffix),
        truncate_at_first(claim, &oidc.write_suffix),
    ) {
        (Some(name), _) => (name, Direction::Out),
        (_, Some(name)) => (name, Direction::In),
        _ => (claim, Direction::Both),
    };

    GroupName::parse(name)
        .inspect_err(
            |err| debug!(claim = %claim, error = %err, "Ignoring an unusable group claim."),
        )
        .ok()
        .map(|name| (name, direction))
}

/// The claim up to its **first** `suffix`, when it ends with one.
///
/// [`None`] for an empty suffix (the installation does not model that
/// direction) and for a claim that does not end with it — the `endsWith` gate
/// and the `indexOf` cut are two different questions, and TAK Server asks both.
fn truncate_at_first<'a>(claim: &'a str, suffix: &str) -> Option<&'a str> {
    if suffix.is_empty() || !claim.ends_with(suffix) {
        return None;
    }

    claim.find(suffix).map(|at| &claim[..at])
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
    use crate::identity::members;
    use crate::stream::{Disposition, DropReason, Router};

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
    fn a_repeated_suffix_truncates_at_the_first_one() {
        // TAK Server is `endsWith(suffix)` then `substring(0, indexOf(suffix))`
        // (research 06 line 580), so `A_READ_B_READ` is channel `A`. `_READ`
        // used to be stripped from the *end*, which put the same directory group
        // in a differently named channel here than it lands in against TAK
        // Server — wrong CoT visibility rather than a parse failure. R-02 M2.
        let oidc = oidc();

        assert_eq!(
            interpret(&oidc, "tak-ops_READ_night_READ"),
            Some((GroupName::parse("ops").unwrap(), Direction::Out)),
        );
        assert_eq!(
            interpret(&oidc, "tak-ops_WRITE_night_WRITE"),
            Some((GroupName::parse("ops").unwrap(), Direction::In)),
        );
        // A claim *containing* a suffix but not ending in one is a bare name:
        // the `endsWith` gate and the `indexOf` cut are two different questions.
        assert_eq!(
            interpret(&oidc, "tak-ops_READ_night"),
            Some((GroupName::parse("ops_READ_night").unwrap(), Direction::Both)),
        );
        // Only `_WRITE` is a suffix here, so the write rule applies and the cut
        // is at the first `_WRITE` — the `_READ` in the middle is just part of
        // the name.
        assert_eq!(
            interpret(&oidc, "tak-ops_READ_WRITE"),
            Some((GroupName::parse("ops_READ").unwrap(), Direction::In)),
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

        let held = members::grants_for_user(&db, user).await.unwrap();

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

        let names: Vec<_> = members::grants_for_user(&db, user)
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

        let held = members::grants_for_user(&db, user).await.unwrap();

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

        assert!(
            members::grants_for_user(&db, user)
                .await
                .unwrap()
                .is_empty()
        );
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

        let held = members::grants_for_user(&db, user).await.unwrap();

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

        let held = members::grants_for_user(&db, user).await.unwrap();

        assert!(held.iter().any(|held| held.group.is_anon()));
        assert!(held.iter().any(|held| held.group.as_str() == "ops"));
    }

    /// A router, a registry and an `AppContext` that reaches both.
    ///
    /// The router reads the context's own database, which is what makes the
    /// channel this test creates through [`create`] the channel the routing
    /// path then looks for.
    async fn live_context() -> (AppContext, std::sync::Arc<crate::stream::Hub>, Router) {
        use std::sync::Arc;

        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let hub = Arc::new(crate::stream::Hub::new());
        let metrics = Arc::new(crate::stream::StreamMetrics::default());
        let store = crate::cot_store::CotStoreHandle::disabled();
        let router = Arc::new(Router::new(
            Arc::clone(&hub),
            context.db().clone(),
            store.clone(),
            crate::stream::mission_hook::no_missions(),
            Arc::clone(&metrics),
            "rustak-test",
        ));

        context
            .install_live(Arc::new(crate::stream::LiveState::new(
                Arc::clone(&hub),
                Arc::clone(&router),
                store,
                metrics,
            )))
            .unwrap();

        let routing = Router::clone(&router);

        (context, hub, routing)
    }

    /// Registers one connection holding the given bit positions.
    fn join(
        hub: &std::sync::Arc<crate::stream::Hub>,
        name: &str,
        grants: &[(u32, Direction)],
    ) -> (
        crate::stream::ConnId,
        tokio::sync::mpsc::Receiver<crate::stream::subscription::Outbound>,
    ) {
        use std::sync::Arc;

        use crate::stream::subscription::{ConnHandle, ConnStats, Subscription};

        let mut groups = GroupSet::new();
        for (bitpos, direction) in grants {
            groups.set(*bitpos, *direction);
        }

        let id = hub.next_id();
        let (tx, rx) = tokio::sync::mpsc::channel(16);

        hub.register(Subscription::new(
            id,
            Arc::new(
                Principal::new(
                    UserId::from(1),
                    Username::parse(name).unwrap(),
                    PrincipalKind::Person,
                    AuthMethod::SetupToken,
                )
                .with_groups(Arc::new(groups)),
            ),
            Vec::new(),
            format!("{name:f>64}"),
            "127.0.0.1:9000".parse().unwrap(),
            ConnHandle::new(id, tx, Arc::new(ConnStats::default()), 512, Shutdown::new()),
        ));

        (id, rx)
    }

    /// A message addressed at one channel and nothing else.
    fn addressed_to(uid: &str, callsign: &str, channel: &str) -> rustak_cot::Event {
        use rustak_cot::detail::marti::{Dest, marti_element};

        let mut event = rustak_cot::Event::builder("a-f-G-U-C", uid)
            .how("m-g")
            .point(51.5, -0.12)
            .typed(
                &rustak_cot::detail::Contact::new(callsign)
                    .with_endpoint(rustak_cot::detail::contact::STREAMING_ENDPOINT),
            )
            .build();

        event.detail.push(marti_element(&[Dest::group(channel)]));

        event
    }

    #[tokio::test]
    async fn a_channel_created_now_can_be_routed_to_now() {
        // `<dest group>` resolves names through a cache that refreshes itself
        // within a second (R-03 H2), so without the hook in `create` an
        // administrator who makes a channel and tells a client to send to it
        // watches the first message reach nobody. The probe below is what
        // fills that cache with an answer that does not have the channel in
        // it, which is the state the hook exists for.
        let (context, hub, router) = live_context().await;

        let (probe_from, _probe_rx) = join(&hub, "alpha", &[]);
        assert_eq!(
            router
                .handle_inbound(probe_from, addressed_to("UID-A", "ALPHA", "ops"))
                .await,
            Disposition::Dropped(DropReason::NoSuchGroup("ops".into())),
            "the cache now holds a map that does not have `ops` in it",
        );

        let created = create(
            &context,
            &CreateGroupRequest {
                name: GroupName::parse("ops").unwrap(),
                description: None,
            },
        )
        .await
        .unwrap();

        let (sender, _sender_rx) = join(&hub, "bravo", &[(created.bitpos, Direction::In)]);
        let (_reader, mut reader_rx) = join(&hub, "charlie", &[(created.bitpos, Direction::Out)]);

        assert_eq!(
            router
                .handle_inbound(sender, addressed_to("UID-B", "BRAVO", "ops"))
                .await,
            Disposition::Relayed {
                recipients: 1,
                explicit: true,
            },
            "the channel is routable in the same moment it was created",
        );
        assert!(
            reader_rx.try_recv().is_ok(),
            "and its reader actually received the message",
        );
    }

    #[tokio::test]
    async fn deleting_a_channel_takes_it_off_the_routing_path_at_once() {
        let (context, hub, router) = live_context().await;

        let created = create(
            &context,
            &CreateGroupRequest {
                name: GroupName::parse("ops").unwrap(),
                description: None,
            },
        )
        .await
        .unwrap();

        let (sender, _sender_rx) = join(&hub, "bravo", &[(created.bitpos, Direction::In)]);
        let (_reader, _reader_rx) = join(&hub, "charlie", &[(created.bitpos, Direction::Out)]);

        assert!(matches!(
            router
                .handle_inbound(sender, addressed_to("UID-B", "BRAVO", "ops"))
                .await,
            Disposition::Relayed { .. },
        ));

        assert!(
            delete(&context, &GroupName::parse("ops").unwrap())
                .await
                .unwrap()
        );

        assert_eq!(
            router
                .handle_inbound(sender, addressed_to("UID-C", "BRAVO", "ops"))
                .await,
            Disposition::Dropped(DropReason::NoSuchGroup("ops".into())),
            "a deleted channel stops being a destination without waiting for the refresh",
        );
    }

    #[tokio::test]
    async fn a_channel_change_on_an_installation_with_no_stream_is_not_an_error() {
        // `[stream.tls] enabled = false` is a supported deployment, and the
        // admin API must not start failing on it because there is no cache to
        // invalidate.
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        let created = create(
            &context,
            &CreateGroupRequest {
                name: GroupName::parse("ops").unwrap(),
                description: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(created.name.as_str(), "ops");
        assert!(
            delete(&context, &created.name).await.unwrap(),
            "and deleting it is no more of a problem",
        );
    }
}
