//! How this server presents itself on its public listener.
//!
//! Read-only apart from one button. TLS is settled by `config.toml` before the
//! process has a listener to serve this endpoint from, so a form that appeared
//! to change it would be lying — but *ordering a certificate now* is not a
//! change to the configuration, and the useful moment for it is exactly when
//! the last order failed: the card shows the authority's own error, an
//! operator fixes whatever it named, and the next scheduled attempt is
//! otherwise hours away.
//!
//! Only ACME has more than one state to be in. A certificate from a file or
//! from the internal authority is either being served or the server did not
//! start, which is why `needs_attention` is `false` for both however old they
//! are.

use rustak_api::{TlsCertificateState, TlsSource, TlsStatus};
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Button, Card, LoadingNote, StatusPill, StatusTone};
use crate::util::{format_iso8601, short_relative};

use super::load::use_resource;

/// The tone a certificate's state is shown in.
fn tone(state: TlsCertificateState) -> StatusTone {
    match state {
        TlsCertificateState::Valid => StatusTone::Ok,
        TlsCertificateState::Expiring => StatusTone::Warning,
        TlsCertificateState::Failed => StatusTone::Error,
        TlsCertificateState::Missing => StatusTone::Neutral,
    }
}

fn state_label(state: TlsCertificateState) -> &'static str {
    match state {
        TlsCertificateState::Valid => "Valid",
        TlsCertificateState::Expiring => "Renewing",
        TlsCertificateState::Failed => "Last order failed",
        TlsCertificateState::Missing => "Not issued yet",
    }
}

/// What the source means, in a sentence.
fn source_note(source: TlsSource) -> &'static str {
    match source {
        TlsSource::Acme => "Ordered from a certificate authority and renewed automatically.",
        TlsSource::Internal => {
            "Issued by this installation's own authority, so a browser will not trust it \
             without the CA."
        }
        TlsSource::Files => "Read from the files named in the configuration.",
        TlsSource::None => "This listener serves plain HTTP.",
    }
}

#[function_component(TlsCard)]
pub fn tls_card() -> Html {
    let tls = use_resource(api::settings::tls);

    let body = match (&tls.data, &tls.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read the transport settings."
                message={message.clone()}
            />
        },
        (Some(status), _) => html! {
            <Details status={status.clone()} on_changed={tls.reload.clone()} />
        },
    };

    let subtitle = tls
        .data
        .as_ref()
        .map(|status| source_note(status.source))
        .unwrap_or("What the public listener presents.");

    html! { <Card title="Transport security" {subtitle}>{ body }</Card> }
}

#[derive(Properties, PartialEq)]
struct DetailsProps {
    status: TlsStatus,
    on_changed: Callback<()>,
}

#[function_component(Details)]
fn details(props: &DetailsProps) -> Html {
    let status = &props.status;
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let renew = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        Callback::from(move |_: MouseEvent| {
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            busy.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                match api::settings::renew_tls().await {
                    Ok(_) => {
                        error.set(None);
                        on_changed.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    if status.source == TlsSource::None {
        return html! {
            <Alert
                kind={AlertKind::Warning}
                title="This server is not serving TLS."
                message="Every credential a client sends — an enrolment token, a client \
                         password, a bearer token — travels in the clear. Set \
                         `[web.public.tls] mode` in the configuration file."
            />
        };
    }

    html! {
        <>
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="We could not order a certificate."
                    message={message.clone()}
                />
            }
            if let Some(reason) = &status.last_error {
                <Alert
                    kind={if status.needs_attention() { AlertKind::Error } else { AlertKind::Warning }}
                    title={match status.attempts {
                        0 | 1 => "The last order failed.".to_string(),
                        attempts => format!("{attempts} orders in a row have failed."),
                    }}
                    message={reason.clone()}
                />
            }

            <StatusPill
                tone={tone(status.state)}
                label={state_label(status.state)}
                title={status.not_after.map(format_iso8601).map(AttrValue::from)}
            />

            <dl class="detail-list">
                <dt>{ "Host names" }</dt>
                <dd>
                    { match status.domains.is_empty() {
                        true => "— not set —".to_string(),
                        false => status.domains.join(", "),
                    } }
                </dd>

                <dt>{ "Valid" }</dt>
                <dd>
                    { match (status.not_before, status.not_after) {
                        (Some(from), Some(until)) => format!(
                            "{} to {} ({})",
                            format_iso8601(from),
                            format_iso8601(until),
                            short_relative(until),
                        ),
                        _ => "Nothing has been issued yet.".to_string(),
                    } }
                </dd>

                if status.source == TlsSource::Acme {
                    <dt>{ "Renews" }</dt>
                    <dd>
                        { status.renews_at.map(short_relative)
                            .unwrap_or_else(|| "—".to_string()) }
                    </dd>

                    <dt>{ "Directory" }</dt>
                    <dd>
                        <code>
                            { status.directory.clone().unwrap_or_else(|| "—".to_string()) }
                        </code>
                    </dd>

                    <dt>{ "Validated by" }</dt>
                    <dd>{ status.challenge.clone().unwrap_or_else(|| "—".to_string()) }</dd>

                    <dt>{ "Last attempt" }</dt>
                    <dd>
                        { status.last_attempt_at.map(short_relative)
                            .unwrap_or_else(|| "Never".to_string()) }
                    </dd>
                }
            </dl>

            if status.source == TlsSource::Acme {
                <Button
                    busy={*busy}
                    title={Some(AttrValue::from(
                        "Order one now rather than waiting for the renewal job.",
                    ))}
                    onclick={renew}
                >
                    { "Renew now" }
                </Button>
            }
        </>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_order_never_looks_like_a_working_certificate() {
        for state in [
            TlsCertificateState::Valid,
            TlsCertificateState::Expiring,
            TlsCertificateState::Failed,
            TlsCertificateState::Missing,
        ] {
            assert_eq!(
                tone(state) == StatusTone::Ok,
                state == TlsCertificateState::Valid,
                "{state:?} should read as working exactly when it is",
            );
            assert!(!state_label(state).is_empty());
        }
    }

    #[test]
    fn every_source_says_where_the_certificate_came_from() {
        for source in [
            TlsSource::Acme,
            TlsSource::Internal,
            TlsSource::Files,
            TlsSource::None,
        ] {
            assert!(!source_note(source).is_empty());
        }
    }

    #[test]
    fn only_an_acme_certificate_can_be_in_a_state_worth_warning_about() {
        // A certificate from a file or from the internal authority is either
        // being served or the server did not start, so there is nothing to
        // warn about however old it is.
        let of = |source, state| TlsStatus {
            state,
            ..TlsStatus::fixed(source)
        };

        assert!(of(TlsSource::Acme, TlsCertificateState::Failed).needs_attention());
        assert!(!of(TlsSource::Files, TlsCertificateState::Failed).needs_attention());
        assert!(!of(TlsSource::Acme, TlsCertificateState::Valid).needs_attention());
    }
}
