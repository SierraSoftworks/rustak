//! One identifier's stored message, and what came before it.
//!
//! Split out of `cot_browser.rs` because it is two more requests — the
//! document and the history window — and one destructive action, each with its
//! own failure.
//!
//! # The XML is the answer, not a debugging aid
//!
//! A `<detail>` a plugin wrote, a stale time a minute in the past, a callsign
//! with a trailing space — none of these is visible in any summary, and all of
//! them explain a marker that is missing. The document is shown verbatim,
//! coloured, through [`crate::components::XmlView`], which never reaches the
//! DOM as markup.

use rustak_api::{CotDetail, CotSummary};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Card, ConfirmButton, LoadingNote, XmlView};
use crate::util::{format_iso8601, short_relative};

use super::load::use_resource;

/// How far back a history read looks: an hour, which is several minutes of
/// position reports and still a bounded request.
const HISTORY_SECONDS: i64 = 3_600;

#[derive(Properties, PartialEq)]
pub struct CotDrawerProps {
    pub uid: String,
    pub on_forgotten: Callback<()>,
}

#[function_component(CotDrawer)]
pub fn cot_drawer(props: &CotDrawerProps) -> Html {
    let uid = props.uid.clone();
    let detail = use_resource(move || {
        let uid = uid.clone();
        async move { api::cot::get(&uid).await }
    });

    let uid = props.uid.clone();
    let history = use_resource(move || {
        let uid = uid.clone();
        async move { api::cot::history(&uid, HISTORY_SECONDS).await }
    });

    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let forget = {
        let uid = props.uid.clone();
        let (busy, error, on_forgotten) = (busy.clone(), error.clone(), props.on_forgotten.clone());

        Callback::from(move |_| {
            let uid = uid.clone();
            let (busy, error, on_forgotten) = (busy.clone(), error.clone(), on_forgotten.clone());

            busy.set(true);
            spawn_local(async move {
                match api::cot::forget(&uid).await {
                    Ok(()) => {
                        error.set(None);
                        on_forgotten.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let body = match (&detail.data, &detail.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read that message."
                message={message.clone()}
            />
        },
        (Some(found), _) => document(found),
    };

    html! {
        <Card
            title={props.uid.clone()}
            subtitle="The message as its recipients were sent it, and what came before."
        >
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="That identifier could not be forgotten."
                    message={message.clone()}
                />
            }

            { body }

            <h3 class="cot-drawer__heading">{ "Earlier, in the last hour" }</h3>
            { history_list(&history.data, &history.error) }

            <ConfirmButton
                label="Forget"
                confirm_label="Forget it"
                question={format!(
                    "Forget '{}'? The latest message and every stored segment for it go, and \
                     there is no undo.",
                    props.uid,
                )}
                busy={*busy}
                onconfirm={forget}
            />
        </Card>
    }
}

fn document(detail: &CotDetail) -> Html {
    html! {
        <>
            <XmlView
                xml={detail.xml.clone()}
                label={format!("The stored message for {}", detail.summary.uid)}
            />
            <p class="cot-drawer__note">
                { format!(
                    "Received {} · stale {}",
                    format_iso8601(detail.summary.received_at),
                    format_iso8601(detail.summary.stale),
                ) }
            </p>
        </>
    }
}

fn history_list(data: &Option<Vec<CotSummary>>, error: &Option<String>) -> Html {
    match (data, error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read the history."
                message={message.clone()}
            />
        },
        (Some(list), _) if list.is_empty() => html! {
            <p class="panel-empty">
                { "Nothing stored in the last hour. Only the latest message is kept \
                   indefinitely." }
            </p>
        },
        (Some(list), _) => html! {
            <ul class="cot-history">
                { for list.iter().enumerate().map(|(index, entry)| html! {
                    <li key={index} class="cot-history__row">
                        <span title={format_iso8601(entry.time)}>
                            { short_relative(entry.time) }
                        </span>
                        <span>{ entry.kind.clone() }</span>
                        <span>{ format!("{:.5}, {:.5}", entry.lat, entry.lon) }</span>
                        if let Some(callsign) = &entry.callsign {
                            <span>{ callsign.clone() }</span>
                        }
                    </li>
                }) }
            </ul>
        },
    }
}
