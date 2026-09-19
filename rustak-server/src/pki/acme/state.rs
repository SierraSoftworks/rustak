//! The two pieces of ACME state the listeners and the renewal share.
//!
//! One handle rather than two globals: the certificate resolver the public
//! listener presents through, and the `http-01` answers an order is currently
//! prepared to give. Both are written by the renewal — a queue job holding
//! nothing but [`Services`](crate::services::Services) — and read by a
//! listener, which is why they used to be process-wide statics. They live in
//! [`AppContext`](crate::services::AppContext) now, so a test gets its own and
//! two servers in one process cannot answer for each other.
//!
//! Start-up fills the resolver in
//! [`web::tls::resolve`](crate::web::tls::resolve); everything else reads it
//! and treats its absence — a listener that is not in `mode = "acme"` — as "no
//! certificate can be swapped in", not as a failure.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use crate::pki::tls::HotSwapCertResolver;

/// The public listener's certificate resolver and the `http-01` answer map.
///
/// Cheap to create and always valid: an installation that never switches ACME
/// on simply holds an empty one.
#[derive(Default)]
pub struct AcmeState {
    /// The resolver the public listener was built with, when it was built with
    /// one.
    resolver: RwLock<Option<Arc<HotSwapCertResolver>>>,

    /// Tokens we are currently prepared to answer, and what to answer with.
    ///
    /// A `BTreeMap` rather than a hash map: it holds at most one entry per name
    /// in one order, and a deterministic iteration order makes a test's failure
    /// message the same every time.
    tokens: RwLock<BTreeMap<String, String>>,
}

impl AcmeState {
    /// An empty handle, as every context starts with.
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes the resolver the public listener was built with.
    ///
    /// Called once, by [`web::tls`](crate::web::tls), and only for
    /// `[web.public.tls] mode = "acme"`. Replacing it is allowed rather than
    /// refused so that a test which builds a second listener over one context
    /// is not fighting the first one's leftovers.
    pub fn publish_resolver(&self, resolver: Arc<HotSwapCertResolver>) {
        if let Ok(mut held) = self.resolver.write() {
            *held = Some(resolver);
        }
    }

    /// The published resolver, or [`None`] when this installation is not in
    /// ACME mode.
    pub fn resolver(&self) -> Option<Arc<HotSwapCertResolver>> {
        self.resolver.read().ok().and_then(|held| held.clone())
    }

    /// Arms the `http-01` answer for one token.
    pub fn publish(&self, token: &str, key_authorization: &str) {
        if let Ok(mut tokens) = self.tokens.write() {
            tokens.insert(token.to_owned(), key_authorization.to_owned());
        }
    }

    /// Disarms it. Runs as soon as validation finishes, whether it succeeded or
    /// not: an answer left armed is a path that serves a secret indefinitely.
    pub fn withdraw(&self, token: &str) {
        if let Ok(mut tokens) = self.tokens.write() {
            tokens.remove(token);
        }
    }

    /// What to answer a request for `token`, if anything.
    ///
    /// Public so that a test can check what is armed at the moment an authority
    /// would fetch it, which is the only moment it matters.
    pub fn answer(&self, token: &str) -> Option<String> {
        self.tokens.read().ok()?.get(token).cloned()
    }

    /// How many answers are armed, for logging and for [`Debug`].
    pub fn armed(&self) -> usize {
        self.tokens.read().map_or(0, |tokens| tokens.len())
    }
}

/// Written out because a key authorization is the proof of control for a name:
/// the count is what a log or a bug report may carry, and the values are not.
impl std::fmt::Debug for AcmeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcmeState")
            .field(
                "resolver",
                &if self.resolver().is_some() {
                    "installed"
                } else {
                    "pending"
                },
            )
            .field("armed", &self.armed())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_handle_has_neither_a_resolver_nor_an_answer() {
        let state = AcmeState::new();

        assert!(state.resolver().is_none());
        assert_eq!(state.answer("never-published"), None);
        assert_eq!(state.armed(), 0);
    }

    #[test]
    fn what_one_holder_publishes_every_other_holder_sees() {
        // The property the whole type exists for: the renewal arms the answer
        // and the listener is the one asked for it.
        let state = Arc::new(AcmeState::new());
        let listener = Arc::clone(&state);

        state.publish("token", "token.thumbprint");
        assert_eq!(
            listener.answer("token").as_deref(),
            Some("token.thumbprint")
        );

        state.withdraw("token");
        assert_eq!(listener.answer("token"), None);
    }

    #[test]
    fn two_handles_do_not_answer_for_each_other() {
        // What the process-wide map could not promise, and what lets two
        // servers — or two tests — run in one process.
        let one = AcmeState::new();
        let other = AcmeState::new();

        one.publish("token", "one.thumbprint");

        assert_eq!(other.answer("token"), None);
    }

    #[test]
    fn the_resolver_is_what_the_renewal_finds() {
        let state = AcmeState::new();
        let resolver = HotSwapCertResolver::new(None);

        state.publish_resolver(Arc::clone(&resolver));

        assert!(Arc::ptr_eq(&state.resolver().unwrap(), &resolver));
    }

    #[test]
    fn a_debug_dump_counts_the_answers_and_prints_none_of_them() {
        // A key authorization proves control of a name; a bug report carrying
        // one hands that proof to whoever reads it.
        let state = AcmeState::new();
        state.publish("token", "token.thumbprint");

        let rendered = format!("{state:?}");

        assert!(rendered.contains("armed: 1"), "{rendered}");
        assert!(!rendered.contains("thumbprint"), "{rendered}");
        assert!(!rendered.contains("token"), "{rendered}");
    }
}
