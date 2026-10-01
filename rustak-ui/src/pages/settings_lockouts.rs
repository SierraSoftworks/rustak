//! Who the sign-in rate limiter is refusing right now, on the Security page.
//!
//! Before this card, "why was I refused?" had no answer short of reading the
//! code: a lockout left nothing behind but the `429` its victim saw (M9-14).
//! Here is every key locked out now — what kind of key, where the failures
//! came from, when it began and ends, about how many failures earned it — and
//! a way to forgive one.
//!
//! The limiter counts in a fixed-size sketch, so the card says two things a
//! table of keys would not need to: a lockout's count is an estimate, and how
//! full each tier is, which decides how often somebody who never failed is
//! refused along with an attacker.
//!
//! Clearing asks first, names the key it is about to forgive, and does one key
//! at a time: a lockout is the limiter doing its job, and the administrator
//! clearing one is vouching for that one caller. The server audits it.

use rustak_api::{Lockout, LockoutClass, LockoutCounter, Lockouts, TierFill};
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, Card, ConfirmButton, EmptyState, LoadingNote, StatusPill, StatusTone,
};
use crate::util::{format_iso8601, short_relative};

use super::load::use_resource;

/// What the card says the key is, in words.
fn describe(lockout: &Lockout) -> String {
    match lockout.class {
        LockoutClass::Account => format!("Account '{}'", lockout.key),
        LockoutClass::Client => format!("OAuth client '{}'", lockout.key),
        LockoutClass::Address => format!("Every '{}' attempt", lockout.key),
        LockoutClass::Source => "Every sign-in".to_string(),
    }
}

/// Where the failures came from: the address, with its prefix when that is
/// more than one IPv4 host.
fn origin(lockout: &Lockout) -> String {
    match (lockout.address, lockout.prefix) {
        (Some(address), Some(prefix)) if address.is_ipv6() => format!("from {address}/{prefix}"),
        (Some(address), _) => format!("from {address}"),
        (None, _) => "from an address the server could not read".to_string(),
    }
}

/// The confirmation, which names exactly what is about to be forgiven.
fn question(lockout: &Lockout) -> String {
    format!(
        "Clear the lockout on {} {}? Its failures are forgotten too, and it can sign in \
         again at once — as can any other key that shared all of its cells.",
        lowercase_first(&describe(lockout)),
        origin(lockout),
    )
}

/// The counters in one line: what has been refused since the server started.
fn counter_summary(counters: &[LockoutCounter]) -> String {
    let refusals: u64 = counters.iter().map(|counter| counter.refusals).sum();
    let lockouts: u64 = counters.iter().map(|counter| counter.lockouts).sum();

    let by_class: Vec<String> = counters
        .iter()
        .filter(|counter| counter.lockouts > 0 || counter.refusals > 0)
        .map(|counter| {
            format!(
                "{} {} / {}",
                counter.class.label().to_lowercase(),
                counter.lockouts,
                counter.refusals,
            )
        })
        .collect();

    match by_class.is_empty() {
        true => "Nothing has been locked out or refused.".to_string(),
        false => format!(
            "{lockouts} lockouts started and {refusals} attempts refused (lockouts / refusals: {}).",
            by_class.join(", "),
        ),
    }
}

/// How full the tiers are, and what that costs somebody who never failed.
fn fill_summary(tiers: &[TierFill]) -> Option<String> {
    let worst = tiers.iter().map(TierFill::false_refusal).reduce(f64::max)?;
    let parts: Vec<String> = tiers
        .iter()
        .map(|tier| {
            format!(
                "{}: {:.1}% of cells locked",
                tier.tier.label(),
                tier.fraction() * 100.0,
            )
        })
        .collect();

    Some(format!(
        "Failure counts are estimates from a fixed-size sketch: never fewer than there \
         were, sometimes more. {}. A caller who never failed is refused about {} times in \
         a million.",
        parts.join("; "),
        (worst * 1_000_000.0).round(),
    ))
}

