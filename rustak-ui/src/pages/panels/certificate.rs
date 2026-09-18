//! The certificate a device last presented, and taking it back.
//!
//! Forgetting a device and revoking its certificate are two different actions
//! with two different consequences, and this is the second one. `DELETE
//! /devices/{uid}` removes what we knew about a client and leaves its
//! certificate working; revoking refuses it at the next handshake and drops
//! the stream connections already holding it.
//!
//! # The reason is part of the action
//!
//! "Revoked" on its own does not tell an administrator six months later
//! whether a device was lost or a certificate simply replaced, and the two lead
//! to different actions — one is an incident and the other is housekeeping. So
//! the reason is chosen before the confirmation rather than defaulted, stored
//! on the row, written to the audit log and shown here afterwards.

use rustak_api::{Certificate, CertificateState, RevocationReason};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{ConfirmButton, Select, SelectOption, StatusPill, StatusTone};
use crate::util::{format_iso8601, short_relative};

/// The tone a certificate's state is shown in.
fn tone(state: CertificateState) -> StatusTone {
    match state {
        CertificateState::Active => StatusTone::Ok,
        CertificateState::Revoked => StatusTone::Error,
        CertificateState::Expired => StatusTone::Warning,
    }
}

/// The reasons worth offering.
///
/// `credential_revoked` and `user_disabled` are left out: the server sets both
/// itself when the cascade runs, so choosing one here would be describing a
/// cause rather than the one being applied.
fn reason_options() -> Vec<SelectOption> {
    RevocationReason::ALL
        .iter()
        .filter(|reason| {
            !matches!(
                reason,
                RevocationReason::CredentialRevoked | RevocationReason::UserDisabled
            )
        })
        .map(|reason| SelectOption::new(reason.as_str(), reason.label()))
        .collect()
}

#[derive(Properties, PartialEq)]
pub struct CertificateDetailsProps {
    /// The certificate, when one could be read.
    pub certificate: Option<Certificate>,

    /// Whether this device has ever presented one at all, which is not the
    /// same as our not being able to read it.
    pub had_one: bool,

    pub on_changed: Callback<()>,
}

#[function_component(CertificateDetails)]
pub fn certificate_details(props: &CertificateDetailsProps) -> Html {
    let reason = use_state(|| RevocationReason::AdminAction);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let Some(certificate) = props.certificate.clone() else {
        return html! {
            <span class="certificate" title={match props.had_one {
                true => "This device presented a certificate we could not read. \
                         It may belong to another account.",
                false => "This device has never presented a client certificate.",
            }}>
                { match props.had_one { true => "Certificate unavailable", false => "No certificate" } }
            </span>
        };
    };

    let state = certificate.state(chrono::Utc::now());

    let revoke = {
        let (id, reason) = (certificate.id, reason.clone());
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());

        Callback::from(move |_| {
            let chosen = *reason;
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());

            busy.set(true);
            spawn_local(async move {
                match api::certificates::revoke(id, chosen).await {
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

    html! {
        <div class="certificate">
            <StatusPill
                tone={tone(state)}
                label={state.label()}
                title={certificate.revocation_reason.clone().map(AttrValue::from)}
            />

            <span
                class="certificate__fingerprint"
                title="The SHA-256 fingerprint the TLS verifier matches on."
            >
                { certificate.fingerprint.chars().take(16).collect::<String>() }
            </span>

            <span title={format_iso8601(certificate.not_after)}>
                { match state {
                    CertificateState::Expired => {
                        format!("Expired {}", short_relative(certificate.not_after))
                    }
                    _ => format!("Expires {}", short_relative(certificate.not_after)),
                } }
            </span>

            <span title="Where this certificate came from">{ certificate.source.label() }</span>

            if state != CertificateState::Revoked {
                <Select
                    id={format!("certificate-reason-{}", certificate.id.get())}
                    value={Some(AttrValue::from(reason.as_str()))}
                    options={reason_options()}
                    disabled={*busy}
                    onchange={
                        let reason = reason.clone();
                        Callback::from(move |chosen: Option<String>| {
                            if let Some(chosen) =
                                chosen.as_deref().and_then(RevocationReason::parse)
                            {
                                reason.set(chosen);
                            }
                        })
                    }
                />

                <ConfirmButton
                    label="Revoke"
                    confirm_label="Revoke it"
                    question={format!(
                        "Revoke the certificate for '{}'? It is refused at the next handshake \
                         and any connection already holding it is dropped.",
                        certificate.subject_cn,
                    )}
                    busy={*busy}
                    onconfirm={revoke}
                />
            }

            if let Some(message) = &*error {
                <p class="certificate__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revoked_certificate_never_looks_like_a_working_one() {
        for state in CertificateState::ALL.iter().copied() {
            assert_eq!(
                tone(state) == StatusTone::Ok,
                state == CertificateState::Active,
                "{state:?} should read as working exactly when it is",
            );
        }
    }

    #[test]
    fn the_two_reasons_the_server_sets_itself_are_not_offered() {
        let offered: Vec<String> = reason_options()
            .into_iter()
            .map(|option| option.value.to_string())
            .collect();

        assert!(offered.contains(&"device_lost".to_string()));
        assert!(
            !offered.contains(&"credential_revoked".to_string()),
            "naming it here would describe a cause rather than apply one",
        );
        assert!(!offered.contains(&"user_disabled".to_string()));
    }
}
