//! Adding a service: the account, its enrolment token and its service token,
//! in one administrative action.
//!
//! # What a sidecar deployed this way needs, and why
//!
//! A sidecar with no orchestrator identity starts with a one-time **enrolment
//! token**, which buys the client certificate the CoT stream and the Marti API
//! authenticate. That certificate does not reach the control API: `/api/v1` is
//! served only by the public listener, and that listener asks for no client
//! certificate (see `plugins::auth`), so registration, heartbeats and the
//! per-service configuration need a **service token** as well. This action
//! therefore mints both — the service token only when the account holds no
//! live one, or when the caller explicitly asks to replace it.
//!
//! # Nothing is left behind
//!
//! The account, the enrolment token and the service token are three writes, and
//! argon2 hashing sits between them, so they cannot share one transaction. What
//! makes the action atomic instead is that each step is undone when a later one
//! fails: an account this action created is deleted — which takes its
//! credentials with it — and on an account that already existed the credentials
//! this action minted are revoked. A rotation revokes the old service token only
//! after both new credentials exist.
//!
//! # What is refused
//!
//! A name that is not a valid service name or username, a person's account (a
//! sidecar cannot enrol or sign in as a person), a service account that is
//! switched off, and a service name already registered to another account —
//! each before anything is written.

use chrono::Utc;
use rustak_api::service_onboarding::{config_fragment, environment};
use rustak_api::{
    CredentialId, CredentialKind, ServiceName, ServiceOnboarding, ServiceOnboardingRequest,
    ServiceTokenOutcome, UserKind,
};

use crate::db::repos::{CredentialRow, NewUser, UserRow};
use crate::prelude::*;

use super::credentials::{self, MintRequest, MintedSecret};
use super::secret_cache::VerifiedSecretCache;

mod report;

use report::{notes, record};

/// Why the action was not taken.
#[derive(Debug)]
pub enum Refusal {
    /// The request named something that cannot be a service: a `400`.
    Invalid(String),

    /// The name is somebody else's, or the account cannot be used: a `409`.
    Conflict(String),

    /// Storage or hashing failed; everything written was undone.
    Failed(Error),
}

impl From<Error> for Refusal {
    fn from(err: Error) -> Self {
        Self::Failed(err)
    }
}

/// What the three steps produced.
struct Issued {
    enrolment: MintedSecret,
    service: Option<MintedSecret>,
    revoked: Vec<CredentialId>,
}

/// Creates the account if it is not there, mints its enrolment token, and
/// mints its service token if it needs one.
///
/// # Errors
///
/// See [`Refusal`]. On any error, nothing this call wrote is left usable.
#[instrument("identity.service_onboarding.onboard", skip_all, fields(service = %request.name.trim()), err(Debug))]
pub async fn onboard(
    context: &AppContext,
    request: &ServiceOnboardingRequest,
    actor: &Username,
) -> Result<ServiceOnboarding, Refusal> {
    let (name, account) = names(request)?;
    let existing = context.db().users().get_by_username(&account).await?;

    if let Some(user) = &existing {
        admissible(user)?;
    }
    unregistered_elsewhere(context, &name, &account, existing.as_ref()).await?;

    let created = existing.is_none();
    let user = match existing {
        Some(user) => user,
        None => {
            context
                .db()
                .users()
                .create(NewUser::service(account.clone()))
                .await?
        }
    };

    let live = live_service_tokens(context, &user).await?;
    let rotate = request.rotate_service_token && !live.is_empty();
    let mint_service = live.is_empty() || rotate;

    let mut minted = Vec::new();
    let issued = issue(
        context,
        &user,
        &name,
        actor,
        mint_service,
        rotate.then_some(&live),
        &mut minted,
    )
    .await;

    let issued = match issued {
        Ok(issued) => issued,
        Err(err) => {
            undo(context, &user, created, &minted, actor).await;
            return Err(err.into());
        }
    };

    let outcome = match (&issued.service, rotate) {
        (None, _) => ServiceTokenOutcome::Kept,
        (Some(_), false) => ServiceTokenOutcome::Minted,
        (Some(_), true) => ServiceTokenOutcome::Rotated,
    };

    record(context, &user, &name, actor, created, &issued, outcome).await;

    info!(
        account = %user.username,
        created,
        service_token = ?outcome,
        "Onboarded a service."
    );

    let expires_at = issued
        .enrolment
        .credential
        .expires_at
        .unwrap_or_else(Utc::now);
    let service_token = issued
        .service
        .as_ref()
        .map(|minted| minted.secret.expose().to_string());

    Ok(ServiceOnboarding {
        config_fragment: config_fragment(&name, &user.username),
        environment: environment(
            issued.enrolment.secret.expose(),
            expires_at,
            service_token.as_deref(),
        ),
        notes: notes(&user.username, created, outcome, issued.revoked.len()),
        name,
        account: user.username.clone(),
        account_created: created,
        enrollment_token: issued.enrolment.secret.expose().to_string(),
        enrollment_token_id: issued.enrolment.credential.id,
        enrollment_expires_at: expires_at,
        service_token_outcome: outcome,
        service_token,
        service_token_id: issued.service.as_ref().map(|minted| minted.credential.id),
        revoked_service_tokens: issued.revoked,
    })
}

