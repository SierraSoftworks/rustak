//! The scrub bar: a track played back like a recording.
//!
//! Sits over the bottom of the map while a track is shown. Dragging it back
//! from the end takes the map to that moment: the marker goes to where the
//! thing was, the line shows where it had been, and everything else leaves
//! the map, because nothing else's past has been read. "Live" brings it all
//! back. [`Position`] is the arithmetic, without a browser; [`PlaybackBar`] is
//! the Yew around it.
//!
//! The bar is also a picture of *when there is anything to see*. A thing comes
//! and goes from the tracking area, and the stretches it was tracked over are
//! drawn along the bar as solid bands with the gaps left hatched, so an
//! operator sees at a glance when it was there. The moment under the pointer
//! is said in a tooltip over the bar, as a media player says a timestamp,
//! which leaves the whole width to the bar.

use chrono::{DateTime, Duration, Utc};
use web_sys::{HtmlElement, HtmlInputElement, HtmlSelectElement};
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

/// A stretch of the scrub bar over which the thing was tracked, in percent
/// of the bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Band {
    pub left: f64,
    pub width: f64,
}

/// Where the track's windows fall along the bar.
pub fn bands(windows: &[(DateTime<Utc>, DateTime<Utc>)], position: &Position) -> Vec<Band> {
    let span = position.span_ms();
    if span == 0 {
        // One moment is all there is, and it was tracked.
        return vec![Band {
            left: 0.0,
            width: 100.0,
        }];
    }

    let percent = |when: DateTime<Utc>| {
        (when - position.start).num_milliseconds().clamp(0, span) as f64 / span as f64 * 100.0
    };

    windows
        .iter()
        .map(|(from, to)| Band {
            left: percent(*from),
            width: percent(*to) - percent(*from),
        })
        .collect()
}

/// Whether the thing was being tracked at `when`.
pub fn tracked(windows: &[(DateTime<Utc>, DateTime<Utc>)], when: DateTime<Utc>) -> bool {
    windows
        .iter()
        .any(|(from, to)| (*from..=*to).contains(&when))
}

/// How wide the slider's thumb is, in CSS pixels: `.map-playback__scrub`'s.
/// The thumb's centre, which is what a value is, travels between half of this
/// in from either end.
const THUMB_PX: f64 = 16.0;

#[derive(Properties, PartialEq)]
pub struct PlaybackBarProps {
    /// What the track is of.
    pub name: AttrValue,
    /// How many fixes it has.
    pub fixes: usize,
    /// When it was being tracked: see [`Track::windows`](super::track::Track::windows).
    pub windows: Vec<(DateTime<Utc>, DateTime<Utc>)>,
    pub position: Position,

    /// The slider moved, to this many milliseconds into the track.
    pub onseek: Callback<i64>,
    /// Play, or pause.
    pub ontoggle: Callback<()>,
    pub onspeed: Callback<u32>,
    /// Back to the live map.
    pub onlive: Callback<()>,
}

/// A moment, as the bar says it: the date, because a track can be a day long.
fn stamp(when: DateTime<Utc>) -> String {
    when.format("%Y-%m-%d %H:%M:%SZ").to_string()
}

