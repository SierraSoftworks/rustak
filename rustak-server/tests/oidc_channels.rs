//! A `groups` claim reaches the stream in the request that carried it.
//!
//! M2-14 made the admin channels API invalidate the stream's cached channel
//! map, so a channel an administrator creates is a usable `<dest group>` at
//! once. The identity-provider path was left behind, because `apply_claims`
//! took a `&Database` and could not reach the stream from there — and the gap
//! is worse than the second it costs on creation, because a *membership* a
//! sign-in takes away never reached a live connection at all: a connection
//! holds the rights it authenticated with, so somebody the directory had just
//! removed from a channel kept receiving its traffic until they reconnected.
//!
//! These are the end-to-end assertions for both halves, against the real
//! `App`, the real `POST /api/v1/auth/token` and a real identity provider
//! ([`TestIdentityProvider`] serves discovery, a key set and RS256 tokens over
//! HTTP). Nothing waits: the routing cache refreshes itself within a second
//! anyway, so a test that slept would pass without the fix.
//!
//! Run with `cargo test -p rustak-server --features testing --test oidc_channels`.

#![cfg(feature = "testing")]

use std::sync::Arc;

use actix_web::{App, test};
use rustak_api::TokenResponse;
use rustak_cot::Event;
use rustak_cot::detail::Contact;
use rustak_cot::detail::contact::STREAMING_ENDPOINT;
use rustak_cot::detail::marti::{Dest, marti_element};
use rustak_server::prelude::*;
use rustak_server::stream::subscription::{ConnHandle, ConnStats, Outbound, Subscription};
use rustak_server::stream::{
    ConnId, Disposition, DropReason, Hub, LiveState, Router, StreamMetrics,
};
use rustak_server::testing::TestServer;
use rustak_server::testing::oidc::{CODE, TestIdentityProvider};

/// The channel every test here signs in claiming.
const CHANNEL: &str = "ops";

/// Signs in through the provider, as the console's callback does.
///
/// The same authorization code twice on purpose: the provider redeems it
/// whenever it is presented, so a second sign-in is a second redemption of the
/// claims the directory is saying *now*.
macro_rules! sign_in {
    ($app:expr) => {{
        let session: TokenResponse = test::call_and_read_body_json(
            &$app,
            test::TestRequest::post()
                .uri("/api/v1/auth/token")
                .set_json(serde_json::json!({
                    "code": CODE,
                    "redirect_uri": "https://localhost/auth/callback",
                    "code_verifier": "a-verifier-the-browser-kept",
                }))
                .to_request(),
        )
        .await;

        assert!(!session.token.is_empty(), "the sign-in was refused");

        session
    }};
}

/// A server that trusts `provider` and admits anybody it vouches for.
///
/// No default channel: every channel in these tests is one a claim asked for,
/// and `__ANON__` would otherwise connect every fixture to every other.
async fn federated(provider: &TestIdentityProvider) -> TestServer {
    let oidc = provider.config();

    TestServer::start_with(move |config| {
        config.auth.oidc = Some(oidc);
        config.auth.user_acl = Some(filt_rs::Filter::new("true").unwrap());
        config.auth.anon_group_default = false;
    })
    .await
}

/// Publishes a registry and a router over the server's own database.
///
/// What `StreamRuntime::bind` does on an installation with the stream listener
/// on, minus the socket: these tests are about what routing *decides*, and a
/// real handshake would add a certificate authority and an enrolment to every
/// one of them without changing a single assertion.
fn install_live(context: &AppContext) -> (Arc<Hub>, Router) {
    let hub = Arc::new(Hub::new());
    let metrics = Arc::new(StreamMetrics::default());
    let store = rustak_server::cot_store::CotStoreHandle::disabled();
    let router = Arc::new(Router::new(
        Arc::clone(&hub),
        context.db().clone(),
        store.clone(),
        rustak_server::stream::mission_hook::no_missions(),
        Arc::clone(&metrics),
        "rustak-test",
    ));

    context
        .install_live(Arc::new(LiveState::new(
            Arc::clone(&hub),
            Arc::clone(&router),
            store,
            metrics,
        )))
        .expect("the registry is installed once");

    (hub, Router::clone(&router))
}

/// Registers one connection holding the given bit positions.
fn join(
    hub: &Arc<Hub>,
    user_id: UserId,
    username: &str,
    grants: &[(u32, Direction)],
) -> (ConnId, tokio::sync::mpsc::Receiver<Outbound>) {
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
                user_id,
                Username::parse(username).expect("a usable username"),
                PrincipalKind::Person,
                AuthMethod::SetupToken,
            )
            .with_groups(Arc::new(groups)),
        ),
        Vec::new(),
        format!("{username:f>64}"),
        "127.0.0.1:9000".parse().unwrap(),
        ConnHandle::new(id, tx, Arc::new(ConnStats::default()), 512, Shutdown::new()),
    ));

    (id, rx)
}

