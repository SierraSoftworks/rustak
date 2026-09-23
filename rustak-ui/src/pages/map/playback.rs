//! The scrub bar: a track played back like a recording.
//!
//! Sits over the bottom of the map while a track is shown. Dragging it back
//! from the end takes the map to that moment: the marker goes to where the
//! thing was, the line shows where it had been, and everything else leaves
//! the map, because nothing else's past has been read. "Live" brings it all
//! back. [`Position`] is the arithmetic, without a browser; [`PlaybackBar`] is
//! the Yew around it.

use chrono::{DateTime, Duration, Utc};
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

use crate::util::{short_duration, short_relative};

/// How many times faster than life a track may be played.
pub const SPEEDS: [u32; 4] = [1, 10, 60, 600];

/// Where playback of a track is.
#[derive(Clone, Debug, PartialEq)]
pub struct Position {
    /// The first fix.
    pub start: DateTime<Utc>,
    /// The last fix, which is also where "live" sits.
    pub end: DateTime<Utc>,
    /// The moment being shown, or [`None`] for the live map.
    pub at: Option<DateTime<Utc>>,
    pub playing: bool,
    pub speed: u32,
}

impl Position {
    /// Live, over `start..=end`.
    pub fn live(start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        Self {
            start,
            end: end.max(start),
            at: None,
            playing: false,
            speed: SPEEDS[0],
        }
    }

    pub fn is_live(&self) -> bool {
        self.at.is_none()
    }

    /// How long the track is, in the slider's units.
    pub fn span_ms(&self) -> i64 {
        (self.end - self.start).num_milliseconds().max(0)
    }

    /// Where the slider sits: at the end when live.
    pub fn offset_ms(&self) -> i64 {
        match self.at {
            Some(at) => (at - self.start)
                .num_milliseconds()
                .clamp(0, self.span_ms()),
            None => self.span_ms(),
        }
    }

    /// The moment `offset` milliseconds into the track.
    pub fn moment(&self, offset_ms: i64) -> DateTime<Utc> {
        self.start + Duration::milliseconds(offset_ms.clamp(0, self.span_ms()))
    }

    /// Where playback is after `elapsed_ms` of wall-clock at this speed, or
    /// [`None`] once that is past the end.
    pub fn advanced(&self, elapsed_ms: i64) -> Option<DateTime<Utc>> {
        let from = self.at.unwrap_or(self.start);
        let to = from + Duration::milliseconds(elapsed_ms.saturating_mul(i64::from(self.speed)));

        (to < self.end).then_some(to)
    }
}

#[derive(Properties, PartialEq)]
pub struct PlaybackBarProps {
    /// What the track is of.
    pub name: AttrValue,
    /// How many fixes it has.
    pub fixes: usize,
    pub position: Position,

    /// The slider moved, to this many milliseconds into the track.
    pub onseek: Callback<i64>,
    /// Play, or pause.
    pub ontoggle: Callback<()>,
    pub onspeed: Callback<u32>,
    /// Back to the live map.
    pub onlive: Callback<()>,
}

#[function_component(PlaybackBar)]
pub fn playback_bar(props: &PlaybackBarProps) -> Html {
    let position = &props.position;

    let onseek = {
        let onseek = props.onseek.clone();
        Callback::from(move |event: InputEvent| {
            if let Some(offset) = event
                .target_dyn_into::<HtmlInputElement>()
                .and_then(|input| input.value().parse::<i64>().ok())
            {
                onseek.emit(offset);
            }
        })
    };
    let onspeed = {
        let onspeed = props.onspeed.clone();
        Callback::from(move |event: Event| {
            if let Some(speed) = event
                .target_dyn_into::<HtmlSelectElement>()
                .and_then(|select| select.value().parse::<u32>().ok())
            {
                onspeed.emit(speed);
            }
        })
    };
    let ontoggle = {
        let ontoggle = props.ontoggle.clone();
        Callback::from(move |_: MouseEvent| ontoggle.emit(()))
    };
    let onlive = {
        let onlive = props.onlive.clone();
        Callback::from(move |_: MouseEvent| onlive.emit(()))
    };

    let moment = match position.at {
        Some(at) => format!("{} · {}", at.format("%H:%M:%SZ"), short_relative(at)),
        None => "Live".to_string(),
    };
    let summary = match props.fixes {
        1 => "1 fix".to_string(),
        fixes => format!(
            "{fixes} fixes over {}",
            short_duration((position.end - position.start).num_seconds())
        ),
    };

    html! {
        <div class="map-playback" role="region" aria-label="Track playback">
            <button
                type="button"
                class="map-playback__button"
                aria-label={if position.playing { "Pause" } else { "Play" }}
                title={format!("Play back where {} has been", props.name)}
                onclick={ontoggle}
            >
                { if position.playing { "⏸" } else { "▶" } }
            </button>
            <input
                type="range"
                class="map-playback__scrub"
                aria-label="Position in the track"
                min="0"
                max={position.span_ms().to_string()}
                step="1"
                value={position.offset_ms().to_string()}
                oninput={onseek}
            />
            <span class="map-playback__time">{ moment }</span>
            <select
                class="map-playback__speed"
                aria-label="Playback speed"
                value={position.speed.to_string()}
                onchange={onspeed}
            >
                { for SPEEDS.iter().map(|speed| html! {
                    <option
                        value={speed.to_string()}
                        selected={*speed == position.speed}
                    >
                        { format!("{speed}×") }
                    </option>
                }) }
            </select>
            <button
                type="button"
                class="map-playback__button map-playback__button--live"
                disabled={position.is_live()}
                onclick={onlive}
            >
                { "Live" }
            </button>
            <span class="map-playback__summary">{ summary }</span>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(time: &str) -> DateTime<Utc> {
        format!("2026-09-18T{time}Z").parse().unwrap()
    }

    fn position() -> Position {
        Position::live(at("12:00:00"), at("12:10:00"))
    }

    #[test]
    fn live_sits_at_the_end_and_a_moment_sits_where_it_is() {
        let live = position();
        assert!(live.is_live());
        assert_eq!(live.span_ms(), 600_000);
        assert_eq!(live.offset_ms(), 600_000);

        let shown = Position {
            at: Some(at("12:02:30")),
            ..position()
        };
        assert_eq!(shown.offset_ms(), 150_000);
        assert_eq!(shown.moment(150_000), at("12:02:30"));
        assert_eq!(shown.moment(-5), at("12:00:00"));
        assert_eq!(shown.moment(10_000_000), at("12:10:00"));
    }

    #[test]
    fn playing_advances_at_the_chosen_speed_and_ends_at_the_end() {
        let paused = Position {
            at: Some(at("12:09:00")),
            speed: 60,
            ..position()
        };

        assert_eq!(paused.advanced(500), Some(at("12:09:30")));
        assert_eq!(paused.advanced(1_000), None);
        // From live, playing starts at the beginning.
        assert_eq!(position().advanced(1_000), Some(at("12:00:01")));
    }

    #[test]
    fn a_track_of_one_fix_has_no_length_and_nowhere_to_go() {
        let single = Position::live(at("12:00:00"), at("11:00:00"));

        assert_eq!(single.span_ms(), 0);
        assert_eq!(single.advanced(1_000), None);
    }
}
