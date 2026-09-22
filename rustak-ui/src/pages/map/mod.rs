//! The map: everything that is reporting, where it says it is, as it changes.
//!
//! The page is three things side by side. [`session`] is the part that talks
//! to the server and owns what is on the map; [`glue`] is the map itself, in
//! JavaScript; and this file is the Yew around them — the status line, the
//! [`roster`], and the pop-over, which is ordinary Yew portalled into an
//! element the map positions. What the pop-over shows is its [`focus`]: one
//! feature's [`popover`], or the [`chooser`] when a click landed on several.
//! Under one feature's pop-over is its [`track`] — where it has been — and
//! over the bottom of the map the [`playback`] bar that scrubs along it; the
//! [`history`] module is the part of the session that owns both.
//!
//! # Yew is not in the hot path
//!
//! A feed can carry hundreds of updates a second. Each goes from the session
//! straight to the map, which redraws once per frame; this component hears
//! about it at most once a second, which is as fast as a list or a relative
//! time is worth redrawing.
//!
//! # The element the map lives in must never be recreated
//!
//! MapLibre builds its canvas inside a `<div>` Yew made, and Yew knows nothing
//! about what is in there. Yew reconciles un-keyed siblings by position, so a
//! sibling that comes and goes — a note, the pop-over's portal — shifts every
//! position beside it, and a shifted `<div>` is a *new* `<div>`: empty, with
//! the map still drawing into the one that was thrown away. So everything
//! optional on this page sits inside a wrapper that is always there, and the
//! children of `.map-page` and `.map-page__body` never change in number.
//!
//! # Read-only, and built to stop being
//!
//! Nothing here publishes yet. The seams are where that will go: the session
//! already owns the store a local edit would be applied to first, the glue
//! already hands geometry across as GeoJSON, and a publish will need exactly
//! one more piece of page state — the channel or mission it is going to —
//! which belongs in the bar below beside the status.

mod chooser;
mod focus;
mod glue;
mod history;
mod playback;
mod popover;
mod render;
mod roster;
mod session;
mod store;
mod track;

use yew::prelude::*;

use crate::app::AuthHandle;
use crate::components::{Alert, AlertKind, Button, StatusPill, StatusTone};

use chooser::Chooser;
use focus::Focus;
use playback::PlaybackBar;
use popover::Popover;
use roster::{ROSTER_ROWS, Roster, RosterEntry};
use session::{FeedStatus, Listeners, Session};

