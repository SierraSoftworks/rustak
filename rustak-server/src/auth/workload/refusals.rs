//! Saying that an assertion was refused — once, quietly, and never with the
//! request's headers attached.
//!
//! # What this replaces
//!
//! `#[instrument(… err(Debug))]` on the verifier, which logged **every**
//! refusal at `ERROR`, inside the request span, with every header the request
//! carried. One sidecar holding a credential it could not renew therefore
//! produced `ERROR … auth.workload.verify: error=Claims` plus a block of
//! headers every five seconds, for hours: 137 lines an hour, per sidecar, none
//! of which said what was wrong and all of which looked like the server
//! failing. It was not failing. It was refusing, correctly.
//!
//! So a refusal is:
//!
//! * `warn`, never `error` — a caller presenting something we will not take is
//!   not this server failing;
//! * detached from the request span (`parent: None`), because the headers of
//!   the request that carried a refused credential are not the subject;
//! * counted per `(issuer, subject, reason)`, so one broken deployment is one
//!   line every five minutes rather than one line per attempt, and a *second*
//!   broken deployment is still heard over the first.
//!
//! # Nothing of the token goes into it
//!
//! The issuer and the subject are read from the token unverified, so they are
//! tidied ([`super::verify`] does the same) and they are all that is kept. The
//! assertion, its signature and every other claim stay out of the log, as they
//! always have.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use chrono::{DateTime, Duration, Utc};

use crate::prelude::*;

use super::Refusal;

/// How often a refusal that keeps happening is mentioned again.
const REMIND_EVERY: Duration = Duration::minutes(5);

/// The most `(issuer, subject, reason)` triples counted at once.
///
/// A ceiling rather than a leak: the issuer and subject are read off the wire,
/// so a caller who varies them could otherwise grow this map for ever.
const MAX_KEYS: usize = 512;

/// The key a refusal is counted under.
type Key = (String, String, &'static str);

/// Whether the caller said the credential was a workload assertion.
///
/// It decides the *volume*, never the decision. `grant_type=jwt-bearer` can
/// only ever have meant one thing, so anything wrong with what came with it is
/// worth a `warn`. An `Authorization` header on an enrolment route is a
/// different matter: every other credential rustak takes arrives the same way,
/// and an ATAK client presenting an ordinary enrolment token would otherwise
/// earn a warning apiece for the crime of not being a JWT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presented {
    /// The caller asked for the `jwt-bearer` grant by name.
    Deliberately,

    /// A header on a route that takes other credentials too.
    Perhaps,
}

/// What a refusal is worth saying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Notice {
    /// The first of its kind for a while: one `warn`.
    First,

    /// The same refusal again: `debug`, and a tally.
    Repeat,

    /// Five minutes of the same refusal: one `warn`, with the tally.
    Reminder { repeats: u32 },
}

/// When a refusal was last mentioned, and how often since.
#[derive(Clone, Copy, Debug)]
struct Seen {
    mentioned_at: DateTime<Utc>,
    repeats: u32,
}

/// Every refusal this process has mentioned lately.
static SEEN: LazyLock<Mutex<HashMap<Key, Seen>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Records a refusal and answers what to say about it.
///
/// The pure half, so the cadence can be tested by moving the clock rather than
/// by reading a log back.
fn note(seen: &mut HashMap<Key, Seen>, key: Key, now: DateTime<Utc>) -> Notice {
    if let Some(held) = seen.get_mut(&key) {
        if now - held.mentioned_at < REMIND_EVERY {
            held.repeats = held.repeats.saturating_add(1);

            return Notice::Repeat;
        }

        let repeats = held.repeats;
        held.mentioned_at = now;
        held.repeats = 0;

        return Notice::Reminder { repeats };
    }

    prune(seen, now);
    seen.insert(
        key,
        Seen {
            mentioned_at: now,
            repeats: 0,
        },
    );

    Notice::First
}

/// Forgets what nobody has presented for two reminder windows.
///
/// Only ever called when a key is about to be added, so the cost is paid by the
/// thing that grows the map. A map that is still full afterwards is emptied:
/// losing a throttle is a few extra lines, and keeping one is unbounded memory
/// under something that is already misbehaving.
fn prune(seen: &mut HashMap<Key, Seen>, now: DateTime<Utc>) {
    if seen.len() < MAX_KEYS {
        return;
    }

    seen.retain(|_, held| now - held.mentioned_at < REMIND_EVERY * 2);

    if seen.len() >= MAX_KEYS {
        seen.clear();
    }
}

