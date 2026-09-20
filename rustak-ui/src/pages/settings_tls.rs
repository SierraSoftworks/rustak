//! How this server presents itself on its public listener.
//!
//! Read-only apart from one button. TLS is settled by `config.toml` before the
//! process has a listener to serve this endpoint from, so a form that appeared
//! to change it would be lying — but *fetching the certificate again now* is
//! not a change to the configuration, and the useful moment for it is exactly
//! when the last attempt failed: the card shows the reason, an operator fixes
//! whatever it named, and the next scheduled attempt is otherwise hours (or,
//! for a pair of files, a `reload_interval`) away.
//!
//! # Two sources fetch their certificate while the server runs
//!
//! `acme` orders one and `files` reads one off disk, and both can sit in an
//! unhealthy steady state: an order that keeps failing, or a pair a sidecar
//! has not written yet — where the listener binds with a certificate from
//! this installation's own authority and waits (M2-13). So both get the
//! banner, the error and the button. A certificate from the internal
//! authority is either being served or the server did not start, and `none`
//! has no certificate to have an opinion about.

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

/// What the state is called, which depends on where the certificate comes from.
///
/// "Not issued yet" is the truth about an ACME order that has not run and
/// nonsense about a pair of files: nobody issues those here, and what is
/// actually happening is that the listener is waiting for them to appear.
fn state_label(state: TlsCertificateState, source: TlsSource) -> &'static str {
    match (state, source) {
        (TlsCertificateState::Valid, _) => "Valid",
        (TlsCertificateState::Expiring, _) => "Renewing",
        (TlsCertificateState::Failed, TlsSource::Files) => "Files unusable",
        (TlsCertificateState::Failed, _) => "Last order failed",
        (TlsCertificateState::Missing, TlsSource::Files) => "Waiting for the files",
        (TlsCertificateState::Missing, _) => "Not issued yet",
    }
}

/// The heading over whatever went wrong last.
///
/// An ACME failure is an *order* that failed and is counted, because it will
/// be retried on a schedule; a files failure is a pair on disk that could not
/// be served, and saying "the last order failed" about it sends an operator
/// looking for an order nobody placed.
fn error_title(status: &TlsStatus) -> String {
    if status.source == TlsSource::Files {
        return "The certificate files could not be used.".to_string();
    }

    match status.attempts {
        0 | 1 => "The last order failed.".to_string(),
        attempts => format!("{attempts} orders in a row have failed."),
    }
}

/// Whether this source is one the server keeps fetching a certificate for,
/// and therefore one with a button worth pressing.
fn is_fetched(source: TlsSource) -> bool {
    matches!(source, TlsSource::Acme | TlsSource::Files)
}

