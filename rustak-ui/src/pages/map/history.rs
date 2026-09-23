//! The part of the session that looks back: the track under whatever the
//! pop-over is open on, and the playback of it.
//!
//! Opening the pop-over on something asks the server where it has been, and
//! the answer is drawn under its marker as a line. The line follows the feed
//! while the map is live. Scrubbing back along it is a different map: the one
//! thing, where it was then, and nothing else — nothing else's past has been
//! read, and a marker drawn where something is *now* beside one drawn where
//! something *was* would be a lie about both. Closing the pop-over takes the
//! track away and returns the live map.

use std::cell::RefCell;
use std::rc::Rc;

use gloo_timers::future::TimeoutFuture;
use rustak_api::MapFeature;
use wasm_bindgen_futures::spawn_local;
use yew::Callback;

use crate::api;

use super::focus::Focus;
use super::playback::Position;
use super::render;
use super::session::Session;
use super::track::Track;

/// How far back a track reaches. The server caps how many fixes it answers
/// with and keeps the newest, so this is the most a quiet device shows.
pub const HISTORY_SECAGO: i64 = 24 * 60 * 60;

/// How often a playing track moves on.
const STEP_MS: u32 = 200;

/// A track, and where playback of it is.
pub struct Replay {
    pub track: Track,
    pub position: Position,
    /// Bumped whenever playback starts or stops, so that the loop driving an
    /// earlier start finds out it has been superseded.
    generation: u64,
}

impl Session {
    pub fn replay(&self) -> Option<&Replay> {
        self.replay.as_ref()
    }

    /// The feature the pop-over should describe: the fix at the moment being
    /// shown, or the live one.
    pub fn displayed(&self, uid: &str) -> Option<&MapFeature> {
        match self
            .replay
            .as_ref()
            .filter(|replay| replay.track.uid() == uid)
        {
            Some(replay) => match replay.position.at {
                Some(at) => replay.track.at(at),
                None => self.store.get(uid),
            },
            None => self.store.get(uid),
        }
    }

    /// Shows the moment `offset_ms` into the track.
    pub fn seek(&mut self, offset_ms: i64) {
        if let Some(replay) = &mut self.replay {
            replay.position.at = Some(replay.position.moment(offset_ms));
        }
        self.draw_history();
    }

    /// Back to the live map, with the track still under it.
    pub fn go_live(&mut self) {
        if let Some(replay) = &mut self.replay {
            replay.position.at = None;
            replay.position.playing = false;
            replay.generation += 1;
        }
        self.draw_history();
    }

    pub fn set_speed(&mut self, speed: u32) {
        if let Some(replay) = &mut self.replay {
            replay.position.speed = speed;
        }
    }

    /// Lets the track follow the feed.
    pub fn extend_track(&mut self, feature: &MapFeature) {
        let Some(replay) = &mut self.replay else {
            return;
        };

        if replay.track.extend(feature) {
            replay.position.end = feature.time;
            self.draw_history();
        }
    }

    /// Takes the track the server answered with. What the feed said about
    /// the same thing while the answer was on its way is in the store and
    /// not in the answer, so the latest of it is added before the track is
    /// drawn.
    fn set_track(&mut self, mut track: Track) {
        if let Some(latest) = self.store.get(track.uid()) {
            track.extend(latest);
        }

        self.replay = track.span().map(|(start, end)| Replay {
            position: Position::live(start, end),
            track,
            generation: 0,
        });
        self.draw_history();
    }

    fn clear_track(&mut self) {
        if self.replay.take().is_some() {
            self.draw_history();
        }
    }

    /// Draws the track and, when a moment is being shown, freezes the map on
    /// that moment.
    fn draw_history(&self) {
        let Some(map) = &self.map else {
            return;
        };
        let Some(replay) = &self.replay else {
            map.show_track(None);
            map.freeze(None);
            return;
        };

        let at = replay.position.at;
        let shown = at.and_then(|at| replay.track.at(at));
        let color = shown
            .or_else(|| self.store.get(replay.track.uid()))
            .map_or(render::MARKER, render::track_color);

        map.show_track(Some(&replay.track.draw(at, color)));
        // Staleness is judged at the moment shown, not now: everything in a
        // track is stale by now.
        map.freeze(
            shown
                .zip(at)
                .map(|(fix, at)| vec![render::draw(fix, at)])
                .as_deref(),
        );
    }
}

