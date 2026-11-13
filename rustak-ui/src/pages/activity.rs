//! What this server has done, and what it refused to do.
//!
//! The audit log is the one place where a refusal is as interesting as a
//! success, so outcomes are shown rather than filtered out, and a failure or a
//! denial is coloured to be findable by eye.

use rustak_api::{AuditCategory, AuditRecord};
use yew::prelude::*;
use yewi::{Timeline, TimelineItem, Timestamp};

use crate::api;
use crate::components::{
    Alert, AlertKind, EmptyState, LoadingNote, Section, Select, SelectOption, StatusTone, Tag,
    use_page_controls,
};

use super::load::{use_refresh_action, use_resource};

/// One audit record, as an event on a timeline: what was done, to whom, how
/// it turned out, and when.
///
/// Shared with the dashboard's "Recent activity", so the two can never
/// disagree about what a record looks like.
fn audit_event(record: &AuditRecord) -> Html {
    let who = match (&record.actor, &record.subject) {
        (Some(actor), Some(subject)) if actor != subject => format!("{actor} → {subject}"),
        (Some(actor), _) => actor.clone(),
        (None, Some(subject)) => subject.clone(),
        (None, None) => "the server".to_string(),
    };
    let title = html! {
        <span class="audit-event__title">
            <code class="audit-event__action">{ record.action.clone() }</code>
            <Tag>{ record.category.label() }</Tag>
        </span>
    };

    html! {
        <TimelineItem
            key={record.id}
            class="audit-event"
            {title}
            tone={StatusTone::of_outcome(record.outcome).tone()}
            tone_label={record.outcome.label()}
            meta={html! { <>{ who }{ " · " }{ record.outcome.label() }</> }}
            aside={html! { <Timestamp at={record.occurred_at} /> }}
        >
            { record.message.clone() }
        </TimelineItem>
    }
}

/// A list of audit records, newest first, as the server returned them.
pub fn audit_timeline(records: &[AuditRecord]) -> Html {
    html! {
        <Timeline class="audit-timeline" label="Audit records, newest first">
            { for records.iter().map(audit_event) }
        </Timeline>
    }
}

#[function_component(Activity)]
pub fn activity() -> Html {
    let category = use_state(|| None::<AuditCategory>);
    let selected = *category;
    let records = use_resource(move || api::audit::list(selected, api::audit::DEFAULT_LIMIT));
    use_refresh_action(records.reload.clone(), records.busy);

    // Changing the category asks the server again: the log is filtered where
    // it is kept, so a rare category is not lost behind the page limit.
    {
        let reload = records.reload.clone();
        use_effect_with(selected, move |_| {
            reload.emit(());
            || ()
        });
    }

    let onchange = {
        let category = category.clone();
        Callback::from(move |value: Option<String>| {
            category.set(value.as_deref().and_then(AuditCategory::parse));
        })
    };
    use_page_controls(html! {
        <Select
            id="activity-category"
            label="Category"
            value={selected.map(|category| AttrValue::from(category.as_str()))}
            options={AuditCategory::ALL
                .iter()
                .map(|category| SelectOption::new(category.as_str(), category.label()))
                .collect::<Vec<_>>()}
            placeholder="Every category"
            clearable=true
            {onchange}
        />
    });

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
                    { audit_timeline(list) }
                }
            </>
        },
    };

    html! { <Section>{ body }</Section> }
}
