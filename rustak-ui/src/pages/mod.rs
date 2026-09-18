//! The routed pages.

mod activity;
mod auth_callback;
mod dashboard;
#[cfg(debug_assertions)]
mod demo;
mod landing;
mod load;
mod login;
mod not_found;
mod protected;
mod settings;
mod setup;
mod stubs;
mod users;

pub use activity::Activity;
pub use auth_callback::AuthCallback;
pub use dashboard::Dashboard;
#[cfg(debug_assertions)]
pub use demo::DemoControls;
pub use landing::Landing;
pub use login::Login;
pub use not_found::NotFound;
pub use protected::Protected;
pub use settings::Settings;
pub use setup::Setup;
pub use stubs::{Credentials, Devices, Groups, Missions, Packages, Profiles, Services};
pub use users::Users;