/// Puts the pop-over on `focus`, and the track under it when it is on one
/// feature. Asking is skipped when the track is already that feature's, so
/// this may be called as often as the page likes.
pub fn follow(session: Rc<RefCell<Session>>, focus: Focus, redraw: Callback<()>) {
    let wanted = focus.feature().map(str::to_owned);

    {
        let mut held = session.borrow_mut();
        let already = held
            .replay
            .as_ref()
            .is_some_and(|replay| Some(replay.track.uid()) == wanted.as_deref());

        // Before the pop-over moves: a frozen map has nothing else on it to
        // move the pop-over to.
        if !already {
            held.clear_track();
        }
        held.focus(focus);

        if already || wanted.is_none() || held.map.is_none() {
            return;
        }
    }

    let Some(uid) = wanted else {
        return;
    };

    spawn_local(async move {
        let fetched = api::map::history(&uid, HISTORY_SECAGO).await;

        let mut held = session.borrow_mut();
        if held.focus.feature() != Some(uid.as_str()) || held.map.is_none() {
            return;
        }

        match fetched {
            Ok(fixes) => {
                held.set_track(Track::new(uid, fixes));
                drop(held);
                redraw.emit(());
            }
            Err(err) => log::warn!("Could not read where {uid} has been: {err}"),
        }
    });
}

/// Plays the track, or pauses it. Playing from the live map, or from the end,
/// starts at the beginning.
pub fn toggle_playing(session: Rc<RefCell<Session>>, redraw: Callback<()>) {
    let generation = {
        let mut held = session.borrow_mut();
        let Some(replay) = &mut held.replay else {
            return;
        };

        replay.generation += 1;
        if replay.position.playing {
            replay.position.playing = false;
            return;
        }

        if replay
            .position
            .at
            .is_none_or(|at| at >= replay.position.end)
        {
            replay.position.at = Some(replay.position.start);
        }
        replay.position.playing = true;
        let generation = replay.generation;

        held.draw_history();
        generation
    };

    spawn_local(async move {
        loop {
            TimeoutFuture::new(STEP_MS).await;

            let mut held = session.borrow_mut();
            if held.map.is_none() {
                return;
            }
            let Some(replay) = &mut held.replay else {
                return;
            };
            if replay.generation != generation || !replay.position.playing {
                return;
            }

            match replay.position.advanced(i64::from(STEP_MS)) {
                Some(at) => replay.position.at = Some(at),
                None => {
                    replay.position.at = Some(replay.position.end);
                    replay.position.playing = false;
                }
            }

            held.draw_history();
            drop(held);
            redraw.emit(());
        }
    });
}

/// What to say while a moment is being shown, rather than the live map.
pub fn shown_note(session: &Session) -> Option<String> {
    let replay = session.replay()?;
    let at = replay.position.at?;

    Some(format!(
        "Showing where {} was at {}. Everything else is hidden until you return to live.",
        replay.track.name(),
        at.format("%H:%M:%SZ"),
    ))
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, Utc};
    use rustak_api::MapPoint;

    use super::*;

    fn at(time: &str) -> DateTime<Utc> {
        format!("2026-09-18T{time}Z").parse().unwrap()
    }

    fn fix(time: &str, lon: f64) -> MapFeature {
        MapFeature {
            uid: "A".to_string(),
            kind: "a-f-G-U-C".to_string(),
            how: None,
            callsign: Some("QUINN".to_string()),
            team: None,
            role: None,
            time: at(time),
            stale: at(time) + Duration::minutes(2),
            received_at: at(time),
            point: MapPoint {
                lat: 51.5,
                lon,
                hae: None,
                ce: None,
                le: None,
            },
            shape: None,
            course: None,
            speed: None,
            battery: None,
            remarks: None,
            software: None,
            sidc: None,
            groups: Vec::new(),
        }
    }

    #[test]
    fn a_fix_the_feed_brought_while_the_history_was_read_is_not_lost() {
        let mut session = Session::default();
        session.store.upsert(fix("12:02:00", -0.12), at("12:02:01"));

        session.set_track(Track::new("A", vec![fix("12:00:00", -0.10)]));

        let replay = session.replay().expect("a track");
        assert_eq!(replay.track.len(), 2);
        assert_eq!(replay.position.end, at("12:02:00"));
        assert_eq!(
            session.displayed("A").map(|fix| fix.point.lon),
            Some(-0.12),
            "live shows the latest"
        );
    }
}
