//! What this server has done, and what it refused to do.
//!
//! The audit log is the one place where a refusal is as interesting as a
//! success, so outcomes are shown rather than filtered out, and a failure or a
//! denial is coloured to be findable by eye.

use rustak_api::{AuditCategory, AuditRecord};
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Card, EmptyState, LoadingNote, StatusPill, StatusTone};
use crate::util::{format_iso8601, short_relative};

use super::load::{use_refresh_action, use_resource};

/// One audit record, as both this page and the dashboard show it.
pub fn audit_row(record: &AuditRecord) -> Html {
    let who = match (&record.actor, &record.subject) {
        (Some(actor), Some(subject)) if actor != subject => format!("{actor} → {subject}"),
        (Some(actor), _) => actor.clone(),
        (None, Some(subject)) => subject.clone(),
        (None, None) => "the server".to_string(),
    };

    html! {
        <li class="audit-row" key={record.id}>
            <span class="audit-row__when" title={format_iso8601(record.occurred_at)}>
                { short_relative(record.occurred_at) }
            </span>
            <span class="audit-row__category">{ record.category.label() }</span>
            <span class="audit-row__action">{ record.action.clone() }</span>
            <span class="audit-row__who">{ who }</span>
            <span class="audit-row__message">
                { record.message.clone().unwrap_or_default() }
            </span>
            <StatusPill
                tone={StatusTone::of_outcome(record.outcome)}
                label={record.outcome.label()}
            />
        </li>
    }
}

#[function_component(Activity)]
pub fn activity() -> Html {
    let category = use_state(|| None::<AuditCategory>);
    let selected = *category;

    let records = use_resource(move || api::audit::list(selected, api::audit::DEFAULT_LIMIT));
    use_refresh_action(records.reload.clone(), records.busy);

    let on_category = {
        let category = category.clone();
        Callback::from(move |value: Option<AuditCategory>| category.set(value))
    };

    let filters = {
        let chip = |value: Option<AuditCategory>, label: &str| {
            let active = value == selected;
            let on_category = on_category.clone();
            let onclick = Callback::from(move |_: MouseEvent| on_category.emit(value));
            html! {
                <button
                    type="button"
                    class={classes!("chip", active.then_some("chip--active"))}
                    aria-pressed={active.to_string()}
                    {onclick}
                >
                    { label.to_string() }
                </button>
            }
        };

        html! {
            <div class="chip-row">
                { chip(None, "Everything") }
                { for AuditCategory::ALL.iter().map(|value| chip(Some(*value), value.label())) }
            </div>
        }
    };

    let body = match (&records.data, &records.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load the activity log."
                message={message.clone()}
            />
        },
        (Some(list), error) => html! {
            <>
                if let Some(message) = error {
                    <Alert
                        kind={AlertKind::Warning}
                        title="The activity log could not be refreshed."
                        message={message.clone()}
                    />
                }
                if list.is_empty() {
                    <EmptyState
                        title="Nothing to report"
                        message="No audit records match this filter yet."
                    />
                } else {
                    <ul class="audit-list">
                        { for list.iter().map(audit_row) }
                    </ul>
                }
            </>
        },
    };

    html! {
        <>
            { filters }
            <Card>{ body }</Card>
        </>
    }
}
