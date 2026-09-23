//! The map: everything that is reporting, where it says it is, as it changes
//! — and, now, what somebody puts on it.
//!
//! The page is the map with things laid over it. [`session`] is the part that
//! talks to the server and owns what is on the map; [`glue`] is the map
//! itself, in JavaScript; and this file is the Yew around them: the
//! [`toolbar`] over the top, the [`objects`] list over one corner, the
//! [`properties`] panel over the other, and the [`playback`] bar over the
//! foot. What the panel shows is the [`focus`]: one feature, with its
//! [`track`] under it and its [`draft`] in the panel when it may be edited.
//! A click that landed on several is the [`chooser`], which is ordinary Yew
//! portalled into an element the map positions, because it belongs where the
//! click was. The [`history`] and [`editing`] modules are the parts of the
//! session that read back and write.
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
//! sibling that comes and goes — a note, a panel — shifts every position
//! beside it, and a shifted `<div>` is a *new* `<div>`: empty, with the map
//! still drawing into the one that was thrown away. So every overlay sits in
//! a wrapper that is always there, and the children of `.map-page` never
//! change in number.
//!
//! # The overlays are one grid
//!
//! The toolbar, the notes, the list, the panel and the bar are cells of one
//! grid laid over the map, so that none of them can be drawn over another
//! whatever the width: three columns where there is room, one stack where
//! there is not. Which it is depends on the width of the *map*, not of the
//! window — the navigation beside it comes and goes — so the stylesheet asks
//! the container rather than the screen.

mod chooser;
mod draft;
mod editing;
mod facts;
mod focus;
mod glue;
mod history;
mod objects;
mod playback;
mod properties;
mod render;
mod roster;
mod session;
mod store;
mod symbols;
mod toolbar;
mod track;

use yew::prelude::*;

use crate::app::AuthHandle;
use crate::components::{Alert, AlertKind, StatusTone};

use chooser::Chooser;
use focus::{Focus, Pick};
use objects::ObjectList;
use playback::PlaybackBar;
use properties::Properties;
use roster::{ROSTER_ROWS, RosterEntry};
use session::{FeedStatus, Listeners, Session};
use toolbar::{Tool, Toolbar};

