//! Onboarding CloudTAK: the one flow in which this server generates, holds and
//! hands over a client's private key.
//!
//! # Why the exception exists
//!
//! Everywhere else rustak issues a certificate against a signing request a
//! device made, and the key stays on the device — which is why
//! `POST /api/v1/config-packages` refuses `include_client_cert` outright.
//! CloudTAK cannot enrol: its *Configure Server* page takes an administrator
//! certificate as an uploaded `.p12` and will not save without one, alongside a
//! username and password for the same account (`compat/cloudtak.md` §2). The
//! only way to satisfy it today is an operator running `openssl` by hand
//! against the enrolment endpoint and re-wrapping the result with
//! `pkcs12 -export -legacy`, which is a recipe nobody gets right twice and
//! which leaves the key wherever the shell left it.
//!
//! # What keeps the exception narrow
//!
//! Five properties, each with a test that fails if it is relaxed:
//!
//! 1. **Administrator only.** The route takes `Administrative`, and the
//!    download takes it again — a session is not a bearer of the bundle.
//! 2. **Audited at both ends.** `cloudtak.onboarding.created` names the
//!    account, the credential, the certificate and the administrator;
//!    `cloudtak.onboarding.downloaded` names who fetched it.
//! 3. **One shot.** The bundle is taken with a single delete-returning write,
//!    so of two simultaneous fetches exactly one gets bytes and the other gets
//!    `410`.
//! 4. **Encrypted twice, stored once.** The bundle is a PKCS#12 encrypted with
//!    a passphrase generated for this hand-over, and *that* is sealed with the
//!    installation's key before it is written. The passphrase itself is
//!    returned in the creation response and stored nowhere — not beside the
//!    bundle, not on a row, not in the audit entry — so a database dump plus
//!    the sealing key yields an encrypted file and not a usable credential.
//!    The stash is swept after ten minutes whether or not it was collected,
//!    and the certificate row keeps no key at all.
//! 5. **Never logged.** The key, the passphrase and the password are carried in
//!    types that redact themselves, and nothing here formats them.
//!
//! The certificate itself is ordinary: same subject, same `pki::issue` path,
//! same row, so `/api/v1/certificates` lists it and
//! `POST /certificates/{id}/revoke` ends it like any other.
//!
//! # The shape of this module
//!
//! This file is the flow: resolve a credential, generate a key, issue against
//! it, hand the result back. `bundle` is the interval between preparing a
//! keystore and collecting it — sealing, the one-shot take and the sweep — and
//! `urls` is where the three URLs CloudTAK stores come from. Both are private:
//! everything the rest of the server needs is re-exported here.

mod bundle;
mod urls;

use chrono::Utc;
use rustak_api::cloudtak::DEFAULT_CREDENTIAL_LABEL;
use rustak_api::{AuditCategory, AuditOutcome, CloudTakOnboarding, CloudTakOnboardingRequest};

use crate::db::AuditEntry;
use crate::db::repos::{CredentialRow, UserRow};
use crate::pki::{Enrollment, IssuedVia, KeyType};
use crate::prelude::*;

pub use bundle::{BUNDLE_PARTITION, Bundle, stash, sweep, take};
pub use urls::{compose as compose_urls, default_host};

/// The path a prepared bundle is fetched from, relative to the origin.
///
/// Relative rather than absolute on purpose: the administrator's browser is
/// already talking to this server, and an absolute URL built from a configured
/// host would be one that has to resolve from the browser as well as from
/// CloudTAK — which is exactly the mismatch the `host` override exists for.
const DOWNLOAD_PATH: &str = "/api/v1/cloudtak-onboarding";

/// Bytes of entropy behind the generated PKCS#12 passphrase: sixteen base64url
/// characters, ninety-six bits.
///
/// Nothing like the shared `atakatak` every other bundle uses. This one is
/// generated per hand-over, it is what actually protects the private key
/// between here and CloudTAK's import dialog, and it exists in exactly two
/// places: the response that is about to be rendered, and the head of the
/// person reading it. It is never written down, which is what makes the sealed
/// stash defence in depth rather than the only defence.
const PASSPHRASE_BYTES: usize = 12;

/// What the bundle's keychain is called inside the file.
const FRIENDLY_NAME: &str = "cloudtak";

