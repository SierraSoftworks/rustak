//! When to order, and what one run of the renewal actually does.
//!
//! # The decision is separate from the work
//!
//! [`decide`] is a pure function of the stored row, `[acme] renew_before` and
//! the clock, so every rule about when a certificate is replaced can be tested
//! without an authority, a listener or a database. [`run`] is what carries the
//! decision out.
//!
//! # Back-off
//!
//! A failed order is retried after an hour, then four, then daily. The
//! authority's rate limits are the reason: a name whose DNS is not ready yet
//! will fail every hour for as long as it takes somebody to notice, and a
//! deployment that hammers Let's Encrypt at that rate is locked out of the
//! production directory for a week. The counter lives in the row, so the
//! back-off survives a restart — which is when an operator is most likely to
//! be retrying by hand.

use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use rustak_api::{AuditCategory, AuditOutcome, TlsCertificateState, TlsSource, TlsStatus};

use crate::config::TlsMode;
use crate::db::AuditEntry;
use crate::pki::tls::HotSwapCertResolver;
use crate::prelude::*;

use super::challenge::Responder;
use super::store::{self, AcmeCertificateRow};
use super::{account, order};

/// What the public listener is presenting, and how healthy it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertState {
    /// No certificate has been issued for these names yet.
    Missing,
    /// Issued, and not yet inside its renewal window.
    Valid { not_after: DateTime<Utc> },
    /// Issued, and due for renewal — or already past its expiry.
    Expiring { not_after: DateTime<Utc> },
    /// The last order failed; `attempts` is how many in a row.
    Failed { attempts: i64, last_error: String },
}

/// What one check of the schedule concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Place an order now.
    Order,
    /// The certificate is current; look again at the next tick.
    Wait,
    /// A recent failure is still backing off, until this instant.
    BackOff { until: DateTime<Utc> },
}

/// How long to wait after `attempts` consecutive failures.
///
/// An hour, four hours, then daily: long enough that a misconfigured name does
/// not spend an installation's rate-limit allowance, short enough that fixing
/// the DNS is followed by a working certificate the same morning.
pub fn backoff(attempts: i64) -> TimeDelta {
    match attempts {
        ..=1 => TimeDelta::hours(1),
        2 => TimeDelta::hours(4),
        _ => TimeDelta::hours(24),
    }
}

/// Whether to order now, given what is stored.
pub fn decide(
    stored: Option<&AcmeCertificateRow>,
    renew_before: TimeDelta,
    now: DateTime<Utc>,
) -> Decision {
    let Some(row) = stored else {
        return Decision::Order;
    };

    let due = match row.not_after {
        Some(not_after) if row.is_issued() => not_after - now <= renew_before,
        // A reserved row with no certificate in it: the first order never
        // finished, so this is still the first order.
        _ => true,
    };

    if !due {
        return Decision::Wait;
    }

    // The back-off applies to the retry, not to the first attempt: `attempts`
    // is reset to zero by every success.
    match row.last_attempt_at {
        Some(last) if row.attempts > 0 => {
            let until = last + backoff(row.attempts);

            if until > now {
                Decision::BackOff { until }
            } else {
                Decision::Order
            }
        }
        _ => Decision::Order,
    }
}

/// How the stored row should be described to an administrator.
pub fn state(
    stored: Option<&AcmeCertificateRow>,
    renew_before: TimeDelta,
    now: DateTime<Utc>,
) -> CertState {
    let Some(row) = stored else {
        return CertState::Missing;
    };

    if row.attempts > 0 {
        return CertState::Failed {
            attempts: row.attempts,
            last_error: row
                .last_error
                .clone()
                .unwrap_or_else(|| "The last ACME order failed.".to_string()),
        };
    }

    match row.not_after {
        Some(not_after) if row.is_issued() => match not_after - now <= renew_before {
            true => CertState::Expiring { not_after },
            false => CertState::Valid { not_after },
        },
        _ => CertState::Missing,
    }
}

/// When the next renewal is due, which is what the admin API reports.
pub fn next_renewal(
    stored: Option<&AcmeCertificateRow>,
    renew_before: TimeDelta,
) -> Option<DateTime<Utc>> {
    let row = stored?;
    let not_after = row.not_after.filter(|_| row.is_issued())?;

    Some(not_after - renew_before)
}