/// Says that an assertion was refused, at the level this run of them calls for.
///
/// `token` is read **unverified** and only for the two fields that identify
/// which deployment is holding the bad credential.
pub(super) fn announce(token: &str, refusal: &Refusal, presented: Presented) {
    // Ours, not theirs: whoever could not do the checking says so itself.
    if matches!(refusal, Refusal::Unavailable(_)) {
        return;
    }

    // A credential that is not a JWT at all, or that names an issuer this
    // server never heard of, is not a workload assertion going wrong — it is
    // something else entirely, on a route that takes something else. The
    // enrolment path carries on with it, and neither of us needed telling.
    if presented == Presented::Perhaps && not_ours(refusal) {
        debug!(
            reason = refusal.reason(),
            "A credential on an enrolment route is not a workload assertion; carrying on with it.",
        );

        return;
    }

    let (issuer, subject) = whose(token);
    let reason = refusal.reason();
    let key = (issuer.clone(), subject.clone(), reason);

    let notice = match SEEN.lock() {
        Ok(mut seen) => note(&mut seen, key, Utc::now()),
        // A poisoned lock is not a reason to swallow a refusal.
        Err(_) => Notice::First,
    };

    match notice {
        Notice::First => warn!(
            parent: None,
            issuer = %issuer,
            subject = %subject,
            reason,
            "Refused a workload identity: {}. Repeats of this one are logged at debug, with a count every five minutes.",
            refusal.sentence(),
        ),
        Notice::Repeat => debug!(
            parent: None,
            issuer = %issuer,
            subject = %subject,
            reason,
            "Refused a workload identity: {}.",
            refusal.sentence(),
        ),
        Notice::Reminder { repeats } => warn!(
            parent: None,
            issuer = %issuer,
            subject = %subject,
            reason,
            repeats,
            "Refused a workload identity {repeats} more times in the last five minutes: {}.",
            refusal.sentence(),
        ),
    }
}

/// Says that a credential was refused before anybody looked at it.
///
/// The rate limiter answers ahead of the verifier, so nothing else here would
/// ever hear about it — and that is a real diagnosis gap: a deployment that
/// spent 45 minutes presenting an expired assertion earned a lockout that
/// outlived the process, and the *next* start's first exchange was refused with
/// `429` while this server logged nothing at all. Same throttle, same shape.
pub(super) fn throttled(token: &str, retry_after: chrono::Duration) {
    let (issuer, subject) = whose(token);
    let key = (issuer.clone(), subject.clone(), "rate-limited");

    let notice = match SEEN.lock() {
        Ok(mut seen) => note(&mut seen, key, Utc::now()),
        Err(_) => Notice::First,
    };

    let seconds = retry_after.num_seconds().max(0);

    match notice {
        Notice::Repeat => debug!(
            parent: None,
            issuer = %issuer,
            subject = %subject,
            "Refused a workload identity: too many attempts from this address.",
        ),
        _ => warn!(
            parent: None,
            issuer = %issuer,
            subject = %subject,
            reason = "rate-limited",
            "Refused a workload identity without checking it: too many attempts from this address, for another {seconds}s. Something is presenting a credential this server will not take, over and over.",
        ),
    }
}

/// Whether a refusal means "this was never one of ours to take".
fn not_ours(refusal: &Refusal) -> bool {
    matches!(
        refusal,
        Refusal::Malformed | Refusal::UnknownIssuer | Refusal::NotConfigured
    )
}

/// Which deployment is holding this credential, as the token itself claims.
///
/// Unverified — a refused token has proved nothing — so this is a label for
/// counting and not a fact. Both values are tidied, because they came off the
/// wire and they are about to be written to a log.
fn whose(token: &str) -> (String, String) {
    (claim(token, "iss"), claim(token, "sub"))
}

