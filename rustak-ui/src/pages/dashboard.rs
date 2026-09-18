//! How this server is doing, at a glance.
//!
//! Four questions, in the order somebody arriving at a console asks them: is it
//! running, what is it called, who can use it, and what has it been doing.

use rustak_api::{Health, ServerSettings, User};
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Card, LoadingNote, Stat, StatusPill, StatusTone};
use crate::util::{nav_href, short_duration};

use super::activity::audit_row;
use super::load::{use_refresh_action, use_resource};

#[function_component(Dashboard)]
pub fn dashboard() -> Html {
    let health = use_resource(api::health::get);
    let settings = use_resource(api::settings::get);
    let users = use_resource(api::users::list);
    let activity = use_resource(|| api::audit::list(None, 6));

    let busy = health.busy || settings.busy || users.busy || activity.busy;
    let reload = {
        let reloads = [
            health.reload.clone(),
            settings.reload.clone(),
            users.reload.clone(),
            activity.reload.clone(),
        ];
        Callback::from(move |_| {
            for reload in &reloads {
                reload.emit(());
            }
        })
    };
    use_refresh_action(reload, busy);

    // One alert for all four, because four stacked copies of "we could not reach
    // the server" says nothing the first one did not.
    let error = [
        &health.error,
        &settings.error,
        &users.error,
        &activity.error,
    ]
    .into_iter()
    .flatten()
    .next()
    .cloned();

    html! {
        <>
            if let Some(message) = error {
                <Alert
                    kind={AlertKind::Error}
                    title="Some of this page could not be loaded."
                    message={message}
                />
            }

            <div class="dashboard">
                <Card title="Server">{ health_panel(health.data.as_ref()) }</Card>
                <Card title="Identity">{ settings_panel(settings.data.as_ref()) }</Card>
                <Card title="People">{ users_panel(users.data.as_deref()) }</Card>
            </div>

            <Card
                title="Recent activity"
                subtitle="The last few things this server did."
                actions={html! {
                    <a class="btn btn--small" href={nav_href("/admin/activity")}>
                        { "See everything" }
                    </a>
                }}
            >
                {
                    match activity.data.as_deref() {
                        None => html! { <LoadingNote /> },
                        Some(records) => html! {
                            <ul class="audit-list">{ for records.iter().map(audit_row) }</ul>
                        },
                    }
                }
            </Card>
        </>
    }
}

fn health_panel(health: Option<&Health>) -> Html {
    let Some(health) = health else {
        return html! { <LoadingNote /> };
    };

    html! {
        <>
            <div class="card__status">
                <StatusPill
                    tone={StatusTone::of_component(health.status)}
                    label={health.status.label()}
                    title={health.message.clone()}
                />
                <StatusPill
                    tone={StatusTone::of_component(health.database)}
                    label={format!("Database: {}", health.database.label())}
                />
            </div>
            <dl class="detail-list">
                <dt>{ "Version" }</dt>
                <dd>{ health.version.clone() }</dd>
                <dt>{ "Uptime" }</dt>
                <dd>{ short_duration(health.uptime_seconds as i64) }</dd>
            </dl>
        </>
    }
}

fn settings_panel(settings: Option<&ServerSettings>) -> Html {
    let Some(settings) = settings else {
        return html! { <LoadingNote /> };
    };

    let domains = if settings.domains.is_empty() {
        "— not set —".to_string()
    } else {
        settings.domains.join(", ")
    };

    html! {
        <dl class="detail-list">
            <dt>{ "Name" }</dt>
            <dd>{ settings.name.clone() }</dd>
            <dt>{ "Host names" }</dt>
            <dd>{ domains }</dd>
            <dt>{ "Base URL" }</dt>
            <dd>{ settings.base_url.clone().unwrap_or_else(|| "— inferred —".to_string()) }</dd>
            <dt>{ "Node ID" }</dt>
            <dd><code>{ settings.node_id.clone().unwrap_or_else(|| "—".to_string()) }</code></dd>
        </dl>
    }
}

fn users_panel(users: Option<&[User]>) -> Html {
    let Some(users) = users else {
        return html! { <LoadingNote /> };
    };

    let admins = users.iter().filter(|user| user.is_admin).count();
    let suspended = users.iter().filter(|user| user.disabled).count();

    html! {
        <div class="stat-row">
            <Stat label="Accounts" value={users.len().to_string()} />
            <Stat label="Administrators" value={admins.to_string()} />
            <Stat
                label="Suspended"
                value={suspended.to_string()}
                detail={(suspended > 0).then_some("Cannot sign in or connect.")}
            />
        </div>
    }
}
