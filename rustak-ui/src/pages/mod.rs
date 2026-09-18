//! The routed pages.

mod activity;
mod auth_callback;
mod dashboard;
#[cfg(debug_assertions)]
mod demo;
mod devices;
mod group_members;
mod groups;
mod landing;
mod load;
mod login;
mod me;
mod not_found;
mod panels;
mod protected;
mod settings;
mod setup;
mod stubs;
mod user_create;
mod user_detail;
mod users;

pub use activity::Activity;
pub use auth_callback::AuthCallback;
pub use dashboard::Dashboard;
#[cfg(debug_assertions)]
pub use demo::DemoControls;
pub use devices::Devices;
pub use groups::Groups;
pub use landing::Landing;
pub use login::Login;
pub use me::Me;
pub use not_found::NotFound;
pub use protected::Protected;
pub use settings::Settings;
pub use setup::Setup;
pub use stubs::{Missions, Packages, Profiles, Services};
pub use user_detail::UserDetail;
pub use users::Users;