/// One string claim of a token's payload, tidied, or `-`.
fn claim(token: &str, name: &str) -> String {
    use base64::Engine as _;

    let read = || -> Option<String> {
        let payload = token.split('.').nth(1)?;
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .ok()?;
        let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;

        Some(
            claims
                .get(name)?
                .as_str()?
                .chars()
                .filter(|character| !character.is_control())
                .take(128)
                .collect(),
        )
    };

    read()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clock these tests move by hand, so that nothing here waits.
    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    fn key(subject: &str) -> Key {
        (
            "https://nomad.example.com".to_string(),
            subject.to_string(),
            "exp",
        )
    }

    #[test]
    fn the_first_refusal_is_the_one_that_is_mentioned() {
        let mut seen = HashMap::new();

        assert_eq!(note(&mut seen, key("ais"), at(0)), Notice::First);
    }

    #[test]
    fn the_same_refusal_again_is_quiet_and_is_counted() {
        // The production finding: one sidecar, one expired assertion, 137
        // ERROR lines an hour with the whole request span attached.
        let mut seen = HashMap::new();

        note(&mut seen, key("ais"), at(0));

        for second in [5, 10, 60, 120, 299] {
            assert_eq!(note(&mut seen, key("ais"), at(second)), Notice::Repeat);
        }

        assert_eq!(
            note(&mut seen, key("ais"), at(300)),
            Notice::Reminder { repeats: 5 },
            "and the reminder says how many it stayed quiet about",
        );
        assert_eq!(
            note(&mut seen, key("ais"), at(305)),
            Notice::Repeat,
            "then it is quiet again until the next window",
        );
    }

    #[test]
    fn a_second_deployment_is_still_heard_over_the_first() {
        // Counted per (issuer, subject, reason): one broken job must not
        // silence the next one to break.
        let mut seen = HashMap::new();

        assert_eq!(note(&mut seen, key("ais"), at(0)), Notice::First);
        assert_eq!(note(&mut seen, key("adsb"), at(1)), Notice::First);
        assert_eq!(
            note(
                &mut seen,
                (
                    "https://nomad.example.com".to_string(),
                    "ais".to_string(),
                    "aud",
                ),
                at(2),
            ),
            Notice::First,
            "a different reason for the same job is a different thing to say",
        );
    }

    #[test]
    fn the_map_cannot_be_grown_for_ever_by_a_caller_that_varies_its_claims() {
        let mut seen = HashMap::new();

        for nth in 0..MAX_KEYS + 10 {
            note(&mut seen, key(&format!("job-{nth}")), at(nth as i64));
        }

        assert!(seen.len() <= MAX_KEYS, "{} keys", seen.len());
    }

    #[test]
    fn a_stale_entry_is_forgotten_rather_than_kept_for_ever() {
        let mut seen = HashMap::new();

        for nth in 0..MAX_KEYS {
            note(&mut seen, key(&format!("job-{nth}")), at(0));
        }

        // Everything above is now older than two windows, so the key that
        // tips the map over its ceiling sweeps them rather than clearing it.
        note(&mut seen, key("later"), at(1_200));

        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn a_credential_that_was_never_a_workload_assertion_is_not_a_warning() {
        // An ATAK client presenting an ordinary enrolment token on the
        // enrolment route must not earn a warning apiece for not being a JWT —
        // that route takes other credentials, and the request carries on.
        for refusal in [
            Refusal::Malformed,
            Refusal::UnknownIssuer,
            Refusal::NotConfigured,
        ] {
            assert!(not_ours(&refusal), "{refusal:?}");
        }

        // Anything that got far enough to fail a check *is* a workload
        // assertion going wrong, wherever it was presented.
        for refusal in [
            Refusal::Algorithm,
            Refusal::Claims(super::super::ClaimRefusal::new("exp", "expired")),
            Refusal::Rule(super::super::RuleRefusal::NoRule),
        ] {
            assert!(!not_ours(&refusal), "{refusal:?}");
        }
    }

    #[test]
    fn the_issuer_and_subject_come_from_the_token_and_nothing_else_does() {
        use base64::Engine as _;

        let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let token = format!(
            "{}.{}.a-signature",
            encoder.encode(br#"{"alg":"RS256","kid":"k1"}"#),
            encoder.encode(
                br#"{"iss":"https://nomad.example.com","sub":"global:default:rustak-plugin-ais:sidecar:ais:rustak","nomad_job_id":"rustak-plugin-ais"}"#
            ),
        );

        assert_eq!(
            whose(&token),
            (
                "https://nomad.example.com".to_string(),
                "global:default:rustak-plugin-ais:sidecar:ais:rustak".to_string(),
            ),
        );
        assert_eq!(
            whose("not-a-jwt"),
            ("-".to_string(), "-".to_string()),
            "a token we cannot take apart identifies nobody",
        );
    }

    #[test]
    fn a_claim_that_came_off_the_wire_cannot_carry_a_log_line_away_with_it() {
        use base64::Engine as _;

        let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let hostile = serde_json::json!({
            "iss": "https://nomad.example.com\n2026-09-22T00:00:00Z ERROR everything is fine",
            "sub": "a".repeat(4_000),
        });
        let token = format!(
            "{}.{}.a-signature",
            encoder.encode(br#"{"alg":"RS256"}"#),
            encoder.encode(hostile.to_string().as_bytes()),
        );

        let (issuer, subject) = whose(&token);

        assert!(!issuer.contains('\n'), "{issuer}");
        assert_eq!(subject.len(), 128, "and nothing unbounded reaches the log");
    }
}