#[function_component(PlaybackBar)]
pub fn playback_bar(props: &PlaybackBarProps) -> Html {
    let position = &props.position;

    // Where the pointer is over the bar, in pixels from its left, and the
    // moment that is: what a media player shows over its own.
    let pointed = use_state(|| None::<(f64, DateTime<Utc>)>);

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

    // The pointer is over the slider itself — nothing else in the wrapper
    // takes pointer events — so its offset is an offset along the bar.
    let onpoint = {
        let (pointed, position) = (pointed.clone(), position.clone());
        Callback::from(move |event: PointerEvent| {
            let Some(width) = event
                .target_dyn_into::<HtmlElement>()
                .map(|slider| f64::from(slider.offset_width()))
                .filter(|width| *width > THUMB_PX)
            else {
                return;
            };

            let x = f64::from(event.offset_x()).clamp(0.0, width);
            let along = ((x - THUMB_PX / 2.0) / (width - THUMB_PX)).clamp(0.0, 1.0);
            let moment = position.moment((along * position.span_ms() as f64).round() as i64);

            pointed.set(Some((x, moment)));
        })
    };
    let onleave = {
        let pointed = pointed.clone();
        Callback::from(move |_: PointerEvent| pointed.set(None))
    };
    // A finger that lifts has left; a mouse that lets go is still there.
    let onlift = {
        let pointed = pointed.clone();
        Callback::from(move |event: PointerEvent| {
            if event.pointer_type() != "mouse" {
                pointed.set(None);
            }
        })
    };

    let gaps = props.windows.len().saturating_sub(1);
    let coverage = match (props.windows.len(), gaps) {
        (0 | 1, _) => "Tracked throughout".to_string(),
        (periods, 1) => format!("Tracked for {periods} periods, with 1 gap"),
        (periods, gaps) => format!("Tracked for {periods} periods, with {gaps} gaps"),
    };
    let summary = match (props.fixes, gaps) {
        (1, _) => "1 fix".to_string(),
        (fixes, gaps) => format!(
            "{fixes} fixes over {}{}",
            short_duration((position.end - position.start).num_seconds()),
            match gaps {
                0 => String::new(),
                1 => " · 1 gap".to_string(),
                gaps => format!(" · {gaps} gaps"),
            }
        ),
    };
    // For whoever cannot hover: the slider says its moment, not its number.
    let value_text = match position.at {
        Some(at) if tracked(&props.windows, at) => stamp(at),
        Some(at) => format!("{}, no data", stamp(at)),
        None => "Live".to_string(),
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
            <div
                class="map-playback__track"
                onpointermove={onpoint.clone()}
                onpointerdown={onpoint}
                onpointerleave={onleave.clone()}
                onpointercancel={onleave}
                onpointerup={onlift}
            >
                <div class="map-playback__bands" role="img" aria-label={coverage}>
                    { for bands(&props.windows, position).into_iter().map(|band| html! {
                        <span
                            class="map-playback__band"
                            style={format!("left: {:.3}%; width: {:.3}%", band.left, band.width)}
                        />
                    }) }
                </div>
                <input
                    type="range"
                    class="map-playback__scrub"
                    aria-label="Position in the track"
                    aria-valuetext={value_text}
                    min="0"
                    max={position.span_ms().to_string()}
                    step="1"
                    value={position.offset_ms().to_string()}
                    oninput={onseek}
                />
                if let Some((x, moment)) = *pointed {
                    <div
                        class="map-playback__tip"
                        role="tooltip"
                        style={format!("left: clamp(4.5rem, {x:.0}px, calc(100% - 4.5rem))")}
                    >
                        <span>{ stamp(moment) }</span>
                        <span>
                            { if tracked(&props.windows, moment) {
                                short_relative(moment)
                            } else {
                                "no data".to_string()
                            } }
                        </span>
                    </div>
                }
            </div>
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

    #[test]
    fn the_bar_is_banded_where_the_thing_was_tracked() {
        let position = Position::live(at("12:00:00"), at("12:10:00"));
        let windows = [
            (at("12:00:00"), at("12:02:30")),
            (at("12:07:30"), at("12:10:00")),
        ];

        assert_eq!(
            bands(&windows, &position),
            [
                Band {
                    left: 0.0,
                    width: 25.0
                },
                Band {
                    left: 75.0,
                    width: 25.0
                },
            ]
        );
        assert!(tracked(&windows, at("12:02:30")));
        assert!(!tracked(&windows, at("12:05:00")));

        // A track of one moment is all band.
        let single = Position::live(at("12:00:00"), at("12:00:00"));
        assert_eq!(
            bands(&[(at("12:00:00"), at("12:00:00"))], &single),
            [Band {
                left: 0.0,
                width: 100.0
            }]
        );
    }
}
