//! The advice rustak repeats, and the one place a binary gives up.
//!
//! `human-errors` splits every failure into a [`Kind::User`] one — something an
//! operator can fix, shown to them verbatim — and a [`Kind::System`] one, which
//! is our bug and is reported to telemetry. The advice attached to an error is
//! what turns it from a diagnosis into an instruction, and a handful of those
//! instructions recur across every crate. Naming them here keeps the wording
//! identical wherever they appear, which matters because an operator who has
//! read one of these messages once should recognise it the next time.
//!
//! [`Kind::User`]: human_errors::Kind::User
//! [`Kind::System`]: human_errors::Kind::System

use std::sync::Arc;

use tracing_batteries::Session;

/// Advice for a failure that is ours rather than the operator's.
///
/// Attach this to a [`Kind::System`](human_errors::Kind::System) error: there is
/// nothing to configure, so the only useful next step is to tell us.
pub const ADVICE_REPORT_DEV: &[&str] = &[
    "This is not something you can fix from your configuration; it is a bug in rustak.",
    "Please report it at https://github.com/SierraSoftworks/rustak/issues, including the log output above.",
];

/// Advice for anything we could not read from or write to the filesystem.
pub const ADVICE_FILE_ACCESS: &[&str] = &[
    "Check that the path exists and that the rustak process may read and write it.",
    "On a container deployment, check that the data directory is mounted and is not read-only.",
];

/// Advice for a failure that a restart will not fix on its own.
pub const ADVICE_RESTART_AFTER_FIXING: &[&str] =
    &["Restart rustak once you have addressed the problems reported above."];

/// Prints a fatal error the way the operator should see it and ends the process.
///
/// This is the single place a rustak binary gives up, and it does three things
/// in a deliberate order:
///
/// 1. writes the pretty form of the error to standard error, because an
///    operator watching `docker logs` needs the message and the advice, not a
///    `Debug` dump;
/// 2. records the error to telemetry **when it is ours** — a
///    [`Kind::System`](human_errors::Kind::System) failure is a bug we want to
///    hear about, whereas a misspelled config key is not something to page
///    anybody over;
/// 3. flushes the telemetry session before exiting, so the report survives the
///    process it describes.
///
/// `session` is optional because the earliest failures — a `.env` that will not
/// parse, arguments that do not make sense — happen before telemetry exists. An
/// error at that point is by definition a user error, and there is nothing to
/// flush.
///
/// Exits with status 1.
pub async fn report_and_exit(error: &human_errors::Error, session: Option<Arc<Session>>) -> ! {
    eprintln!("{}", human_errors::pretty(error));

    if let Some(session) = session {
        if error.is(human_errors::Kind::System) {
            session.record_human_error(error);
        }

        crate::telemetry::shutdown(session).await;
    }

    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_advice_slice_gives_the_reader_something_to_do() {
        // Advice that is empty, or that only restates the problem, turns a
        // message an operator can act on back into one they cannot. These are
        // shown verbatim, so the bar is that each line is an instruction.
        for advice in [
            ADVICE_REPORT_DEV,
            ADVICE_FILE_ACCESS,
            ADVICE_RESTART_AFTER_FIXING,
        ] {
            assert!(!advice.is_empty(), "an advice slice must not be empty");

            for line in advice {
                assert!(
                    line.len() > 20,
                    "advice should be an instruction, got {line:?}",
                );
                assert!(
                    line.ends_with('.'),
                    "advice should read as a sentence, got {line:?}",
                );
            }
        }
    }

    #[test]
    fn the_report_advice_says_where_to_report() {
        // The whole value of this slice is the URL; a version of it that said
        // "please report this" without saying where would be worse than nothing.
        assert!(
            ADVICE_REPORT_DEV
                .iter()
                .any(|line| line.contains("github.com/SierraSoftworks/rustak")),
            "the reporting advice must name somewhere to report to",
        );
    }

    #[test]
    fn a_system_error_is_distinguishable_from_a_user_error() {
        // `report_and_exit` branches on this, and it is the difference between
        // paging somebody and showing somebody a typo.
        let ours = human_errors::system("We could not open the database.", ADVICE_REPORT_DEV);
        let theirs = human_errors::user("We could not read your config file.", ADVICE_FILE_ACCESS);

        assert!(ours.is(human_errors::Kind::System));
        assert!(theirs.is(human_errors::Kind::User));
        assert!(!theirs.is(human_errors::Kind::System));
    }
}
