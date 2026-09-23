//! The part of the session that draws: a sketch while a drawing tool is in
//! hand, and an outline with handles while a drawing that exists is in focus.
//!
//! Only one of them is over the map at a time, and the sketch wins: choosing
//! a tool is saying what the next clicks are for. A finished sketch is
//! published the way a placed marker is — sent to the server, and drawn when
//! the server answers with what it relayed — and then opened, so that the
//! next thing somebody does is name it. A dragged outline is not published
//! until it is saved: see [`Session::outlined`].

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::spawn_local;
use web_sys::KeyboardEvent;
use yew::{Callback, hook, use_effect_with};

use crate::api;

use super::draft::{Draft, PLACED_PREFIX, editable};
use super::focus::{Focus, Pick};
use super::geometry::Geometry;
use super::glue::SketchMode;
use super::session::Session;
use super::sketch::{Outline, Pointer, Sketch};
use super::toolbar::Tool;

impl Session {
    pub fn sketch(&self) -> Option<&Sketch> {
        self.sketch.as_ref()
    }

    /// Whether the outline in focus has been dragged since it was saved.
    pub fn outline_moved(&self) -> bool {
        self.outline.as_ref().is_some_and(|outline| outline.moved)
    }

    /// Starts a sketch for a drawing tool, and drops one for any other.
    pub(super) fn sketch_for(&mut self, tool: Tool) {
        self.sketch = match tool {
            Tool::Draw(form) => Some(Sketch::new(form)),
            _ => None,
        };
        self.reshape();
        self.show_overlay();
    }

    /// Takes back the sketch's last click.
    pub fn undo_sketch(&mut self) {
        if let Some(sketch) = &mut self.sketch {
            sketch.undo();
        }
        self.show_overlay();
    }

    /// Takes what the pointer did. Answers whether the page has something
    /// new to say about it, which is only once a drag has ended.
    pub fn pointer(&mut self, pointer: Pointer) -> bool {
        let settled = match (pointer, &mut self.sketch, &mut self.outline) {
            (Pointer::Hover(at), Some(sketch), _) => {
                sketch.hover(at);
                false
            }
            (Pointer::Drag { index, at, done }, None, Some(outline)) => {
                outline.drag(index, at);
                done
            }
            _ => return false,
        };

        self.show_overlay();
        settled
    }

    /// Puts handles on the drawing in focus, when it is one this console may
    /// reshape and the map is live — and takes them off otherwise. An outline
    /// somebody has dragged is kept until it is saved or left.
    pub(super) fn reshape(&mut self) {
        let live = self
            .replay
            .as_ref()
            .is_none_or(|replay| replay.position.is_live());
        let wanted = self
            .focus
            .feature()
            .filter(|_| live && self.sketch.is_none())
            .and_then(|uid| self.store.get(uid))
            .filter(|feature| editable(feature))
            .and_then(|feature| Outline::of(&feature.uid, Geometry::of(feature)?));

        let dragged = matches!(
            (&self.outline, &wanted),
            (Some(held), Some(wanted)) if held.moved && held.uid == wanted.uid
        );
        if !dragged && self.outline != wanted {
            self.outline = wanted;
            self.show_overlay();
        }
    }

    /// The draft with its outline as the map has it, for a drawing whose
    /// handles have been dragged.
    pub fn outlined(&self, mut draft: Draft) -> Draft {
        if let Some(outline) = self.outline.as_ref().filter(|it| it.uid == draft.uid) {
            draft.geometry = Some(outline.geometry.clone());
        }
        draft
    }

    /// Forgets what was dragged: it has been saved, or is no longer wanted.
    pub(super) fn settle_outline(&mut self) {
        self.outline = None;
        self.reshape();
        self.show_overlay();
    }

    fn show_overlay(&self) {
        let Some(map) = &self.map else {
            return;
        };

        let drawn = match (&self.sketch, &self.outline) {
            (Some(sketch), _) => Some((sketch.draw(), SketchMode::Draw)),
            (None, Some(outline)) => Some((outline.draw(), SketchMode::Edit)),
            (None, None) => None,
        };
        map.show_sketch(drawn.as_ref().map(|(drawn, mode)| (drawn, *mode)));
    }
}

/// A click while a drawing tool is in hand: one more vertex, or the last.
pub fn click(
    session: Rc<RefCell<Session>>,
    pick: &Pick,
    on_focus: Callback<Focus>,
    redraw: Callback<()>,
) {
    let finished = {
        let mut held = session.borrow_mut();
        let finished = held
            .sketch
            .as_mut()
            .and_then(|sketch| sketch.click(pick.at, pick.near));
        held.show_overlay();
        finished
    };

    match finished {
        Some(geometry) => publish(session, geometry, on_focus, redraw),
        None => redraw.emit(()),
    }
}

/// Finishes the sketch as it stands, for whoever has no double click.
pub fn finish(session: Rc<RefCell<Session>>, on_focus: Callback<Focus>, redraw: Callback<()>) {
    let finished = session.borrow().sketch.as_ref().and_then(Sketch::finish);

    if let Some(geometry) = finished {
        publish(session, geometry, on_focus, redraw);
    }
}

/// Publishes a finished drawing, puts it on the map, and opens it. The tool
/// hands back to select either way.
fn publish(
    session: Rc<RefCell<Session>>,
    geometry: Geometry,
    on_focus: Callback<Focus>,
    redraw: Callback<()>,
) {
    let draft = {
        let mut held = session.borrow_mut();
        held.set_tool(Tool::Select);
        held.edit.busy = true;
        held.edit.problem = None;

        let uid = format!("{PLACED_PREFIX}{}", uuid::Uuid::new_v4());
        let made = held.made_here();
        Draft::drawn(uid, geometry, made + 1, held.edit.channels.clone())
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

/// While something is being drawn, the keys a drawing program answers to:
/// Backspace takes back a click, Enter finishes and Escape gives up. Not
/// while somebody is typing, which is what those keys are for there.
#[hook]
pub fn use_keys(drawing: bool, actions: [Callback<()>; 3]) {
    let [undo, finish, cancel] = actions;
    use_effect_with(drawing, move |drawing| {
        let on_key = drawing.then(|| {
            Closure::<dyn Fn(KeyboardEvent)>::new(move |event: KeyboardEvent| {
                let typing = event
                    .target()
                    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                    .is_some_and(|element| {
                        matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
                    });
                let action = match event.key().as_str() {
                    _ if typing => return,
                    "Backspace" => &undo,
                    "Enter" => &finish,
                    "Escape" => &cancel,
                    _ => return,
                };

                event.prevent_default();
                action.emit(());
            })
        });
        if let Some(on_key) = &on_key {
            let _ = gloo_utils::window()
                .add_event_listener_with_callback("keydown", on_key.as_ref().unchecked_ref());
        }

        move || {
            if let Some(on_key) = &on_key {
                let _ = gloo_utils::window().remove_event_listener_with_callback(
                    "keydown",
                    on_key.as_ref().unchecked_ref(),
                );
            }
        }
    });
}
