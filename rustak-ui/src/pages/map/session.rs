//! The part of the map page that outlives a render: the connection to the
//! server, what is on the map, and the map.
//!
//! One task reads the feed and one keeps time. Both are started by [`start`]
//! and both watch the same flag, so stopping is setting it and cancelling
//! whatever read is in flight.
//!
//! # Feed first, snapshot second
//!
//! Every connection — the first, and every reconnection — opens the feed and
//! *then* reads the snapshot. Whatever is relayed in between arrives twice and
//! the [`Store`] keeps the newer; the other order would lose it. A `reset`
//! from the server is handled the same way: drop the feed and start again.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::Utc;
use gloo_timers::future::TimeoutFuture;
use rustak_api::MapUpdate;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, HtmlElement};
use yew::{AttrValue, Callback, NodeRef};

use crate::api::map::{Canceller, Feed};
use crate::api::{self, ApiError};
use crate::components::StatusTone;

use super::glue::{Basemap, Map};
use super::store::{Changes, Store};

/// How long to wait before trying again after the network failed.
const RETRY_MS: u32 = 5_000;

/// How long to wait after the *server* refused. It said why, and the reason —
/// no stream listener, too many maps open — does not change in five seconds.
const RETRY_REFUSED_MS: u32 = 30_000;

/// How often time is let pass: staleness, and the page's own redraw.
const TICK_MS: u32 = 1_000;

/// What the page says about its connection.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedStatus {
    Connecting,
    Live,
    /// The snapshot was read and the feed was not: what is drawn is what the
    /// server held a moment ago, and it is not moving.
    NotLive(String),
    Reconnecting(String),
    /// There is no map at all: the libraries did not load, or there is no WebGL.
    Failed(String),
    /// The server no longer accepts this session. The map has been emptied:
    /// what it showed was shown to somebody who may not be entitled to it now.
    SignedOut(String),
}

impl FeedStatus {
    /// The pill's tone and label, and the sentence behind them.
    pub fn describe(&self) -> (StatusTone, AttrValue, Option<AttrValue>) {
        let said = |message: &String| Some(AttrValue::from(message.clone()));

        match self {
            Self::Connecting => (StatusTone::Neutral, "Connecting".into(), None),
            Self::Live => (StatusTone::Ok, "Live".into(), None),
            Self::NotLive(why) => (StatusTone::Warning, "Not live".into(), said(why)),
            Self::Reconnecting(why) => (StatusTone::Warning, "Reconnecting".into(), said(why)),
            Self::Failed(why) => (StatusTone::Error, "Unavailable".into(), said(why)),
            Self::SignedOut(why) => (StatusTone::Error, "Signed out".into(), said(why)),
        }
    }
}

/// What the page holds between renders.
#[derive(Default)]
pub struct Session {
    store: Store,
    map: Option<Map>,
    selected: Option<String>,
    feed: Option<Canceller>,
    /// Whether anything has changed since the page last drew itself.
    dirty: bool,
    /// Whether the view has been moved to fit what is on the map. Once: after
    /// that the view is the reader's.
    fitted: bool,
}

impl Session {
    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn popover_element(&self) -> Option<Element> {
        self.map.as_ref().map(Map::popover_element)
    }

    pub fn select(&mut self, uid: Option<String>) {
        if let Some(map) = &self.map {
            map.select(uid.as_deref());
        }
        self.selected = uid;
    }

    pub fn fly_to(&self, uid: &str) {
        if let Some(map) = &self.map {
            map.fly_to(uid);
        }
    }

    pub fn fit_all(&self) {
        if let Some(map) = &self.map {
            map.fit_all();
        }
    }
}

/// The running half, as the page holds it.
#[derive(Clone)]
pub struct Running {
    session: Rc<RefCell<Session>>,
    alive: Rc<Cell<bool>>,
    on_status: Callback<FeedStatus>,
    on_select: Callback<Option<String>>,
    on_redraw: Callback<()>,
    on_signed_out: Callback<()>,
}

/// What the page wants to hear about.
pub struct Listeners {
    pub on_status: Callback<FeedStatus>,
    pub on_select: Callback<Option<String>>,
    pub on_redraw: Callback<()>,
    /// The server refused the session. The console re-resolves it, which is
    /// what puts the sign-in prompt where this page was.
    pub on_signed_out: Callback<()>,
}

/// Starts the map, the feed and the clock.
pub fn start(session: Rc<RefCell<Session>>, container: NodeRef, listeners: Listeners) -> Running {
    let running = Running {
        session,
        alive: Rc::new(Cell::new(true)),
        on_status: listeners.on_status,
        on_select: listeners.on_select,
        on_redraw: listeners.on_redraw,
        on_signed_out: listeners.on_signed_out,
    };

    spawn_local(running.clone().run(container));
    spawn_local(running.clone().keep_time());

    running
}

