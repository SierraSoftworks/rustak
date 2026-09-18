//! Handles that are not available when the context is built.
//!
//! [`AppContext`](super::AppContext) is created as soon as the database and the
//! secret store are open, because everything else start-up does needs it. Two of
//! the handles it carries are not ready at that moment: the content store and
//! the JWT signing keys are both built by `rustak-server`'s M0-10 modules, and
//! the keys in particular are *read out of the database* using the secret store
//! the context already holds. Making them constructor arguments would mean
//! building the context twice, or threading a second, half-populated struct
//! through start-up.
//!
//! [`Late`] is the alternative: a shared cell that start-up fills exactly once,
//! before the listeners and the job host are spawned, and which every clone of
//! the context sees. Reading one before it is filled is a
//! [`Kind::System`](human_errors::Kind::System) error naming the handle, which
//! is the honest answer — it means the wiring in `runtime::run_all` is in the
//! wrong order, not that the operator did anything.

use std::{
    fmt,
    sync::{Arc, OnceLock},
};

use rustak_core::prelude::*;

/// A handle installed once during start-up, after the context exists.
///
/// Every clone shares one cell, so installing a value through any clone is
/// visible through all of them — including the clones actix and the job host
/// took before it was installed.
pub struct Late<T: ?Sized> {
    cell: Arc<OnceLock<Arc<T>>>,
    what: &'static str,
}

impl<T: ?Sized> Late<T> {
    /// An empty slot. `what` names the handle in error messages, as a noun
    /// phrase: `"the content store"`.
    pub fn new(what: &'static str) -> Self {
        Self {
            cell: Arc::new(OnceLock::new()),
            what,
        }
    }

    /// Installs the handle, which start-up does exactly once.
    ///
    /// # Errors
    ///
    /// A [`Kind::System`](human_errors::Kind::System) error when the slot is
    /// already filled. Refused rather than ignored: a second install means two
    /// parts of start-up each believe they own the handle, and whichever of
    /// them loses would go on using a value nobody else can see.
    pub fn install(&self, value: Arc<T>) -> Result<(), Error> {
        self.cell.set(value).map_err(|_| {
            human_errors::system(
                format!("{} has already been set up.", capitalised(self.what)),
                crate::db::ADVICE_REPORT_DEV,
            )
        })
    }

    /// The handle, or [`None`] when start-up has not installed it yet.
    pub fn get(&self) -> Option<Arc<T>> {
        self.cell.get().cloned()
    }

    /// Whether the handle has been installed.
    pub fn is_installed(&self) -> bool {
        self.cell.get().is_some()
    }

    /// The handle, or an error saying it is not ready.
    ///
    /// # Errors
    ///
    /// A [`Kind::System`](human_errors::Kind::System) error naming the handle
    /// when it has not been installed.
    pub fn require(&self) -> Result<Arc<T>, Error> {
        self.get().ok_or_else(|| {
            human_errors::system(
                format!("{} is not available yet.", capitalised(self.what)),
                crate::db::ADVICE_REPORT_DEV,
            )
        })
    }
}

/// Written out because deriving it would demand `T: Clone`, which is not what
/// is being cloned — the cell is shared, not copied.
impl<T: ?Sized> Clone for Late<T> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
            what: self.what,
        }
    }
}

impl<T: ?Sized> fmt::Debug for Late<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Late({}, {})",
            self.what,
            if self.is_installed() {
                "installed"
            } else {
                "pending"
            }
        )
    }
}

/// `the content store` → `The content store`, so the message reads as a
/// sentence without every call site having to write the name twice.
fn capitalised(what: &str) -> String {
    let mut chars = what.chars();

    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Stands in for a type a later brief introduces.
///
/// It has no values, so a [`Late<Pending>`] is permanently empty and every
/// accessor for it reports that the handle is not available. That is deliberate:
/// it keeps the slot, its accessor and its documentation in place — and keeps
/// them honest — until the brief that owns the real type lands, at which point
/// the alias naming this type is repointed and nothing else changes.
///
/// See [`JwtKeys`](super::JwtKeys) and [`ContentStore`](super::ContentStore).
#[derive(Debug)]
pub enum Pending {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_starts_empty() {
        let slot: Late<String> = Late::new("the content store");

        assert!(!slot.is_installed());
        assert!(slot.get().is_none());
        assert!(
            slot.require()
                .unwrap_err()
                .to_string()
                .contains("The content store is not available yet.")
        );
    }

    #[test]
    fn every_clone_sees_what_any_clone_installs() {
        // The property the whole type exists for: actix and the job host take
        // their clones of the context before start-up has finished filling it.
        let slot: Late<String> = Late::new("the signing keys");
        let taken_early = slot.clone();

        slot.install(Arc::new("installed".to_string())).unwrap();

        assert_eq!(taken_early.require().unwrap().as_str(), "installed");
    }

    #[test]
    fn a_second_install_is_refused() {
        let slot: Late<String> = Late::new("the signing keys");
        slot.install(Arc::new("first".to_string())).unwrap();

        let refused = slot.install(Arc::new("second".to_string()));

        assert!(refused.is_err());
        assert_eq!(slot.require().unwrap().as_str(), "first");
    }

    #[test]
    fn a_pending_slot_can_never_be_filled() {
        let slot: Late<Pending> = Late::new("the content store");

        // There is no value of `Pending`, so this is the whole of the type's
        // behaviour until the brief that owns the real type lands.
        assert!(slot.get().is_none());
        assert!(!slot.is_installed());
    }
}
