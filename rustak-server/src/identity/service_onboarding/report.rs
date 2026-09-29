//! What an onboarding says it did: the sentences the response carries and the
//! audit entry it leaves. Neither ever holds a secret — the audit entry names
//! credentials by their rows.

use rustak_api::{AuditCategory, AuditOutcome, ServiceName, ServiceTokenOutcome};

use crate::db::AuditEntry;
use crate::db::repos::UserRow;
use crate::prelude::*;

use super::Issued;

/// What the action did, in the words the response carries.
pub(super) fn notes(
    account: &Username,
    created: bool,
    outcome: ServiceTokenOutcome,
    revoked: usize,
) -> Vec<String> {
    let mut notes = vec![if created {
        format!("Created the service account '{account}' and minted its one-time enrolment token.")
    } else {
        format!(
            "The service account '{account}' already existed: a fresh one-time enrolment token \
             was minted for it. Its certificates and any earlier enrolment token were left alone."
        )
    }];

    notes.push(match outcome {
        ServiceTokenOutcome::Minted => {
            "Minted a service token, which the sidecar reaches the control API with.".to_string()
        }
        ServiceTokenOutcome::Kept => "Kept the service token this account already holds; it is \
             not shown again. Ask again with the service token replaced to mint a new one — which \
             revokes the old one and stops whatever is using it."
            .to_string(),
        ServiceTokenOutcome::Rotated => format!(
            "Minted a new service token and revoked the {revoked} it replaces: a sidecar still \
             using the old one can no longer reach the control API."
        ),
    });

    notes
}

/// Writes who added which service, and what it was given. Never a secret.
pub(super) async fn record(
    context: &AppContext,
    user: &UserRow,
    name: &ServiceName,
    actor: &Username,
    created: bool,
    issued: &Issued,
    outcome: ServiceTokenOutcome,
) {
    let entry = AuditEntry::new(
        AuditCategory::Administration,
        "service.onboarding.created",
        AuditOutcome::Success,
    )
    .subject(&user.username)
    .actor(actor)
    .message(format!(
        "The service '{name}' was set up to run as {}; its secrets were shown once.",
        user.username
    ))
    .detail(serde_json::json!({
        "service": name,
        "account_created": created,
        "enrollment_token_id": issued.enrolment.credential.id,
        "service_token": outcome,
        "service_token_id": issued.service.as_ref().map(|minted| minted.credential.id),
        "revoked_service_tokens": issued.revoked,
    }));

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a service onboarding in the audit log.");
        context.session().record_human_error(&err);
    }
}
