//! The clients that have enrolled.
//!
//! An administrator asking for nobody in particular gets every device in the
//! installation; anybody else gets their own, because that is what the server
//! answers rather than a refusal.

use rustak_api::{ActiveGroup, Device, DeviceUid, Username};

use crate::api::{ApiError, delete_empty, get_json, put_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// Every device, or only `username`'s.
pub async fn list(username: Option<&Username>) -> Result<Vec<Device>, ApiError> {
    demo!(Ok(fixtures::devices(username)));

    match username {
        Some(username) => {
            get_json(&format!(
                "/devices?username={}",
                urlencode(username.as_str())
            ))
            .await
        }
        None => get_json("/devices").await,
    }
}

/// Forgets a device.
///
/// This is not a certificate revocation: the certificate belongs to the
/// account rather than to the row, and taking it back is what revoking the
/// credential it was issued against does.
pub async fn forget(uid: &DeviceUid) -> Result<(), ApiError> {
    demo!(fixtures::forget_device(uid));

    delete_empty(&format!("/devices/{}", urlencode(uid.as_str()))).await
}

/// Switches channels on or off for one device, and answers with the state that
/// resulted — which is not always what was asked for, because a channel deleted
/// since the device cached it is dropped rather than recreated.
#[allow(dead_code)]
pub async fn set_active_groups(
    uid: &DeviceUid,
    wanted: &[ActiveGroup],
) -> Result<Vec<ActiveGroup>, ApiError> {
    demo!(Ok(fixtures::set_active_groups(uid, wanted)));

    put_json(
        &format!("/devices/{}/active-groups", urlencode(uid.as_str())),
        &wanted.to_vec(),
    )
    .await
}