/// What the button says, what it says on hover, and what it says when the
/// request fails — all three, in one place, because they are the same sentence
/// told three ways and a mode that changed one of them would want the others.
fn renew_label(source: TlsSource) -> (&'static str, &'static str, &'static str) {
    match source {
        TlsSource::Files => (
            "Reload certs",
            "Read the certificate files now rather than waiting for the next check.",
            "We could not re-read the certificate files.",
        ),
        _ => (
            "Renew now",
            "Order one now rather than waiting for the renewal job.",
            "We could not order a certificate.",
        ),
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

    // The loaded card is `Details`' own to render: the button in its heading
    // belongs to the request state that lives there.
    let body = match (&tls.data, &tls.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read the transport settings."
                message={message.clone()}
            />
        },
        (Some(status), _) => {
            return html! {
                <Details status={status.clone()} on_changed={tls.reload.clone()} />
            };
        }
    };

    html! {
        <Card title="Transport security" subtitle="What the public listener presents.">
            { body }
        </Card>
    }
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

    // Every one of these depends on which of the two fetching sources this is,
    // and `html!` has nowhere to put a `let`.
    let (renew_action, renew_explanation, renew_failure) = renew_label(status.source);

    let subtitle = source_note(status.source);

    if status.source == TlsSource::None {
        return html! {
            <Card title="Transport security" {subtitle}>
                <Alert
                    kind={AlertKind::Warning}
                    title="This server is not serving TLS."
                    message="Every credential a client sends — an enrolment token, a client \
                             password, a bearer token — travels in the clear. Set \
                             `[web.public.tls] mode` in the configuration file."
                />
            </Card>
        };
    }

    // The one action on the card, in the heading row. Sources that do not
    // fetch a certificate have nothing there.
    let actions = if is_fetched(status.source) {
        html! {
            <Button
                small=true
                busy={*busy}
                title={Some(AttrValue::from(renew_explanation))}
                onclick={renew}
            >
                { renew_action }
            </Button>
        }
    } else {
        Html::default()
    };

    html! {
        <Card title="Transport security" {subtitle} {actions}>
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title={renew_failure}
                    message={message.clone()}
                />
            }
            if let Some(reason) = &status.last_error {
                <Alert
                    kind={if status.needs_attention() { AlertKind::Error } else { AlertKind::Warning }}
                    title={error_title(status)}
                    message={reason.clone()}
                />
            }
            // What the listener is doing about a certificate it has not got.
            // `state` says "missing"; this is the sentence that says why that
            // is not necessarily a problem yet — a `files` deployment whose
            // sidecar has not written the pair is waiting, not broken.
            if let Some(note) = &status.note {
                <Alert
                    kind={if status.needs_attention() { AlertKind::Warning } else { AlertKind::Info }}
                    title="What the listener is waiting for."
                    message={note.clone()}
                />
            }

            <StatusPill
                tone={tone(status.state)}
                label={state_label(status.state, status.source)}
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

                if status.source == TlsSource::Files {
                    <dt>{ "Certificate file" }</dt>
                    <dd>
                        <code>
                            { status.cert_file.clone().unwrap_or_else(|| "—".to_string()) }
                        </code>
                    </dd>

                    <dt>{ "Key file" }</dt>
                    <dd>
                        <code>
                            { status.key_file.clone().unwrap_or_else(|| "—".to_string()) }
                        </code>
                    </dd>

                    <dt>{ "Last read" }</dt>
                    <dd title={status.loaded_at.map(format_iso8601).map(AttrValue::from)}>
                        { status.loaded_at.map(short_relative)
                            .unwrap_or_else(|| "Never — nothing has been read off disk."
                                .to_string()) }
                    </dd>
                }

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
        </Card>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every state, for a loop that wants to cover all of them.
    const STATES: &[TlsCertificateState] = &[
        TlsCertificateState::Valid,
        TlsCertificateState::Expiring,
        TlsCertificateState::Failed,
        TlsCertificateState::Missing,
    ];

    /// Every source, likewise.
    const SOURCES: &[TlsSource] = &[
        TlsSource::Acme,
        TlsSource::Internal,
        TlsSource::Files,
        TlsSource::None,
    ];

    #[test]
    fn a_failed_order_never_looks_like_a_working_certificate() {
        for state in STATES {
            assert_eq!(
                tone(*state) == StatusTone::Ok,
                *state == TlsCertificateState::Valid,
                "{state:?} should read as working exactly when it is",
            );

            for source in SOURCES {
                assert!(!state_label(*state, *source).is_empty());
            }
        }
    }

    #[test]
    fn a_listener_waiting_for_its_files_is_not_told_it_has_not_been_issued_one() {
        // Nobody issues a pair of files. "Not issued yet" sends an operator
        // looking for an order this installation never places; what is
        // actually happening is that the listener is waiting for a sidecar to
        // write them (M2-13).
        assert_eq!(
            state_label(TlsCertificateState::Missing, TlsSource::Files),
            "Waiting for the files",
        );
        assert_eq!(
            state_label(TlsCertificateState::Missing, TlsSource::Acme),
            "Not issued yet",
        );

        let files = TlsStatus {
            state: TlsCertificateState::Failed,
            last_error: Some("the key is not the leaf's".to_string()),
            ..TlsStatus::fixed(TlsSource::Files)
        };
        assert_eq!(
            error_title(&files),
            "The certificate files could not be used."
        );

        let acme = TlsStatus {
            attempts: 3,
            ..TlsStatus::fixed(TlsSource::Acme)
        };
        assert_eq!(error_title(&acme), "3 orders in a row have failed.");
    }

    #[test]
    fn the_button_is_offered_for_the_sources_that_fetch_a_certificate() {
        // `internal` is issued here and `none` has nothing to fetch, so there
        // is no request either of them could make.
        assert!(is_fetched(TlsSource::Acme));
        assert!(is_fetched(TlsSource::Files));
        assert!(!is_fetched(TlsSource::Internal));
        assert!(!is_fetched(TlsSource::None));

        assert_eq!(renew_label(TlsSource::Files).0, "Reload certs");
        assert_eq!(renew_label(TlsSource::Acme).0, "Renew now");
        assert_eq!(
            renew_label(TlsSource::Files).2,
            "We could not re-read the certificate files.",
            "a files installation places no orders, so it cannot fail to place one",
        );

        for source in SOURCES {
            let (label, explanation, failure) = renew_label(*source);
            assert!(!label.is_empty());
            assert!(!explanation.is_empty());
            assert!(!failure.is_empty());
        }
    }

    #[test]
    fn every_source_says_where_the_certificate_came_from() {
        for source in SOURCES {
            assert!(!source_note(*source).is_empty());
        }
    }

    #[test]
    fn a_certificate_fetched_while_the_server_runs_is_the_one_worth_warning_about() {
        // A certificate from the internal authority is either being served or
        // the server did not start, so there is nothing to warn about however
        // old it is. `acme` and `files` both fetch theirs from somewhere else
        // while the server runs, and both can keep failing to.
        let of = |source, state| TlsStatus {
            state,
            ..TlsStatus::fixed(source)
        };

        assert!(of(TlsSource::Acme, TlsCertificateState::Failed).needs_attention());
        assert!(of(TlsSource::Files, TlsCertificateState::Failed).needs_attention());
        assert!(
            of(TlsSource::Files, TlsCertificateState::Missing).needs_attention(),
            "a listener still waiting for its pair is presenting one no client trusts",
        );
        assert!(!of(TlsSource::Internal, TlsCertificateState::Failed).needs_attention());
        assert!(!of(TlsSource::Acme, TlsCertificateState::Valid).needs_attention());
        assert!(!of(TlsSource::Files, TlsCertificateState::Valid).needs_attention());
    }
}