/// The service name and the account, both validated.
fn names(request: &ServiceOnboardingRequest) -> Result<(ServiceName, Username), Refusal> {
    let name =
        ServiceName::parse(&request.name).map_err(|err| Refusal::Invalid(err.to_string()))?;

    let account = request
        .account
        .as_deref()
        .map(str::trim)
        .filter(|account| !account.is_empty())
        .unwrap_or(name.as_str());

    let account = Username::parse(account).map_err(|err| Refusal::Invalid(err.to_string()))?;

    Ok((name, account))
}

/// Whether an account that already exists can be a sidecar's.
fn admissible(user: &UserRow) -> Result<(), Refusal> {
    if user.kind != UserKind::Service {
        return Err(Refusal::Conflict(format!(
            "'{}' is a person's account, and a sidecar cannot enrol or sign in as a person. \
             Choose another service name, or name a service account for it.",
            user.username
        )));
    }

    if user.disabled {
        return Err(Refusal::Conflict(format!(
            "The service account '{}' is switched off, so a sidecar could not enrol as it. \
             Switch it back on first.",
            user.username
        )));
    }

    Ok(())
}

/// Refuses a service name another account has already registered, because the
/// sidecar would be refused at registration with credentials in hand.
async fn unregistered_elsewhere(
    context: &AppContext,
    name: &ServiceName,
    account: &Username,
    user: Option<&UserRow>,
) -> Result<(), Refusal> {
    let Some(registered) = context.db().services().get_by_name(name).await? else {
        return Ok(());
    };

    if user.is_some_and(|user| user.id == registered.user_id) {
        return Ok(());
    }

    Err(Refusal::Conflict(format!(
        "The service '{name}' is registered to another account, so a sidecar signing in as \
         '{account}' could not register under that name."
    )))
}

/// The account's service tokens that still work.
async fn live_service_tokens(
    context: &AppContext,
    user: &UserRow,
) -> Result<Vec<CredentialRow>, Error> {
    let now = Utc::now();

    Ok(context
        .db()
        .credentials()
        .list_for_user(user.id, false)
        .await?
        .into_iter()
        .filter(|row| row.kind == CredentialKind::ServiceToken && row.is_usable_at(now))
        .collect())
}

/// Mints what is needed, recording each credential in `minted` as it lands so
/// that a failure part-way through knows what to undo.
async fn issue(
    context: &AppContext,
    user: &UserRow,
    name: &ServiceName,
    actor: &Username,
    mint_service: bool,
    replacing: Option<&Vec<CredentialRow>>,
    minted: &mut Vec<CredentialId>,
) -> Result<Issued, Error> {
    let config = &context.config().auth;
    let enrolment_label = format!("Sidecar {name}: first start");
    let service_label = format!("Sidecar {name}");

    let enrolment = credentials::mint(
        context.db(),
        config,
        user,
        MintRequest::new(CredentialKind::EnrollmentToken, &enrolment_label, actor),
    )
    .await?;
    minted.push(enrolment.credential.id);

    let service = if mint_service {
        let service = credentials::mint(
            context.db(),
            config,
            user,
            MintRequest::new(CredentialKind::ServiceToken, &service_label, actor),
        )
        .await?;
        minted.push(service.credential.id);

        Some(service)
    } else {
        None
    };

    let mut revoked = Vec::new();
    if let Some(replacing) = replacing {
        for old in replacing {
            credentials::revoke(context.db(), old.id, actor, VerifiedSecretCache::shared()).await?;
            revoked.push(old.id);
        }

        // An open event feed bought with an old token re-authorizes now rather
        // than at its next periodic check. Not `sessions::end_all`: that would
        // also drop the sidecar's CoT stream, which its certificate — untouched
        // by a rotation — keeps every right to.
        context.events().invalidate(&user.username);
    }

    Ok(Issued {
        enrolment,
        service,
        revoked,
    })
}

/// Takes back what a failed action wrote.
///
/// Logged rather than returned: the caller is already answering with the
/// failure that made this necessary, and that is the one to report.
async fn undo(
    context: &AppContext,
    user: &UserRow,
    created: bool,
    minted: &[CredentialId],
    actor: &Username,
) {
    let undone = if created {
        // Credentials cascade with the account.
        context.db().users().delete(user.id).await.map(|_| ())
    } else {
        let mut outcome = Ok(());
        for id in minted {
            if let Err(err) =
                credentials::revoke(context.db(), *id, actor, VerifiedSecretCache::shared()).await
            {
                outcome = Err(err);
            }
        }
        outcome
    };

    match undone {
        Ok(()) => info!(
            account = %user.username,
            created,
            "Undid a service onboarding that failed part-way."
        ),
        Err(err) => {
            error!(error = %err, account = %user.username, "Could not undo a service onboarding that failed part-way.");
            context.session().record_human_error(&err);
        }
    }
}
