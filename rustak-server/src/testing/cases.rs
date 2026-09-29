//! Running several named cases against one expensive fixture.
//!
//! An integration test that boots a server per assertion pays for the server
//! every time, and on a two-vCPU runner under `-Cinstrument-coverage` that is
//! most of what the suite costs. The alternative — one server and a list of
//! cases — has a well-known failure mode: the first case to panic takes the
//! rest with it, and the report names the test rather than the case.
//!
//! [`run`] is the other half. Every case runs to completion, concurrently, on
//! the caller's runtime; a panic in one is caught and does not stop the others;
//! and the test fails once, at the end, naming every case that failed with the
//! message it failed with. What a case may share with its neighbours is the
//! caller's business — distinct accounts, distinct addresses — and the caller
//! should say so where the cases are written.

use std::panic::AssertUnwindSafe;

use futures::FutureExt as _;
use futures::future::LocalBoxFuture;

/// One case: the name it is reported under, and the work.
pub type Case<'a> = (&'static str, LocalBoxFuture<'a, ()>);

/// Runs every case and fails, once, naming each one that panicked.
///
/// # Panics
///
/// When any case panicked, with every failing case's name and message, or when
/// two cases share a name, which would make the report ambiguous.
pub async fn run(cases: Vec<Case<'_>>) {
    let total = cases.len();
    let mut names: Vec<&str> = cases.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), total, "every case needs a name of its own");

    let outcomes =
        futures::future::join_all(cases.into_iter().map(|(name, case)| async move {
            (name, AssertUnwindSafe(case).catch_unwind().await)
        }))
        .await;

    let failures: Vec<String> = outcomes
        .into_iter()
        .filter_map(|(name, outcome)| {
            outcome
                .err()
                .map(|panic| format!("  {name}: {}", message(panic.as_ref())))
        })
        .collect();

    assert!(
        failures.is_empty(),
        "{} of {total} cases failed:\n{}",
        failures.len(),
        failures.join("\n"),
    );
}

/// The text a panic carried, when it carried any.
fn message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("(a panic with no message)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_case_runs_even_after_one_fails_and_each_failure_is_named() {
        let ran = std::cell::Cell::new(0);
        let cases: Vec<Case<'_>> = vec![
            (
                "first fails",
                Box::pin(async { panic!("the first reason") }),
            ),
            (
                "second passes",
                Box::pin(async {
                    ran.set(ran.get() + 1);
                }),
            ),
            (
                "third fails",
                Box::pin(async { panic!("the third reason") }),
            ),
        ];

        let outcome = AssertUnwindSafe(run(cases)).catch_unwind().await;
        let report = message(outcome.expect_err("two cases failed").as_ref()).to_string();

        assert_eq!(ran.get(), 1, "the case after a failure still ran");
        assert!(report.starts_with("2 of 3 cases failed"), "{report}");
        assert!(report.contains("first fails: the first reason"), "{report}");
        assert!(report.contains("third fails: the third reason"), "{report}");
        assert!(!report.contains("second passes"), "{report}");
    }

    #[tokio::test]
    async fn cases_that_all_pass_pass() {
        run(vec![("only", Box::pin(async {}))]).await;
    }

    #[tokio::test]
    #[should_panic(expected = "every case needs a name of its own")]
    async fn two_cases_with_one_name_are_refused() {
        run(vec![
            ("twice", Box::pin(async {})),
            ("twice", Box::pin(async {})),
        ])
        .await;
    }
}
