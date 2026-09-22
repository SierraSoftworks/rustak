//! What an account has chosen for itself, as the preferences its devices take.
//!
//! An administrator's profiles configure a fleet; this configures one person's
//! devices with what that person chose under Account in the console. It is
//! generated rather than stored — there is no profile row behind it — and it
//! rides in the same two packages everything else does: the one a device is
//! handed after enrolling, and the one it asks for on every connection.
//!
//! # Which way round it is sent
//!
//! First, before any profile an administrator configured. Nothing here knows
//! what ATAK does with two files that set the same key, so nothing here
//! depends on it; but a fleet-wide profile that pins the same key is policy,
//! and the file it is in is the later one.
//!
//! # Adding a preference
//!
//! A new field on [`UserPreferencesPatch`] that a device should hear about is
//! one more `maybe` in [`device_prefs`]. One that only matters to the console
//! is not mentioned here at all.

use chrono::{DateTime, Utc};
use rustak_api::{Symbology, UserPreferencesPatch};
use rustak_core::prelude::*;

use super::builder::ProfileFileData;
use super::prefs::{APP_PREFS, PrefGroup, render};
use crate::db::Database;

/// The generated file an account's own choices are delivered in.
pub const ACCOUNT_PREF: &str = "rustak-account.pref";

/// The ATAK preferences an account's choices come to.
///
/// Empty for an account whose choices are all the console's own business,
/// which is a file not worth sending.
pub fn device_prefs(chosen: &UserPreferencesPatch) -> PrefGroup {
    PrefGroup::app(APP_PREFS).maybe(
        "symbologyProvider",
        chosen.symbology.map(symbology_provider),
    )
}

/// The file for one account, or [`None`] when it has chosen nothing a device
/// takes — or nothing since `since`, which is when the device last asked.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the read fails.
pub async fn account_file(
    db: &Database,
    user: UserId,
    since: Option<DateTime<Utc>>,
) -> Result<Option<ProfileFileData>, Error> {
    let Some((chosen, updated)) = db.user_preferences().chosen(user).await? else {
        return Ok(None);
    };

    let group = device_prefs(&chosen);
    if group.is_empty() || since.is_some_and(|since| updated <= since) {
        return Ok(None);
    }

    Ok(Some(ProfileFileData::new(
        ACCOUNT_PREF,
        render(&[group]).into_bytes(),
        updated,
    )))
}

/// ATAK's name for an edition: the `symbologyProvider` it selects by.
const fn symbology_provider(symbology: Symbology) -> &'static str {
    match symbology {
        Symbology::Milstd2525C => "2525C",
        Symbology::Milstd2525D => "2525D",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edition_is_delivered_under_the_name_atak_selects_it_by() {
        for (symbology, expected) in [
            (Symbology::Milstd2525C, ">2525C<"),
            (Symbology::Milstd2525D, ">2525D<"),
        ] {
            let document = render(&[device_prefs(&UserPreferencesPatch {
                symbology: Some(symbology),
            })]);

            assert!(document.contains("key=\"symbologyProvider\""), "{document}");
            assert!(
                document.contains("class=\"class java.lang.String\""),
                "{document}"
            );
            assert!(document.contains(expected), "{document}");
        }
    }

    #[test]
    fn an_account_that_has_chosen_nothing_for_its_devices_sends_nothing() {
        assert!(device_prefs(&UserPreferencesPatch::default()).is_empty());
    }
}