/// What `GET /api/v1/settings/tls` reports.
///
/// Every source answers, not only ACME: an administrator asking what the
/// listener is presenting should be told "a certificate from this
/// installation's own authority" rather than nothing at all.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the stored row cannot be read.
pub async fn status(services: &impl Services) -> Result<TlsStatus, Error> {
    let config = services.config();

    let source = match config.web.public.tls.mode {
        TlsMode::Internal => TlsSource::Internal,
        TlsMode::Files => TlsSource::Files,
        TlsMode::Acme => TlsSource::Acme,
        TlsMode::None => TlsSource::None,
    };

    if source != TlsSource::Acme {
        return Ok(TlsStatus {
            domains: config.server.domains.clone(),
            ..TlsStatus::fixed(source)
        });
    }

    let domains = store::normalise(config.acme.domains(&config.server));
    let stored = store::load(services.db(), &domains).await?;
    let held = stored.as_ref();
    let now = Utc::now();

    Ok(TlsStatus {
        source,
        state: match state(held, config.acme.renew_before, now) {
            CertState::Missing => TlsCertificateState::Missing,
            CertState::Valid { .. } => TlsCertificateState::Valid,
            CertState::Expiring { .. } => TlsCertificateState::Expiring,
            CertState::Failed { .. } => TlsCertificateState::Failed,
        },
        domains: domains.clone(),
        not_before: held
            .filter(|row| row.is_issued())
            .and_then(|row| row.not_before),
        not_after: held
            .filter(|row| row.is_issued())
            .and_then(|row| row.not_after),
        renews_at: next_renewal(held, config.acme.renew_before),
        directory: Some(config.acme.directory.to_string()),
        challenge: held
            .and_then(|row| row.challenge_type)
            .map(|challenge| challenge.as_str().to_string()),
        attempts: held.map_or(0, |row| u32::try_from(row.attempts).unwrap_or(u32::MAX)),
        last_attempt_at: held.and_then(|row| row.last_attempt_at),
        last_error: held.and_then(|row| row.last_error.clone()),
        // The fields only `mode = "files"` fills in — the paths it reads and
        // when it last read them. An order has no files behind it.
        ..TlsStatus::fixed(source)
    })
}

/// Runs one renewal: decide, order, store, and swap the listener over.
///
/// `resolver` is the public listener's, when `[web.public.tls] mode = "acme"`
/// installed one; without it a `tls-alpn-01` challenge cannot be answered and
/// a new certificate cannot be served until the next restart, both of which
/// are said rather than assumed.
///
/// `forced` skips the schedule — that is the admin API's "renew now" — but not
/// the ordering itself, so a forced run against a healthy certificate does
/// place a real order and does spend rate limit.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when ACME is not configured, and a
/// [`human_errors::Kind::System`] error carrying the authority's own words when
/// an order fails. The failure is recorded on the row before it is returned, so
/// the back-off applies to the next attempt either way.
#[instrument("pki.acme.renew", skip_all, fields(forced), err(Display))]
pub async fn run(
    services: &impl Services,
    resolver: Option<Arc<HotSwapCertResolver>>,
    forced: bool,
) -> Result<CertState, Error> {
    let config = services.config();
    let acme = &config.acme;

    if !acme.enabled {
        return Err(human_errors::user(
            "ACME is not switched on, so there is no certificate to renew.",
            &[
                "Set `[acme] enabled = true` and `[web.public.tls] mode = \"acme\"`.",
                "Restart the server after changing the configuration file.",
            ],
        ));
    }

    let domains = store::normalise(acme.domains(&config.server));

    if domains.is_empty() {
        return Err(human_errors::user(
            "ACME has no names to order a certificate for.",
            &[
                "Set `[acme] domains`, or `[server] domains`, to the public host names this server answers to.",
            ],
        ));
    }

    let stored = store::load(services.db(), &domains).await?;
    let now = Utc::now();

    if !forced {
        match decide(stored.as_ref(), acme.renew_before, now) {
            Decision::Wait => return Ok(state(stored.as_ref(), acme.renew_before, now)),
            Decision::BackOff { until } => {
                debug!(%until, "An ACME order failed recently; waiting before the next attempt.");

                return Ok(state(stored.as_ref(), acme.renew_before, now));
            }
            Decision::Order => {}
        }
    }

    let id = store::reserve(services.db(), &domains).await?;

    match place_order(services, resolver, &domains, id).await {
        Ok(state) => Ok(state),
        Err(err) => {
            let attempts = store::record_failure(services.db(), id, &err.description()).await?;

            warn!(
                attempts,
                retry_in = %backoff(attempts),
                error = %err,
                "The ACME order failed; the certificate in place is unchanged."
            );

            services
                .audit()
                .record(
                    AuditEntry::new(
                        AuditCategory::Pki,
                        "acme.renew.failed",
                        AuditOutcome::Failure,
                    )
                    .subject(domains.join(", "))
                    .message(err.description()),
                )
                .await?;

            Err(err)
        }
    }
}

