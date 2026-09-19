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
//! the reason is part of what is chosen rather than defaulted: the plain
//! "Revoke" on the row's button is an administrator's decision, and the menu
//! beside it offers the three that say more. Each is stored on the row, written
//! to the audit log and shown here afterwards.

use rustak_api::{Certificate, CertificateState, RevocationReason};
use yew::prelude::*;

use crate::components::{MenuAction, MenuItem, StatusPill, StatusTone};
use crate::util::{format_iso8601, short_relative};

/// The tone a certificate's state is shown in.
fn tone(state: CertificateState) -> StatusTone {
    match state {
        CertificateState::Active => StatusTone::Ok,
        CertificateState::Revoked => StatusTone::Error,
        CertificateState::Expired => StatusTone::Warning,
    }
}

/// The reasons offered in the menu beside the plain "Revoke", and what each is
/// called there.
///
/// `admin_action` is the button itself, not a menu item. `credential_revoked`
/// and `user_disabled` are left out: the server sets both itself when the
/// cascade runs, so choosing one here would be describing a cause rather than
/// the one being applied.
const MENU_REASONS: &[(RevocationReason, &str)] = &[
    (RevocationReason::UserRequest, "Revoke (user requested)"),
    (RevocationReason::DeviceLost, "Revoke (device lost)"),
    (RevocationReason::Superseded, "Revoke (cert replaced)"),
];

/// The revocation actions for a certificate: the plain one for the row's
/// button, and the reasoned ones for the menu beside it. `None` when there is
/// nothing left to revoke.
///
/// Every one asks first, naming the subject, because a revocation is refused
/// at the next handshake and drops any connection already holding it.
pub fn revoke_actions(
    certificate: &Certificate,
    revoke: &Callback<RevocationReason>,
) -> Option<(MenuAction, Vec<MenuItem>)> {
    if certificate.state(chrono::Utc::now()) == CertificateState::Revoked {
        return None;
    }

    let question = format!(
        "Revoke the certificate for '{}'? It is refused at the next handshake and any \
         connection already holding it is dropped.",
        certificate.subject_cn,
    );

    let action = |label: &'static str, reason: RevocationReason| {
        let revoke = revoke.clone();
        MenuAction::new(label, Callback::from(move |()| revoke.emit(reason)))
            .danger()
            .confirm(question.clone(), "Revoke it")
    };

    let primary = action("Revoke", RevocationReason::AdminAction);
    let items = MENU_REASONS
        .iter()
        .map(|(reason, label)| MenuItem::Action(action(label, *reason)))
        .collect();

    Some((primary, items))
}

#[derive(Properties, PartialEq)]
pub struct CertificateDetailsProps {
    /// The certificate, when one could be read.
    pub certificate: Option<Certificate>,

    /// Whether this device has ever presented one at all, which is not the
    /// same as our not being able to read it.
    pub had_one: bool,
}

/// What a device's certificate is and how long it is good for. The actions on
/// it live on the row, beside the device's own — see [`revoke_actions`].
#[function_component(CertificateDetails)]
pub fn certificate_details(props: &CertificateDetailsProps) -> Html {
    let Some(certificate) = &props.certificate else {
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
        let offered: Vec<RevocationReason> =
            MENU_REASONS.iter().map(|(reason, _)| *reason).collect();

        assert!(offered.contains(&RevocationReason::DeviceLost));
        assert!(
            !offered.contains(&RevocationReason::CredentialRevoked),
            "naming it here would describe a cause rather than apply one",
        );
        assert!(!offered.contains(&RevocationReason::UserDisabled));
        assert!(
            !offered.contains(&RevocationReason::AdminAction),
            "the plain button already is this one",
        );

        for (_, label) in MENU_REASONS {
            assert!(
                label.starts_with("Revoke ("),
                "{label} should read as a kind of revoke"
            );
        }
    }
}
