//! The account-level active-channel selection, driven through the real listener.
//!
//! `PUT /Marti/api/groups/active` with no `clientUid` is the account's own
//! selection — CloudTAK's browser half, the admin UI and a script all send it
//! that way, because none of them has a device. M2-06 kept that answer in the
//! key/value store, where `/Marti/api/groups/*` could read it and the routing
//! path could not; a device enrolled *afterwards* therefore had no
//! `device_group_state` rows, and the router read "nothing switched off" and
//! delivered on a channel the account had switched off.
//!
//! `user_group_state` (migration `0012`) is joined by `members.effective`, so
//! the selection now applies from the new device's very first connection. These
//! are the tests that would have caught the gap: they assert *delivery*, not
//! what an endpoint says about it.

mod stream_support;

use std::time::Duration;

use rustak_api::identity::Direction;
use rustak_api::{ActiveGroup, GroupName};
use rustak_core::prelude::{DeviceUid, Username};
use rustak_server::db::repos::NewDevice;
use rustak_server::identity::members;
use rustak_server::prelude::Services as _;

use stream_support::{EXPECT, Harness, SETTLE};

/// The two directions a full member of a channel holds.
const BOTH: Direction = Direction::Both;

/// The account's own selection for one channel, in one direction.
fn state(name: &str, direction: Direction, active: bool) -> ActiveGroup {
    ActiveGroup {
        group: GroupName::parse(name).expect("a usable channel name"),
        direction,
        active,
    }
}

/// Records an account-level selection the way a caller with no device does.
async fn account_selection(harness: &Harness, username: &str, states: &[ActiveGroup]) {
    let db = harness.context.db();
    let user = db
        .users()
        .get_by_username(&Username::parse(username).unwrap())
        .await
        .unwrap()
        .expect("the account under test");

    members::set_active_for_user(db, user.id, states)
        .await
        .expect("the account-level selection");
}

#[tokio::test]
async fn a_device_enrolled_after_an_account_level_change_routes_on_it() {
    // The regression this table exists for. Ada switches Blue off from a
    // browser, then enrols a laptop. The laptop has never called the channels
    // endpoint, so it has no state of its own — and it must still not be sent
    // Blue traffic.
    let harness = Harness::start().await;
    harness.enroll("ada", "UID-PHONE", &[("Blue", BOTH)]).await;
    let bob = harness.enroll("bob", "UID-BOB", &[("Blue", BOTH)]).await;

    account_selection(&harness, "ada", &[state("Blue", Direction::Out, false)]).await;

    let laptop_id = harness.enroll("ada", "UID-LAPTOP", &[("Blue", BOTH)]).await;
    let mut laptop = harness.eud(&laptop_id, "LAPTOP").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    harness.await_connected(2).await;
    drain(&mut laptop).await;

    bravo.send_sa(51.6, -0.13).await.unwrap();

    laptop
        .expect_none(SETTLE)
        .await
        .expect("the account switched Blue off before this device existed");

    harness.stop().await;
}

#[tokio::test]
async fn an_account_that_has_switched_nothing_off_still_receives_everything() {
    // The control for the test above: the same shape, with no selection, so a
    // failure there is about the selection rather than about the harness.
    let harness = Harness::start().await;
    let bob = harness.enroll("bob", "UID-BOB", &[("Blue", BOTH)]).await;
    let laptop_id = harness.enroll("ada", "UID-LAPTOP", &[("Blue", BOTH)]).await;

    let mut laptop = harness.eud(&laptop_id, "LAPTOP").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    harness.await_connected(2).await;
    drain(&mut laptop).await;

    bravo.send_sa(51.6, -0.13).await.unwrap();

    laptop
        .expect_uid("UID-BOB", EXPECT)
        .await
        .expect("nobody has switched anything off");

    harness.stop().await;
}

#[tokio::test]
async fn a_device_that_has_switched_the_channel_on_overrules_the_account() {
    // Most specific first: the account is the *default* a device inherits, not
    // an override of what that device said itself.
    let harness = Harness::start().await;
    let bob = harness.enroll("bob", "UID-BOB", &[("Blue", BOTH)]).await;
    let laptop_id = harness.enroll("ada", "UID-LAPTOP", &[("Blue", BOTH)]).await;

    account_selection(&harness, "ada", &[state("Blue", Direction::Out, false)]).await;

    // The device row is written when a client first connects, so a state for a
    // device that has not connected yet needs the row made first — which is
    // exactly what `PUT …/active?clientUid=` from a device that is offline
    // would do.
    let db = harness.context.db();
    let ada = db
        .users()
        .get_by_username(&Username::parse("ada").unwrap())
        .await
        .unwrap()
        .expect("the account under test");
    let laptop_row = db
        .devices()
        .create(NewDevice::new(
            DeviceUid::parse("UID-LAPTOP").unwrap(),
            ada.id,
        ))
        .await
        .expect("the device row the client will reconnect to");

    members::set_active(db, laptop_row.id, &[state("Blue", Direction::Out, true)])
        .await
        .unwrap();

    let mut laptop = harness.eud(&laptop_id, "LAPTOP").await;
    let mut bravo = harness.eud(&bob, "BRAVO").await;
    harness.await_connected(2).await;
    drain(&mut laptop).await;

    bravo.send_sa(51.6, -0.13).await.unwrap();

    laptop
        .expect_uid("UID-BOB", EXPECT)
        .await
        .expect("this device said Blue is on, whatever the account's default is");

    harness.stop().await;
}

/// Reads whatever a client has already been sent, so that a later assertion is
/// about what happens next rather than about the replay.
async fn drain(eud: &mut rustak_client::stream::testing::Eud) {
    let _ = eud.expect_none(Duration::from_millis(150)).await;
}
