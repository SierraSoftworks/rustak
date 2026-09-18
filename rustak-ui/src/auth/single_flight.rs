//! A single-flight coordinator.
//!
//! It collapses concurrent runs of an async operation onto one shared execution,
//! so the operation runs at most once at a time however many callers ask for it
//! together (see [`super::refresh_session`] for why that matters). The state
//! lives in an `Rc`/`RefCell` behind a `thread_local!` rather than behind a
//! lock, because WebAssembly is single-threaded.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;

use futures::FutureExt;
use futures::future::{LocalBoxFuture, Shared};

struct Inner<T: Clone> {
    /// The run currently in flight, tagged with the generation that started it.
    /// Cloned for every caller that joins it, and cleared once it settles so the
    /// next call starts afresh.
    current: RefCell<Option<(u64, Shared<LocalBoxFuture<'static, T>>)>>,
    generation: Cell<u64>,
}

pub struct SingleFlight<T: Clone> {
    inner: Rc<Inner<T>>,
}

impl<T: Clone + 'static> SingleFlight<T> {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(Inner {
                current: RefCell::new(None),
                generation: Cell::new(0),
            }),
        }
    }

    /// Runs the future produced by `start`, unless a run begun by an earlier,
    /// still-unsettled call is already in flight — in which case that one is
    /// joined instead and `start` is never invoked. The returned future owns a
    /// handle to the shared state, so it can be awaited outside the borrow that
    /// produced it.
    pub fn run<F>(&self, start: F) -> impl Future<Output = T> + use<F, T>
    where
        F: FnOnce() -> LocalBoxFuture<'static, T>,
    {
        let inner = self.inner.clone();
        async move {
            let (generation, shared) = {
                let mut current = inner.current.borrow_mut();
                if let Some((generation, existing)) = current.as_ref() {
                    (*generation, existing.clone())
                } else {
                    let generation = inner.generation.get().wrapping_add(1);
                    inner.generation.set(generation);
                    let shared = start().shared();
                    *current = Some((generation, shared.clone()));
                    (generation, shared)
                }
            };

            let result = shared.await;

            // Retire this run so the next call starts afresh — but only if a
            // later run has not already replaced it, or we would strand the
            // newer one and let a second concurrent run start.
            let mut current = inner.current.borrow_mut();
            if matches!(current.as_ref(), Some((g, _)) if *g == generation) {
                *current = None;
            }
            result
        }
    }
}