/// The device identity the certificate is recorded against.
///
/// CloudTAK works its own connection uid out from the certificate's subject
/// (`compat/cloudtak.md` §6), so this is only what the administrator sees in
/// the certificate list — and seeing which certificate went to CloudTAK is the
/// point of recording it.
const CLIENT_UID: &str = "cloudtak";

/// Everything one hand-over needs.
#[derive(Debug)]
pub struct Onboarding<'a> {
    /// The account CloudTAK will sign in as.
    pub user: &'a UserRow,

    /// The administrator asking, for the audit trail.
    pub actor: &'a Username,

    /// What the caller asked for.
    pub request: &'a CloudTakOnboardingRequest,
}

/// Mints or checks a credential, issues a certificate, and stashes the bundle.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the named credential is not a live
/// client password for this account, when client passwords are switched off, or
/// when the host is not one that can go in a URL; a
/// [`human_errors::Kind::System`] error when the authority, the key generation
/// or a write fails.
#[instrument("identity.cloudtak.onboard", skip_all, fields(user = %onboarding.user.username), err(Display))]
pub async fn onboard(
    context: &AppContext,
    onboarding: Onboarding<'_>,
) -> Result<CloudTakOnboarding, Error> {
    let Onboarding {
        user,
        actor,
        request,
    } = onboarding;

    let config = context.config();
    // Composed first: a refused host must not leave a minted credential and an
    // issued certificate behind for an operator who mistyped a name.
    let urls = urls::compose(&config, request.host.as_deref(), request.ports.as_ref())?;
    let pki = context.pki()?;

    let (credential, password) = credential(context, user, actor, request).await?;
    let minted = password.is_some();

    // RSA because every TAK client and `@tak-ps/node-p12` reads it, and off the
    // async worker because generating one is hundreds of milliseconds of bignum
    // arithmetic.
    let username = user.username.clone();
    let generated = tokio::task::spawn_blocking(move || {
        crate::pki::issue::generate_signing_request(&username, KeyType::Rsa2048)
    })
    .await
    .or_system_err(&["This is unexpected; please report it with the surrounding log entries."])??;

    let issued = pki
        .enroll(
            context.db(),
            Enrollment {
                username: &user.username,
                csr_body: &generated.csr_der,
                client_uid: Some(CLIENT_UID),
                user_id: Some(user.id),
                device_id: None,
                credential_id: Some(credential.id),
                issued_via: IssuedVia::CloudTakOnboarding,
                channels_capable: true,
            },
        )
        .await?;

    let certificate_id = context
        .db()
        .certificates()
        .get_by_fingerprint(&issued.fingerprint)
        .await?
        .ok_or_else(|| {
            human_errors::system(
                "A certificate was issued for CloudTAK but no record of it was written.",
                &["This is unexpected; please report it with the surrounding log entries."],
            )
        })?
        .id;

    // Generated here, used to encrypt the bundle, returned once, and never
    // stored: `bundle::stash` is handed the already-encrypted bytes and has no
    // way to learn what opened them.
    let passphrase = generate_token(PASSPHRASE_BYTES);
    let chain = pki.chain();
    let links: Vec<&[u8]> = chain.iter().map(|link| link.as_ref()).collect();

    let p12 = crate::pki::client_keystore(
        &generated.key_pkcs8,
        issued.der.as_ref(),
        &links,
        &crate::pki::P12Options::handover(passphrase.expose(), FRIENDLY_NAME),
    )?;

    let (id, expires_at) = bundle::stash(context, &user.username, certificate_id, p12).await?;

    record(context, user, actor, &credential, certificate_id, minted).await;

    Ok(CloudTakOnboarding {
        username: user.username.clone(),
        password: password.map(|secret| secret.expose().to_string()),
        p12_download_url: format!("{DOWNLOAD_PATH}/{id}.p12"),
        p12_password: passphrase.expose().to_string(),
        urls,
        certificate_id,
        credential_id: credential.id,
        expires_at,
    })
}

