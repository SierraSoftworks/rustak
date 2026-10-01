//! What a limiter subject names, and how often each kind has been refused.
//!
//! The limiter is keyed on `(address, subject)` and the subject is whatever
//! the endpoint is guessing at. Three kinds of thing turn up there, and this is
//! the one place that tells them apart:
//!
//! * **an endpoint's own name** — [`subjects`] — where the only thing telling
//!   one caller from another is the address ([`LockoutClass::Address`]);
//! * **an OAuth client**, namespaced by [`CLIENT_PREFIX`] so that a client
//!   identifier equal to somebody's username never shares their bucket
//!   ([`LockoutClass::Client`]);
//! * **anything else**, which is a username ([`LockoutClass::Account`]).
//!
//! Those are tier 2, the pair. A tier-1 lockout names no subject at all — it is
//! the address — and is always [`LockoutClass::Source`], which [`classify`]
//! never answers.
//!
//! The endpoint names live here rather than beside each endpoint so that
//! adding one is adding it to [`subjects::ALL`], and a subject that is not in
//! the list is counted as an account rather than silently as nothing.

use std::sync::atomic::{AtomicU64, Ordering};

use rustak_api::{LockoutClass, LockoutCounter};

/// The subjects an endpoint uses when it has no account to name.
pub mod subjects {
    /// Every passkey ceremony (R-01 L2: one bucket for all four).
    pub const PASSKEY: &str = "passkey";
    /// `POST /api/v1/setup/admin`, spending the first-run setup token.
    pub const SETUP_TOKEN: &str = "setup-token";
    /// `/api/v1/auth/token`, `/auth/refresh` and the OIDC link.
    pub const AUTH_TOKEN: &str = "auth-token";
    /// The `/oauth/token` password grant with no username at all.
    pub const OAUTH_TOKEN: &str = "oauth-token";
    /// A workload identity, before it has named the account it speaks for.
    pub const WORKLOAD_IDENTITY: &str = "workload-identity";

    /// All of the above.
    pub const ALL: &[&str] = &[
        PASSKEY,
        SETUP_TOKEN,
        AUTH_TOKEN,
        OAUTH_TOKEN,
        WORKLOAD_IDENTITY,
    ];
}

/// What the code grant puts in front of a client identifier.
///
/// The same value as `auth::oauth_server::code_grant`'s own private constant.
/// If the two ever differ, a client's lockout is listed and counted as an
/// account's — mislabelled, but still listed and still clearable.
pub const CLIENT_PREFIX: &str = "oauth-client:";

/// Which class a subject belongs to, and the part of it worth showing.
pub fn classify(subject: &str) -> (LockoutClass, &str) {
    if subjects::ALL.contains(&subject) {
        return (LockoutClass::Address, subject);
    }

    match subject.strip_prefix(CLIENT_PREFIX) {
        Some(client) => (LockoutClass::Client, client),
        None => (LockoutClass::Account, subject),
    }
}

/// The subject a class and a shown key came from — [`classify`] backwards.
///
/// [`None`] for a pair [`classify`] could never have produced, such as an
/// `address` key that is not an endpoint name, so an administrator cannot
/// clear a key under a class it was never counted as — and for
/// [`LockoutClass::Source`], which names an address and has no subject.
pub fn subject_for(class: LockoutClass, key: &str) -> Option<String> {
    let subject = match class {
        LockoutClass::Address => key.to_owned(),
        LockoutClass::Client => format!("{CLIENT_PREFIX}{key}"),
        LockoutClass::Account => key.to_owned(),
        LockoutClass::Source => return None,
    };

    (classify(&subject) == (class, key)).then_some(subject)
}

/// Refusals and lockouts, one pair of counters per class.
///
/// Relaxed atomics, as `StreamMetrics` keeps: the counts are only ever added
/// to, and a reader on another thread needs no more than the latest value.
#[derive(Debug, Default)]
pub struct Counters {
    refusals: [AtomicU64; LockoutClass::ALL.len()],
    lockouts: [AtomicU64; LockoutClass::ALL.len()],
}

impl Counters {
    /// Counts an attempt turned away because its key was locked out.
    pub fn refused(&self, class: LockoutClass) {
        self.refusals[class.index()].fetch_add(1, Ordering::Relaxed);
    }

    /// Counts a lockout that began.
    pub fn locked(&self, class: LockoutClass) {
        self.lockouts[class.index()].fetch_add(1, Ordering::Relaxed);
    }

    /// Every class's counts, in [`LockoutClass::ALL`] order.
    pub fn snapshot(&self) -> Vec<LockoutCounter> {
        LockoutClass::ALL
            .iter()
            .map(|class| LockoutCounter {
                class: *class,
                refusals: self.refusals[class.index()].load(Ordering::Relaxed),
                lockouts: self.lockouts[class.index()].load(Ordering::Relaxed),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_endpoint_name_is_an_address_lockout() {
        for subject in subjects::ALL {
            assert_eq!(classify(subject), (LockoutClass::Address, *subject));
        }
    }

    #[test]
    fn a_client_is_shown_without_its_namespace() {
        assert_eq!(
            classify("oauth-client:cloudtak"),
            (LockoutClass::Client, "cloudtak")
        );
    }

    #[test]
    fn anything_else_is_an_account() {
        assert_eq!(classify("ada"), (LockoutClass::Account, "ada"));
    }

    #[test]
    fn a_shown_key_leads_back_to_the_subject_it_came_from() {
        for subject in [
            "passkey",
            "oauth-client:cloudtak",
            "ada",
            "workload-identity",
        ] {
            let (class, key) = classify(subject);

            assert_eq!(subject_for(class, key).as_deref(), Some(subject));
        }
    }

    #[test]
    fn a_class_a_key_was_never_counted_under_leads_nowhere() {
        // Otherwise "clear the account called passkey" would forgive every
        // passkey ceremony from that address.
        assert_eq!(subject_for(LockoutClass::Account, "passkey"), None);
        assert_eq!(subject_for(LockoutClass::Account, "oauth-client:x"), None);
        assert_eq!(subject_for(LockoutClass::Address, "ada"), None);
        assert_eq!(subject_for(LockoutClass::Source, "198.51.100.4/32"), None);
        assert_eq!(
            subject_for(LockoutClass::Client, "passkey").as_deref(),
            Some("oauth-client:passkey")
        );
    }

    #[test]
    fn the_endpoints_use_the_names_classified_here() {
        assert_eq!(
            crate::auth::workload::RATE_LIMIT_SUBJECT,
            subjects::WORKLOAD_IDENTITY
        );
    }

    #[test]
    fn counters_are_kept_per_class() {
        let counters = Counters::default();

        counters.refused(LockoutClass::Account);
        counters.refused(LockoutClass::Account);
        counters.locked(LockoutClass::Client);

        let snapshot = counters.snapshot();

        assert_eq!(snapshot.len(), LockoutClass::ALL.len());
        assert_eq!(snapshot[LockoutClass::Account.index()].refusals, 2);
        assert_eq!(snapshot[LockoutClass::Client.index()].lockouts, 1);
        assert_eq!(snapshot[LockoutClass::Address.index()].refusals, 0);
    }
}