impl Running {
    /// Ends both tasks and takes the map off the page.
    pub fn stop(&self) {
        self.alive.set(false);

        let mut session = self.session.borrow_mut();
        if let Some(feed) = session.feed.take() {
            feed.cancel();
        }
        session.map = None;
    }

    async fn run(self, container: NodeRef) {
        let Some(element) = container.cast::<HtmlElement>() else {
            return;
        };

        let on_select = self.on_select.clone();
        let basemap = Basemap::default();
        let created = Map::create(&element, &basemap, move |uid| on_select.emit(uid));

        match created.await {
            // The page went away while the libraries were loading.
            Ok(_) if !self.alive.get() => return,
            Ok(map) => self.session.borrow_mut().map = Some(map),
            Err(message) => return self.on_status.emit(FeedStatus::Failed(message)),
        }

        while self.alive.get() {
            let ended = self.connect().await;
            if !self.alive.get() {
                return;
            }

            let wait = match &ended {
                // Trying again would only be refused again.
                FeedStatus::SignedOut(_) => return self.on_status.emit(ended),
                FeedStatus::NotLive(_) => RETRY_REFUSED_MS,
                _ => RETRY_MS,
            };
            self.on_status.emit(ended);
            TimeoutFuture::new(wait).await;
        }
    }

    /// One connection, from opening the feed to losing it. Answers what to say
    /// while waiting to try again.
    async fn connect(&self) -> FeedStatus {
        let feed = api::map::open().await;

        match api::map::features().await {
            Ok(snapshot) => {
                let changes = self
                    .session
                    .borrow_mut()
                    .store
                    .replace(snapshot, Utc::now());
                self.apply(changes);
                self.fit_once();
            }
            Err(err) => return self.lost(&err),
        }

        let mut feed: Feed = match feed {
            Ok(feed) => feed,
            Err(ApiError::Server(refused)) => return FeedStatus::NotLive(refused),
            Err(err) => return self.lost(&err),
        };

        self.session.borrow_mut().feed = Some(feed.canceller());
        self.on_status.emit(FeedStatus::Live);

        while let Some(update) = feed.next().await {
            let changes = match update {
                MapUpdate::Upsert(feature) => {
                    self.session.borrow_mut().store.upsert(*feature, Utc::now())
                }
                MapUpdate::Remove { uid } => self.session.borrow_mut().store.remove(&uid),
                // `Reset`, and anything a newer server says that this build
                // cannot act on: the snapshot is always right.
                _ => break,
            };

            self.apply(changes);
        }

        // Dropping the feed cancels it; the next connection is what finds out
        // why it ended. If the server closed it because the credential stopped
        // being good, that connection is refused and `lost` empties the map.
        FeedStatus::Reconnecting("The live feed ended. Reconnecting.".to_string())
    }

    /// What a failed request means. A refusal is not a failure to retry: the
    /// map is emptied, because what it shows was read under a session the
    /// server no longer honours, and the console is told to look again at who
    /// is signed in.
    fn lost(&self, err: &ApiError) -> FeedStatus {
        if !matches!(err, ApiError::Unauthorized | ApiError::Forbidden) {
            return FeedStatus::Reconnecting(err.to_string());
        }

        let cleared = self.session.borrow_mut().store.clear();
        self.apply(cleared);
        self.on_signed_out.emit(());

        FeedStatus::SignedOut(err.to_string())
    }

    /// Dims and drops what has gone stale, and lets the page redraw when there
    /// is something new to say — or a pop-over open whose ages are ticking.
    async fn keep_time(self) {
        while self.alive.get() {
            TimeoutFuture::new(TICK_MS).await;

            let swept = self.session.borrow_mut().store.sweep(Utc::now());
            self.apply(swept);

            let mut session = self.session.borrow_mut();
            let redraw = session.dirty || session.selected.is_some();
            session.dirty = false;
            drop(session);

            if redraw && self.alive.get() {
                self.on_redraw.emit(());
            }
        }
    }

    fn apply(&self, changes: Changes) {
        if changes.is_empty() {
            return;
        }

        let mut session = self.session.borrow_mut();
        if let Some(map) = &session.map {
            map.apply(&changes.upserts, &changes.removes);
        }
        session.dirty = true;

        let deselect = session
            .selected
            .as_ref()
            .is_some_and(|uid| changes.removes.contains(uid));
        drop(session);

        if deselect {
            self.on_select.emit(None);
        }
    }

    fn fit_once(&self) {
        let mut session = self.session.borrow_mut();
        if !session.fitted && session.store.len() > 0 {
            session.fitted = true;
            session.fit_all();
        }
    }
}
