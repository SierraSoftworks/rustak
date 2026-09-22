//! The part of the session that writes: placing a marker, saving what was
//! typed about it, and deleting it.
//!
//! Every write goes to the server and comes back as the feature the server
//! relayed — the same bytes every device and every other map got — and only
//! then is it put on this map. So this map never shows a marker the server
//! did not accept, and a marker it does show is exactly what everybody else
//! sees. What the feed then echoes is the same feature again, and the store
//! keeps the newer, which is a no-op.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::Utc;
use rustak_api::{Direction, MapFeature, Username};
use wasm_bindgen_futures::spawn_local;
use yew::Callback;

use crate::api;

use super::draft::Draft;
use super::focus::Focus;
use super::session::Session;
use super::toolbar::Tool;

/// What the uid of a marker placed here begins with, so that the next one can
/// be numbered after the ones before it.
const PLACED_PREFIX: &str = "rustak-console-";

/// What the properties panel needs to know about writing.
#[derive(Debug, Default)]
pub struct Edit {
    /// A write is in flight.
    pub busy: bool,
    /// Why the last write did not happen.
    pub problem: Option<String>,
    /// The channels the signed-in account may publish into.
    pub channels: Vec<String>,
}

impl Session {
    pub fn tool(&self) -> Tool {
        self.tool
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        if let Some(map) = &self.map {
            map.set_cursor(match tool {
                Tool::Select => "",
                Tool::Pin => "crosshair",
            });
        }
    }

    pub fn edit(&self) -> &Edit {
        &self.edit
    }

    /// Puts a feature the server has just relayed on the map.
    fn take(&mut self, feature: MapFeature) {
        let changes = self.store.upsert(feature, Utc::now());
        if let Some(map) = &self.map {
            map.apply(&changes.upserts, &changes.removes);
        }
        self.dirty = true;
    }

    fn drop_feature(&mut self, uid: &str) {
        let changes = self.store.remove(uid);
        if let Some(map) = &self.map {
            map.apply(&changes.upserts, &changes.removes);
        }
        self.dirty = true;
    }
}

/// Places a marker at `[lon, lat]`: publishes it, puts it on the map, and
/// opens it for editing. The pin tool hands back to select either way.
pub fn place(
    session: Rc<RefCell<Session>>,
    at: [f64; 2],
    on_focus: Callback<Focus>,
    redraw: Callback<()>,
) {
    let draft = {
        let mut held = session.borrow_mut();
        held.set_tool(Tool::Select);
        held.edit.busy = true;
        held.edit.problem = None;

        let uid = format!("{PLACED_PREFIX}{}", uuid::Uuid::new_v4());
        let placed = held
            .store
            .roster("")
            .iter()
            .filter(|feature| feature.uid.starts_with(PLACED_PREFIX))
            .count();
        Draft::placed(uid, at, placed + 1, held.edit.channels.clone())
    };
    redraw.emit(());

    spawn_local(async move {
        let published = match draft.publish() {
            Ok(feature) => api::map::publish(&draft.uid, &feature).await,
            Err(problem) => Err(api::ApiError::Server(problem)),
        };

        let mut held = session.borrow_mut();
        held.edit.busy = false;
        match published {
            Ok(feature) => {
                held.take(feature);
                drop(held);
                on_focus.emit(Focus::Feature(draft.uid));
            }
            Err(err) => {
                held.edit.problem = Some(err.to_string());
                drop(held);
            }
        }
        redraw.emit(());
    });
}

/// Saves what was typed about a marker.
pub fn save(session: Rc<RefCell<Session>>, draft: Draft, redraw: Callback<()>) {
    let feature = match draft.publish() {
        Ok(feature) => feature,
        Err(problem) => {
            session.borrow_mut().edit.problem = Some(problem);
            return redraw.emit(());
        }
    };

    {
        let mut held = session.borrow_mut();
        held.edit.busy = true;
        held.edit.problem = None;
    }
    redraw.emit(());

    spawn_local(async move {
        let published = api::map::publish(&draft.uid, &feature).await;

        let mut held = session.borrow_mut();
        held.edit.busy = false;
        match published {
            Ok(feature) => held.take(feature),
            Err(err) => held.edit.problem = Some(err.to_string()),
        }
        drop(held);
        redraw.emit(());
    });
}

/// Deletes a marker from every map and device, and closes it here.
pub fn delete(
    session: Rc<RefCell<Session>>,
    uid: String,
    on_focus: Callback<Focus>,
    redraw: Callback<()>,
) {
    {
        let mut held = session.borrow_mut();
        held.edit.busy = true;
        held.edit.problem = None;
    }
    redraw.emit(());

    spawn_local(async move {
        let removed = api::map::remove(&uid).await;

        let mut held = session.borrow_mut();
        held.edit.busy = false;
        match removed {
            Ok(()) => {
                held.drop_feature(&uid);
                drop(held);
                on_focus.emit(Focus::Nothing);
            }
            Err(err) => {
                held.edit.problem = Some(err.to_string());
                drop(held);
            }
        }
        redraw.emit(());
    });
}

/// Reads which channels the signed-in account may publish into, once.
pub fn load_channels(session: Rc<RefCell<Session>>, username: Username, redraw: Callback<()>) {
    spawn_local(async move {
        let Ok(memberships) = api::groups::memberships(&username).await else {
            return;
        };

        let mut channels: Vec<String> = memberships
            .iter()
            .filter(|membership| matches!(membership.direction, Direction::In | Direction::Both))
            .map(|membership| membership.group.as_str().to_string())
            .collect();
        channels.sort();
        channels.dedup();

        session.borrow_mut().edit.channels = channels;
        redraw.emit(());
    });
}