#[function_component(LiveMap)]
pub fn live_map() -> Html {
    let container = use_node_ref();
    let redraw = use_force_update();
    let status = use_state(|| FeedStatus::Connecting);
    let focus = use_state(Focus::default);
    let search = use_state(String::new);
    let session = use_mut_ref(Session::default);

    let auth = use_context::<AuthHandle>();
    let username = auth
        .as_ref()
        .and_then(|auth| auth.user.as_ref())
        .map(|me| me.username.clone());

    // Re-resolving the session is what turns a refusal into the sign-in
    // prompt: `Protected`, above this page, draws whatever the answer is.
    let on_signed_out = auth.map(|auth| auth.refresh).unwrap_or_default();

    // Redraws this component, for the parts of the session that change on
    // their own time: the feed, a track arriving, a write coming back.
    let on_redraw = {
        let redraw = redraw.clone();
        Callback::from(move |()| redraw.force_update())
    };
    let on_focus = {
        let focus = focus.clone();
        Callback::from(move |next: Focus| focus.set(next))
    };

    // A click on the map: what the tool says it is.
    let on_pick = {
        let (session, on_focus, on_redraw) = (session.clone(), on_focus.clone(), on_redraw.clone());
        Callback::from(move |pick: Pick| {
            if session.borrow().tool() == Tool::Pin {
                editing::place(
                    session.clone(),
                    pick.at,
                    on_focus.clone(),
                    on_redraw.clone(),
                );
            } else {
                on_focus.emit(pick.into());
            }
        })
    };

    {
        let (container, session, status, on_focus, on_redraw) = (
            container.clone(),
            session.clone(),
            status.clone(),
            on_focus.clone(),
            on_redraw.clone(),
        );

        use_effect_with((), move |_| {
            let running = session::start(
                session,
                container,
                Listeners {
                    on_status: Callback::from(move |next| status.set(next)),
                    on_pick,
                    on_focus,
                    on_redraw,
                    on_signed_out,
                },
            );

            move || running.stop()
        });
    }

    // Which channels a placed marker may go into, once the account is known.
    {
        let (session, on_redraw) = (session.clone(), on_redraw.clone());
        use_effect_with(username, move |username| {
            if let Some(username) = username.clone() {
                editing::load_channels(session, username, on_redraw);
            }
            || ()
        });
    }

    // The panel follows the focus, and the track follows the panel. The
    // status is a dependency because the first thing it reports is that there
    // is now a map to open one on.
    {
        let (session, on_redraw) = (session.clone(), on_redraw.clone());
        use_effect_with(((*focus).clone(), (*status).clone()), move |(focus, _)| {
            history::follow(session, focus.clone(), on_redraw);
            || ()
        });
    }

    let onselect = {
        let (session, focus) = (session.clone(), focus.clone());
        Callback::from(move |uid: String| {
            session.borrow().fly_to(&uid);
            focus.set(Focus::Feature(uid));
        })
    };
    let onchoose = {
        let focus = focus.clone();
        Callback::from(move |uid: String| focus.set(Focus::Feature(uid)))
    };
    let onclose = {
        let focus = focus.clone();
        Callback::from(move |()| focus.set(Focus::Nothing))
    };
    let onsearch = {
        let search = search.clone();
        Callback::from(move |value: String| search.set(value))
    };
    let onfit = {
        let session = session.clone();
        Callback::from(move |()| session.borrow().fit_all())
    };
    let ontool = {
        let (session, redraw) = (session.clone(), redraw.clone());
        Callback::from(move |tool: Tool| {
            session.borrow_mut().set_tool(tool);
            redraw.force_update();
        })
    };
    let onsave = {
        let (session, on_redraw) = (session.clone(), on_redraw.clone());
        Callback::from(move |draft| editing::save(session.clone(), draft, on_redraw.clone()))
    };
    let ondelete = {
        let (session, on_focus, on_redraw) = (session.clone(), on_focus.clone(), on_redraw.clone());
        Callback::from(move |uid| {
            editing::delete(session.clone(), uid, on_focus.clone(), on_redraw.clone())
        })
    };
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
    let groups = objects::group(&matching);

    // Editing is of the live marker, so a moment being shown from its past
    // is looked at and not written over.
    let live = held.replay().is_none_or(|replay| replay.position.is_live());
    let panel = focus
        .feature()
        .and_then(|uid| held.displayed(uid))
        .map(|feature| {
            html! {
                <Properties
                    feature={feature.clone()}
                    {live}
                    channels={held.edit().channels.clone()}
                    problem={held.edit().problem.clone()}
                    busy={held.edit().busy}
                    {onclose}
                    {onsave}
                    {ondelete}
                />
            }
        });

    // The chooser is rendered into the element the map positions, so it is
    // the map's to place and Yew's to fill.
    let chooser = match &*focus {
        Focus::Choosing { uids, .. } => {
            let entries: Vec<RosterEntry> = uids
                .iter()
                .filter_map(|uid| held.store().get(uid))
                .take(ROSTER_ROWS)
                .map(RosterEntry::of)
                .collect();

            Some(html! { <Chooser {entries} onselect={onchoose} /> })
        }
        _ => None,
    };
    let chooser = chooser
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
            <div
                ref={container}
                class="map-page__canvas"
                role="application"
                aria-label="Map. Everything on it is also listed beside it."
            />

            <div class="map-page__overlay">
            <div class="map-page__toolbar">
                <Toolbar
                    tool={held.tool()}
                    {ontool}
                    {onfit}
                    {tone}
                    {label}
                    title={explanation.clone()}
                    count={held.store().len()}
                />
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
                if held.tool() == Tool::Pin {
                    <p class="map-page__note">{ "Click the map to place a marker." }</p>
                }
                // A placement that failed has no panel to say so in.
                if focus.feature().is_none() {
                    if let Some(problem) = &held.edit().problem {
                        <Alert kind={AlertKind::Error} title="The marker was not placed." message={problem.clone()} />
                    }
                }
                if let Some(shown) = shown {
                    <p class="map-page__note map-page__note--shown">{ shown }</p>
                }
            </div>

            <div class="map-page__objects">
                <ObjectList
                    {groups}
                    matched={matching.len()}
                    search={(*search).clone()}
                    {onsearch}
                    selected={focus.feature().map(str::to_owned)}
                    {onselect}
                />
            </div>

            <div class="map-page__properties">{ for panel }</div>
            <div class="map-page__playback">{ for playback }</div>
            </div>
            <div class="map-page__portal">{ for chooser }</div>
        </div>
    }
}
