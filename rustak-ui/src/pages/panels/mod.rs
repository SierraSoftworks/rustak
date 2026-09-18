//! The panels several pages are made of.
//!
//! An account's devices, credentials and channels appear in three places — an
//! administrator's view of somebody else, a person's view of their own, and the
//! whole-installation lists — and the endpoints behind them take the same
//! `username` argument in every case. Writing each panel once is what keeps
//! "enrol my phone" and "enrol somebody's phone" the same flow instead of two
//! that drift.

mod channels;
mod credentials;
mod devices;
mod mint;
mod profile;

pub use channels::ChannelsPanel;
pub use credentials::CredentialsPanel;
pub use devices::DevicesPanel;
pub use profile::ProfilePanel;
