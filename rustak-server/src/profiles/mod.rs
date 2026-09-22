//! Device profiles: what a device is configured with, and how it gets it.
//!
//! ATAK asks this server for configuration twice — once immediately after
//! enrolling, and once on every stream connection — and imports whatever comes
//! back as a Mission Package. That is the whole mechanism by which an operator
//! can turn Channels on, set a callsign, or ship a map source to a fleet
//! without touching a device.
//!
//! # The layers
//!
//! * [`model`] is storage: the profiles, their files and their typed
//!   preferences.
//! * [`prefs`] renders a `.pref` document, whose exact bytes matter.
//! * [`account`] is what one account chose for itself, as its devices take it.
//! * [`builder`] packs files into the Mission Package layouts ATAK expects.
//! * [`config_package`] builds the manual configuration zip an operator emails
//!   to somebody setting a client up by hand.
//! * [`service`] is what the routes call: it decides which profiles a caller is
//!   owed, assembles their files, and hands back bytes plus a `Last-Modified`.
//!
//! # The one preference everything else depends on
//!
//! `deviceProfileEnableOnConnect` defaults to **false** in ATAK, so a device
//! that has never received an enrolment profile will never ask for a connection
//! profile. [`prefs::enrollment_defaults`] turns it on, which is why the
//! enrolment profile is generated even when an operator has configured nothing.

pub mod account;
pub mod builder;
pub mod catalog;
pub mod config_package;
pub mod model;
pub mod package;
pub mod prefs;
pub mod repo;
pub mod service;

pub use builder::{
    MULTI_FILE, PROFILE_FILENAME, ProfileFileData, build_multifile_package, build_profile_package,
    guess_content_type,
};
pub use config_package::{ConfigPackageInput, build_itak, build_wintak_atak};
pub use model::{Delivery, NewProfile, ProfileFileRow, ProfileRow};
pub use package::{multifile_package, profile_package};
pub use prefs::{APP_PREFS, COT_STREAMS, PrefGroup, UserSettings, enrollment_defaults, render};
pub use repo::ProfilesRepo;
pub use service::{Assembled, ProfileService};