/// `"Account 'ada'"` → `"account 'ada'"`, for the middle of a sentence.
fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[function_component(LockoutsCard)]
pub fn lockouts_card() -> Html {
    let listed = use_resource(api::lockouts::list);

    let body = match (&listed.data, &listed.error) {
        (None, None) => html! { <LoadingNote /> },
        (None, Some(message)) => html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not read the sign-in lockouts."
                message={message.clone()}
            />
        },
        (Some(lockouts), _) => html! {
            <LockoutList lockouts={lockouts.clone()} on_changed={listed.reload.clone()} />
        },
    };

    html! {
        <Card
            title="Sign-in lockouts"
            subtitle="Addresses, and accounts or endpoints from an address, that the rate \
                limiter is refusing after too many failed attempts. A lockout ends on its \
                own; clear one only for a caller you know."
        >
            { body }
        </Card>
    }
}

#[derive(Properties, PartialEq)]
struct LockoutListProps {
    lockouts: Lockouts,
    on_changed: Callback<()>,
}

#[function_component(LockoutList)]
fn lockout_list(props: &LockoutListProps) -> Html {
    let listed = &props.lockouts;
    let shown = listed.lockouts.len();
    let total = usize::try_from(listed.total).unwrap_or(usize::MAX);

    html! {
        <>
            if listed.lockouts.is_empty() {
                <EmptyState
                    title="Nothing is locked out."
                    message="Every key the limiter has seen is being let through."
                />
            } else {
                <div class="lockout-list">
                    { for listed.lockouts.iter().map(|lockout| html! {
                        <LockoutRow
                            key={format!("{:?}|{:?}|{}", lockout.class, lockout.address, lockout.key)}
                            lockout={lockout.clone()}
                            on_changed={props.on_changed.clone()}
                        />
                    }) }
                </div>
            }

            if total > shown {
                <p class="panel-note">
                    { format!("Showing the {shown} most recent of {total} lockouts.") }
                </p>
            }

            if let Some(fill) = fill_summary(&listed.tiers) {
                <p class="panel-note">{ fill }</p>
            }

            <p class="panel-note" title={format_iso8601(listed.counting_since)}>
                { format!(
                    "Since the server started {}: {}",
                    short_relative(listed.counting_since),
                    counter_summary(&listed.counters),
                ) }
            </p>
        </>
    }
}

#[derive(Properties, PartialEq)]
struct LockoutRowProps {
    lockout: Lockout,
    on_changed: Callback<()>,
}