/// The ordering half of [`run`], separated so that every failure in it is
/// recorded against the row by exactly one piece of code.
async fn place_order(
    services: &impl Services,
    resolver: Option<Arc<HotSwapCertResolver>>,
    domains: &[String],
    id: i64,
) -> Result<CertState, Error> {
    let config = services.config();
    let acme = &config.acme;

    let account = account::ensure(
        services.db(),
        services.secrets(),
        acme,
        services.http_client(),
    )
    .await?;

    let responder = Responder::new(resolver.clone());
    let issued = order::place(
        &account,
        domains,
        acme.challenge,
        &responder,
        config.pki.key_type,
    )
    .await?;

    let not_after = issued.not_after;

    store::store(
        services.db(),
        services.secrets(),
        id,
        issued.chain_pem,
        &issued.key_pkcs8,
        (issued.not_before, not_after),
        issued.challenge,
    )
    .await?;

    super::install(
        services.db(),
        services.secrets(),
        domains,
        resolver.as_ref(),
    )
    .await?;

    services
        .audit()
        .record(
            AuditEntry::new(AuditCategory::Pki, "acme.issued", AuditOutcome::Success)
                .subject(domains.join(", "))
                .message(format!("Valid until {not_after}."))
                .detail(serde_json::json!({
                    "challenge": issued.challenge.as_str(),
                    "directory": acme.directory.to_string(),
                })),
        )
        .await?;

    Ok(CertState::Valid { not_after })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::AcmeChallenge;
    use crate::db::Database;

    async fn row(chain: bool, not_after: Option<DateTime<Utc>>) -> AcmeCertificateRow {
        let db = Database::open_in_memory().await.unwrap();
        let secrets = crate::crypto::SecretStore::ephemeral();
        let domains = vec!["tak.example.com".to_string()];
        let id = store::reserve(&db, &domains).await.unwrap();

        if chain {
            let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
            let certificate = rcgen::CertificateParams::new(domains.clone())
                .unwrap()
                .self_signed(&key)
                .unwrap();

            store::store(
                &db,
                &secrets,
                id,
                certificate.pem(),
                &key.serialize_der(),
                (Utc::now(), not_after.unwrap_or_else(Utc::now)),
                AcmeChallenge::Http01,
            )
            .await
            .unwrap();
        }

        store::load(&db, &domains).await.unwrap().unwrap()
    }

    fn renew_before() -> TimeDelta {
        TimeDelta::days(30)
    }

    #[tokio::test]
    async fn nothing_stored_means_order_now() {
        assert_eq!(decide(None, renew_before(), Utc::now()), Decision::Order);
        assert_eq!(state(None, renew_before(), Utc::now()), CertState::Missing);
    }

    #[tokio::test]
    async fn a_reserved_row_that_never_got_a_certificate_is_still_the_first_order() {
        let reserved = row(false, None).await;

        assert_eq!(
            decide(Some(&reserved), renew_before(), Utc::now()),
            Decision::Order,
        );
        assert_eq!(
            state(Some(&reserved), renew_before(), Utc::now()),
            CertState::Missing,
        );
    }

    #[tokio::test]
    async fn a_fresh_certificate_is_left_alone() {
        let now = Utc::now();
        let issued = row(true, Some(now + TimeDelta::days(90))).await;

        assert_eq!(decide(Some(&issued), renew_before(), now), Decision::Wait);
        assert!(matches!(
            state(Some(&issued), renew_before(), now),
            CertState::Valid { .. }
        ));
    }

    #[tokio::test]
    async fn a_certificate_inside_its_renewal_window_is_ordered_again() {
        let now = Utc::now();
        let issued = row(true, Some(now + TimeDelta::days(29))).await;

        assert_eq!(decide(Some(&issued), renew_before(), now), Decision::Order);
        assert!(matches!(
            state(Some(&issued), renew_before(), now),
            CertState::Expiring { .. }
        ));
    }

    #[tokio::test]
    async fn the_boundary_is_inclusive_so_the_window_is_never_skipped() {
        let now = Utc::now();
        let issued = row(true, Some(now + renew_before())).await;

        assert_eq!(decide(Some(&issued), renew_before(), now), Decision::Order);
    }

    #[tokio::test]
    async fn an_expired_certificate_is_renewed_rather_than_reported_as_valid() {
        let now = Utc::now();
        let issued = row(true, Some(now - TimeDelta::days(1))).await;

        assert_eq!(decide(Some(&issued), renew_before(), now), Decision::Order);
        assert!(matches!(
            state(Some(&issued), renew_before(), now),
            CertState::Expiring { .. }
        ));
    }

    #[tokio::test]
    async fn a_recent_failure_backs_off_before_trying_again() {
        let now = Utc::now();
        let mut failed = row(false, None).await;
        failed.attempts = 1;
        failed.last_attempt_at = Some(now - TimeDelta::minutes(10));

        assert!(matches!(
            decide(Some(&failed), renew_before(), now),
            Decision::BackOff { .. },
        ));

        failed.last_attempt_at = Some(now - TimeDelta::hours(2));
        assert_eq!(decide(Some(&failed), renew_before(), now), Decision::Order);

        failed.attempts = 3;
        assert!(matches!(
            decide(Some(&failed), renew_before(), now),
            Decision::BackOff { .. },
        ));
    }

    #[test]
    fn the_back_off_grows_and_then_stops_growing() {
        assert_eq!(backoff(0), TimeDelta::hours(1));
        assert_eq!(backoff(1), TimeDelta::hours(1));
        assert_eq!(backoff(2), TimeDelta::hours(4));
        assert_eq!(backoff(3), TimeDelta::hours(24));
        assert_eq!(backoff(50), TimeDelta::hours(24));
    }

    #[tokio::test]
    async fn a_failure_is_what_an_administrator_is_shown_even_while_the_old_certificate_serves() {
        let now = Utc::now();
        let mut failed = row(true, Some(now + TimeDelta::days(90))).await;
        failed.attempts = 2;
        failed.last_error = Some("dns problem: NXDOMAIN".to_string());

        assert_eq!(
            state(Some(&failed), renew_before(), now),
            CertState::Failed {
                attempts: 2,
                last_error: "dns problem: NXDOMAIN".to_string(),
            },
        );
    }

    #[tokio::test]
    async fn the_next_renewal_is_the_expiry_less_the_window() {
        let now = Utc::now();
        let issued = row(true, Some(now + TimeDelta::days(90))).await;

        let due = next_renewal(Some(&issued), renew_before()).unwrap();

        assert!(due > now + TimeDelta::days(59));
        assert!(due < now + TimeDelta::days(61));
        assert_eq!(next_renewal(None, renew_before()), None);
    }

    #[tokio::test]
    async fn an_installation_that_is_not_using_acme_still_says_what_it_presents() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        let reported = status(&context).await.unwrap();

        // The mock's default is the internal authority, which is the default
        // an installation that writes an empty configuration file gets.
        assert_eq!(reported.source, TlsSource::Internal);
        assert_eq!(reported.state, TlsCertificateState::Valid);
        assert!(reported.directory.is_none());
        assert!(!reported.needs_attention());
    }

    #[tokio::test]
    async fn an_acme_installation_before_its_first_order_reports_what_it_will_ask_for() {
        let context = AppContext::new_mock(|config| {
            config.web.public.tls.mode = TlsMode::Acme;
            config.acme.enabled = true;
            config.acme.domains = vec!["TAK.example.com.".to_string()];
        })
        .await
        .unwrap();

        let reported = status(&context).await.unwrap();

        assert_eq!(reported.source, TlsSource::Acme);
        assert_eq!(reported.state, TlsCertificateState::Missing);
        assert_eq!(reported.domains, vec!["tak.example.com".to_string()]);
        assert_eq!(reported.directory.as_deref(), Some("letsencrypt"));
        assert!(reported.needs_attention());
    }

    #[tokio::test]
    async fn a_failed_order_is_reported_with_the_authoritys_own_words() {
        let context = AppContext::new_mock(|config| {
            config.web.public.tls.mode = TlsMode::Acme;
            config.acme.enabled = true;
            config.acme.domains = vec!["tak.example.com".to_string()];
        })
        .await
        .unwrap();

        let domains = vec!["tak.example.com".to_string()];
        let id = store::reserve(context.db(), &domains).await.unwrap();
        store::record_failure(context.db(), id, "dns problem: NXDOMAIN")
            .await
            .unwrap();

        let reported = status(&context).await.unwrap();

        assert_eq!(reported.state, TlsCertificateState::Failed);
        assert_eq!(reported.attempts, 1);
        assert_eq!(
            reported.last_error.as_deref(),
            Some("dns problem: NXDOMAIN")
        );
    }

    #[tokio::test]
    async fn renewing_without_acme_configured_says_which_keys_to_set() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        let refused = run(&context, None, true).await.unwrap_err();

        assert!(refused.is(human_errors::Kind::User));
        assert!(refused.description().contains("ACME is not switched on"));
        assert!(refused.to_string().contains("[acme] enabled"));
    }

    #[tokio::test]
    async fn renewing_with_no_names_configured_never_reaches_the_authority() {
        // Otherwise a forced renewal would reserve a row keyed on the empty
        // list and place an order for nothing.
        let context = AppContext::new_mock(|config| {
            config.web.public.tls.mode = TlsMode::Acme;
            config.acme.enabled = true;
            config.acme.accept_tos = true;
            config.server.domains = Vec::new();
            config.acme.domains = Vec::new();
        })
        .await
        .unwrap();

        let refused = run(&context, None, true).await.unwrap_err();

        assert!(refused.is(human_errors::Kind::User));
        assert!(refused.description().contains("no names"));
    }
}