#[function_component(LiveMap)]
pub fn live_map() -> Html {
    let container = use_node_ref();
    let redraw = use_force_update();
    let status = use_state(|| FeedStatus::Connecting);
    let focus = use_state(Focus::default);
    let search = use_state(String::new);
    let session = use_mut_ref(Session::default);

    let auth = use_context::<AuthHandle>();

    // Re-resolving the session is what turns a refusal into the sign-in
    // prompt: `Protected`, above this page, draws whatever the answer is.
    let on_signed_out = auth.map(|auth| auth.refresh).unwrap_or_default();

    // Redraws this component, for the parts of the session that change on
    // their own time: the feed, a track arriving, a track playing.
    let on_redraw = {
        let redraw = redraw.clone();
        Callback::from(move |()| redraw.force_update())
    };

    {
        let (container, session, status, focus, on_redraw) = (
            container.clone(),
            session.clone(),
            status.clone(),
            focus.clone(),
            on_redraw.clone(),
        );

        use_effect_with((), move |_| {
            let running = session::start(
                session,
                container,
                Listeners {
                    on_status: Callback::from(move |next| status.set(next)),
                    on_focus: Callback::from(move |next| focus.set(next)),
                    on_redraw,
                    on_signed_out,
                },
            );

            move || running.stop()
        });
    }

    // The pop-over follows the focus, and the track follows the pop-over. The
    // status is a dependency because the first thing it reports is that there
    // is now a map to open one on.
    {
        let (session, on_redraw) = (session.clone(), on_redraw.clone());
        use_effect_with(((*focus).clone(), (*status).clone()), move |(focus, _)| {
            history::follow(session, focus.clone(), on_redraw);
            || ()
        });
    }

    // From the roster, which may be naming something off the edge of the view.
    let onselect = {
        let (session, focus) = (session.clone(), focus.clone());
        Callback::from(move |uid: String| {
            session.borrow().fly_to(&uid);
            focus.set(Focus::Feature(uid));
        })
    };
    // From the chooser, which is by construction already looking at it.
    let onchoose = {
        let focus = focus.clone();
        Callback::from(move |uid: String| focus.set(Focus::Feature(uid)))
    };
    let onsearch = {
        let search = search.clone();
        Callback::from(move |value: String| search.set(value))
    };
    let fit = {
        let session = session.clone();
        Callback::from(move |_: MouseEvent| session.borrow().fit_all())
    };

    // The playback bar. Each of these changes the session and then this
    // component, which draws the bar from what the session now says.
    let onseek = {
        let (session, redraw) = (session.clone(), redraw.clone());
        Callback::from(move |offset: i64| {
            session.borrow_mut().seek(offset);
            redraw.force_update();
        })
    };
    let onspeed = {
        let (session, redraw) = (session.clone(), redraw.clone());
        Callback::from(move |speed: u32| {
            session.borrow_mut().set_speed(speed);
            redraw.force_update();
        })
    };
    let onlive = {
        let (session, redraw) = (session.clone(), redraw.clone());
        Callback::from(move |()| {
            session.borrow_mut().go_live();
            redraw.force_update();
        })
    };
    let ontoggle = {
        let (session, on_redraw) = (session.clone(), on_redraw.clone());
        Callback::from(move |()| {
            history::toggle_playing(session.clone(), on_redraw.clone());
            on_redraw.emit(());
        })
    };

    let held = session.borrow();
    let matching = held.store().roster(&search);
    let entries: Vec<RosterEntry> = matching
        .iter()
        .take(ROSTER_ROWS)
        .map(|feature| RosterEntry::of(feature))
        .collect();

    // Rendered into the element the map positions, so the pop-over is the
    // map's to place and Yew's to fill. On a feature, it describes the fix at
    // the moment being shown — which is the live one until somebody scrubs.
    let content = match &*focus {
        Focus::Nothing => None,
        Focus::Feature(uid) => held
            .displayed(uid)
            .map(|feature| html! { <Popover feature={feature.clone()} /> }),
        Focus::Choosing { uids, .. } => {
            let entries: Vec<RosterEntry> = uids
                .iter()
                .filter_map(|uid| held.store().get(uid))
                .map(RosterEntry::of)
                .collect();

            Some(html! { <Chooser {entries} onselect={onchoose} /> })
        }
    };
    let popover = content
        .zip(held.popover_element())
        .map(|(content, host)| create_portal(content, host));

    let playback = held.replay().map(|replay| {
        html! {
            <PlaybackBar
                name={replay.track.name()}
                fixes={replay.track.len()}
                position={replay.position.clone()}
                {onseek}
                {ontoggle}
                {onspeed}
                {onlive}
            />
        }
    });
    let shown = history::shown_note(&held);

    let (tone, label, explanation) = status.describe();

    html! {
        <div class="map-page">
            <div class="map-page__bar">
                <StatusPill {tone} {label} title={explanation.clone()} />
                <span class="map-page__count">
                    { match held.store().len() {
                        1 => "1 thing on the map".to_string(),
                        count => format!("{count} things on the map"),
                    } }
                </span>
                <Button small=true onclick={fit}>{ "Fit everything" }</Button>
            </div>

            <div class="map-page__notes">
                if let FeedStatus::Failed(message) = &*status {
                    <Alert
                        kind={AlertKind::Error}
                        title="The map could not be started."
                        message={message.clone()}
                    />
                } else if tone != StatusTone::Ok {
                    if let Some(explanation) = explanation {
                        <p class="map-page__note">{ explanation }</p>
                    }
                }
                if let Some(shown) = shown {
                    <p class="map-page__note map-page__note--shown">{ shown }</p>
                }
            </div>

            <div class="map-page__body">
                // The playback bar is laid over the map, so the two share a
                // wrapper; its own wrapper is always there, for the reason
                // above.
                <div class="map-page__stage">
                    <div
                        ref={container}
                        class="map-page__canvas"
                        role="application"
                        aria-label="Map. Everything on it is also listed beside it."
                    />
                    <div class="map-page__playback">{ for playback }</div>
                </div>
                <Roster
                    {entries}
                    matched={matching.len()}
                    search={(*search).clone()}
                    {onsearch}
                    selected={focus.feature().map(str::to_owned)}
                    {onselect}
                />
            </div>

            <div class="map-page__portal">{ for popover }</div>
        </div>
    }
}