#[function_component(LockoutRow)]
fn lockout_row(props: &LockoutRowProps) -> Html {
    let lockout = &props.lockout;
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let clear = {
        let (busy, error, on_changed) = (busy.clone(), error.clone(), props.on_changed.clone());
        let lockout = lockout.clone();
        Callback::from(move |()| {
            let (busy, error, on_changed) = (busy.clone(), error.clone(), on_changed.clone());
            let lockout = lockout.clone();
            busy.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                match api::lockouts::clear(&lockout).await {
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
        <div class="lockout-row">
            <div class="lockout-row__identity">
                <span>{ describe(lockout) }</span>
                <span class="lockout-row__origin">{ origin(lockout) }</span>
            </div>

            <div class="lockout-row__meta">
                <span title={format_iso8601(lockout.started_at)}>
                    { format!("Began {}", short_relative(lockout.started_at)) }
                </span>
                <span title={format_iso8601(lockout.ends_at)}>
                    { format!("ends {}", short_relative(lockout.ends_at)) }
                </span>
            </div>

            <StatusPill
                tone={StatusTone::Warning}
                label={format!("about {} failures", lockout.failures)}
                title={Some(AttrValue::from(
                    "Estimated failures inside the window that earned it: never fewer than \
                     there were, possibly more.",
                ))}
            />

            <ConfirmButton
                label="Clear"
                confirm_label="Clear it"
                question={question(lockout)}
                busy={*busy}
                onconfirm={clear}
            />

            if let Some(message) = &*error {
                <p class="lockout-row__error" role="alert">{ message.clone() }</p>
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    fn lockout(class: LockoutClass, key: &str) -> Lockout {
        let started_at = Utc::now();

        Lockout {
            class,
            address: "203.0.113.7".parse().ok(),
            prefix: Some(32),
            key: key.to_string(),
            started_at,
            ends_at: started_at + Duration::minutes(15),
            failures: 10,
        }
    }

    #[test]
    fn every_class_is_described_by_what_it_names() {
        assert_eq!(
            describe(&lockout(LockoutClass::Account, "ada")),
            "Account 'ada'"
        );
        assert_eq!(
            describe(&lockout(LockoutClass::Client, "cloudtak")),
            "OAuth client 'cloudtak'"
        );
        assert_eq!(
            describe(&lockout(LockoutClass::Address, "passkey")),
            "Every 'passkey' attempt"
        );
        assert_eq!(
            describe(&lockout(LockoutClass::Source, "203.0.113.7/32")),
            "Every sign-in"
        );
    }

    #[test]
    fn an_address_lockout_and_an_ipv6_prefix_say_how_wide_they_are() {
        let source = lockout(LockoutClass::Source, "203.0.113.7/32");
        assert!(
            question(&source).starts_with("Clear the lockout on every sign-in from 203.0.113.7?")
        );

        let network = Lockout {
            address: "2001:db8:1::".parse().ok(),
            prefix: Some(48),
            ..lockout(LockoutClass::Source, "2001:db8:1::/48")
        };
        assert_eq!(origin(&network), "from 2001:db8:1::/48");
    }

    #[test]
    fn the_fill_says_counts_are_estimates_and_what_a_full_tier_costs() {
        use rustak_api::LockoutTier;

        let tier = |tier, locked| TierFill {
            tier,
            sampled: 4096,
            locked,
            rows: 4,
        };

        assert_eq!(fill_summary(&[]), None);

        let summary =
            fill_summary(&[tier(LockoutTier::Source, 0), tier(LockoutTier::Pair, 1024)]).unwrap();

        assert!(summary.contains("estimates"), "{summary}");
        assert!(
            summary.contains("Addresses: 0.0% of cells locked"),
            "{summary}"
        );
        assert!(
            summary.contains("Accounts and endpoints: 25.0% of cells locked"),
            "{summary}"
        );
        assert!(
            summary.contains("about 3906 times in a million"),
            "{summary}"
        );
    }

    #[test]
    fn the_question_names_the_key_and_where_it_came_from() {
        let asked = question(&lockout(LockoutClass::Account, "ada"));

        assert!(asked.starts_with("Clear the lockout on account 'ada' from 203.0.113.7?"));

        let unknown = Lockout {
            address: None,
            ..lockout(LockoutClass::Address, "passkey")
        };
        assert!(question(&unknown).contains("an address the server could not read"));
    }

    #[test]
    fn the_counters_say_nothing_happened_rather_than_a_row_of_zeroes() {
        let zero = |class| LockoutCounter {
            class,
            refusals: 0,
            lockouts: 0,
        };
        let counters: Vec<_> = LockoutClass::ALL.into_iter().map(zero).collect();

        assert_eq!(
            counter_summary(&counters),
            "Nothing has been locked out or refused."
        );
    }

    #[test]
    fn the_counters_total_and_name_only_the_classes_that_moved() {
        let counters = vec![
            LockoutCounter {
                class: LockoutClass::Address,
                refusals: 0,
                lockouts: 0,
            },
            LockoutCounter {
                class: LockoutClass::Account,
                refusals: 7,
                lockouts: 2,
            },
            LockoutCounter {
                class: LockoutClass::Client,
                refusals: 1,
                lockouts: 1,
            },
        ];

        assert_eq!(
            counter_summary(&counters),
            "3 lockouts started and 8 attempts refused (lockouts / refusals: account 2 / 7, \
             oauth client 1 / 1).",
        );
    }
}