/// A message addressed at one channel and nothing else.
fn addressed_to(uid: &str, callsign: &str, channel: &str) -> Event {
    let mut event = Event::builder("a-f-G-U-C", uid)
        .how("m-g")
        .point(51.5, -0.12)
        .typed(&Contact::new(callsign).with_endpoint(STREAMING_ENDPOINT))
        .build();

    event.detail.push(marti_element(&[Dest::group(channel)]));

    event
}

/// The channel a claim named, once the sign-in has been through.
async fn channel(server: &TestServer) -> rustak_server::db::repos::GroupRow {
    server
        .db()
        .groups()
        .get_by_name(&GroupName::parse(CHANNEL).expect("a usable channel name"))
        .await
        .expect("the channel table reads")
        .expect("the sign-in created the channel its claim named")
}

#[actix_web::test]
async fn a_channel_a_sign_in_created_is_a_destination_in_the_same_request() {
    let provider = TestIdentityProvider::start().await;
    provider.set_groups(&[CHANNEL]);

    let server = federated(&provider).await;
    let (hub, router) = install_live(&server.context);
    let app = test::init_service(App::new().configure(server.app())).await;

    assert!(
        server
            .db()
            .groups()
            .get_by_name(&GroupName::parse(CHANNEL).unwrap())
            .await
            .unwrap()
            .is_none(),
        "the channel is the sign-in's to create",
    );

    // Fills the cache with a map that does not have the channel in it, which is
    // the state the invalidation exists for. Without it the next lookup answers
    // from this reading for up to a second, whatever the table now says.
    let (probe, _probe_rx) = join(&hub, UserId::from(1), "probe", &[]);
    assert_eq!(
        router
            .handle_inbound(probe, addressed_to("UID-P", "PROBE", CHANNEL))
            .await,
        Disposition::Dropped(DropReason::NoSuchGroup(CHANNEL.into())),
    );

    sign_in!(app);

    let created = channel(&server).await;
    assert_eq!(created.source, rustak_api::GroupSource::Oidc);

    let (sender, _sender_rx) = join(
        &hub,
        UserId::from(2),
        "bravo",
        &[(created.bitpos, Direction::In)],
    );
    let (_reader, mut reader_rx) = join(
        &hub,
        UserId::from(3),
        "charlie",
        &[(created.bitpos, Direction::Out)],
    );

    assert_eq!(
        router
            .handle_inbound(sender, addressed_to("UID-B", "BRAVO", CHANNEL))
            .await,
        Disposition::Relayed {
            recipients: 1,
            explicit: true,
        },
        "a channel a sign-in created is routable without waiting for the refresh",
    );
    assert!(
        reader_rx.try_recv().is_ok(),
        "and its reader actually received the message",
    );
}

#[actix_web::test]
async fn a_claim_that_stopped_being_sent_takes_a_live_connection_off_the_channel() {
    let provider = TestIdentityProvider::start().await;
    provider.set_groups(&[CHANNEL]);

    let server = federated(&provider).await;
    let (hub, router) = install_live(&server.context);
    let app = test::init_service(App::new().configure(server.app())).await;

    sign_in!(app);

    let alice = server
        .db()
        .users()
        .get_by_username(&Username::parse("alice").unwrap())
        .await
        .unwrap()
        .expect("the sign-in created the account");
    let ops = channel(&server).await;

    // Her device, holding what the first sign-in granted her, and somebody else
    // who may publish into the channel.
    let (_alice, mut alice_rx) = join(
        &hub,
        alice.id,
        "alice",
        &[(ops.bitpos, Direction::In), (ops.bitpos, Direction::Out)],
    );
    let (peer, _peer_rx) = join(
        &hub,
        UserId::from(9_000),
        "bravo",
        &[(ops.bitpos, Direction::In)],
    );

    assert_eq!(
        router
            .handle_inbound(peer, addressed_to("UID-B", "BRAVO", CHANNEL))
            .await,
        Disposition::Relayed {
            recipients: 1,
            explicit: true,
        },
    );
    assert!(
        alice_rx.try_recv().is_ok(),
        "her connection is reachable on the channel to begin with",
    );

    // The directory stops saying she is in it.
    provider.set_groups(&[]);
    sign_in!(app);

    assert_eq!(
        router
            .handle_inbound(peer, addressed_to("UID-C", "BRAVO", CHANNEL))
            .await,
        Disposition::Dropped(DropReason::NoRecipients),
        "the connection she already had lost the channel in the same request",
    );

    // Asserted second because the queue holds the notice and nothing else: the
    // message above reached nobody, which is the point.
    match alice_rx.try_recv() {
        Ok(Outbound::Event(encoded)) => assert_eq!(
            encoded.event().r#type,
            "t-x-g-c",
            "her device is told to discard its map and re-fetch",
        ),
        other => panic!("expected a channel-change notice, got {other:?}"),
    }

    assert!(
        server
            .db()
            .members()
            .list_for_user(alice.id)
            .await
            .unwrap()
            .is_empty(),
        "and the membership behind it is gone, not merely unroutable",
    );
}