/// Mints a client password, or checks the one the caller named.
///
/// The secret comes back only from minting: a reused credential's is stored as
/// an argon2id hash and nothing here can produce it again, which is the same
/// reason `GET /credentials/{id}/enroll-url` leaves the token out.
async fn credential(
    context: &AppContext,
    user: &UserRow,
    actor: &Username,
    request: &CloudTakOnboardingRequest,
) -> Result<(CredentialRow, Option<Secret>), Error> {
    let Some(id) = request.credential.existing() else {
        let label = request
            .label
            .as_deref()
            .map_or(DEFAULT_CREDENTIAL_LABEL, str::trim);
        let label = if label.is_empty() {
            DEFAULT_CREDENTIAL_LABEL
        } else {
            label
        };

        let minted = super::credentials::mint(
            context.db(),
            &context.config().auth,
            user,
            super::credentials::MintRequest::new(CredentialKind::ClientPassword, label, actor),
        )
        .await?;

        return Ok((minted.credential, Some(minted.secret)));
    };

    Ok((existing(context, id, user).await?, None))
}

/// The named credential, if it is a live client password for this account.
async fn existing(
    context: &AppContext,
    id: CredentialId,
    user: &UserRow,
) -> Result<CredentialRow, Error> {
    let credential = context
        .db()
        .credentials()
        .get(id)
        .await?
        .filter(|credential| credential.user_id == user.id)
        .ok_or_else(|| {
            human_errors::user(
                "That account has no such credential.",
                &["Mint a new client password instead, or name one of this account's."],
            )
        })?;

    if credential.kind != CredentialKind::ClientPassword {
        return Err(human_errors::user(
            "CloudTAK signs in with a client password, not with that kind of credential.",
            &["Mint a client password for this account, or name one it already has."],
        ));
    }

    if credential.revoked_at.is_some() || credential.expires_at.is_some_and(|at| at <= Utc::now()) {
        return Err(human_errors::user(
            "That credential has expired or been revoked, so CloudTAK could not sign in with it.",
            &["Mint a new client password for this account."],
        ));
    }

    Ok(credential)
}

/// Writes who prepared a hand-over, for whom, and what it carries.
///
/// Best effort, like every other audit write in a handler: the administrator
/// already holds the bundle by this point, and failing the request would leave
/// a certificate issued and no way to say so.
async fn record(
    context: &AppContext,
    user: &UserRow,
    actor: &Username,
    credential: &CredentialRow,
    certificate_id: CertificateId,
    minted: bool,
) {
    let entry = AuditEntry::new(
        AuditCategory::Administration,
        "cloudtak.onboarding.created",
        AuditOutcome::Success,
    )
    .subject(&user.username)
    .actor(actor)
    .message(format!(
        "A CloudTAK hand-over was prepared for {}; the private key was generated here for one \
         download and kept nowhere else.",
        user.username
    ))
    .detail(serde_json::json!({
        "credential_id": credential.id,
        "credential_label": credential.label,
        "certificate_id": certificate_id,
        "minted_credential": minted,
    }));

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a CloudTAK hand-over in the audit log.");
        context.session().record_human_error(&err);
    }
}

/// Writes who collected one.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the write fails; the caller logs
/// it rather than failing a download that has already succeeded.
pub async fn record_download(
    context: &AppContext,
    bundle: &Bundle,
    actor: &Username,
) -> Result<(), Error> {
    context
        .db()
        .record(
            AuditEntry::new(
                AuditCategory::Administration,
                "cloudtak.onboarding.downloaded",
                AuditOutcome::Success,
            )
            .subject(&bundle.username)
            .actor(actor)
            .message(format!(
                "The CloudTAK keystore prepared for {} was downloaded and deleted.",
                bundle.username
            ))
            .detail(serde_json::json!({
                "certificate_id": bundle.certificate_id,
            })),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passphrase_is_sixteen_characters_and_never_the_same_twice() {
        let first = generate_token(PASSPHRASE_BYTES);

        assert_eq!(first.len(), 16);
        assert_ne!(
            first.expose(),
            generate_token(PASSPHRASE_BYTES).expose(),
            "generated per hand-over, never shared",
        );
    }

    #[test]
    fn the_download_path_is_relative_to_this_origin() {
        // An absolute URL would have to resolve from the administrator's
        // browser as well, which is exactly what the `host` override is for.
        assert!(DOWNLOAD_PATH.starts_with('/'));
        assert!(!DOWNLOAD_PATH.contains("://"));
    }
}
