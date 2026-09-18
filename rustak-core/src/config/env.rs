//! The one interpolation expression rustak understands: `env.NAME`.
//!
//! [`interpolate`](super::interpolation::interpolate) knows how to *find* a
//! `${{ … }}` expression; this module decides what one may *say*. Exactly one
//! form is meaningful — `env.NAME`, naming a process environment variable —
//! and anything else is a typo worth reporting rather than a feature to guess
//! at.
//!
//! # An unset variable is left verbatim, not an error
//!
//! When `NAME` is not set, [`resolve`] returns the original `${{ env.NAME }}`
//! text instead of an empty string or an error. That is deliberate, and it is
//! the behaviour automate has: config loading happens before the code that
//! knows which keys are *required*, so the loader cannot tell "the operator
//! forgot to export `RUSTAK_OIDC_CLIENT_SECRET`" from "this optional section
//! is not in use". Leaving the marker in place hands that judgement to the
//! type that consumes the value, which can say what the key is *for* —
//! [`is_unresolved`] is how such a type recognises one.
//!
//! An empty string would be far worse than either: a blank client secret or a
//! blank signing key is a *credential* that silently downgrades to nothing.

/// The opening marker of an interpolation expression, and so the tell-tale of
/// a value that was never resolved. See [`is_unresolved`].
pub const UNRESOLVED_MARKER: &str = "${{";

/// Advice shown when an expression is not one we know how to evaluate.
const ADVICE_UNKNOWN_EXPRESSION: &[&str] = &[
    "Only `env.VARIABLE_NAME` expressions are supported in rustak configuration files.",
    "Escape literal text that looks like an expression with `\\${{ ... }}`.",
];

/// Evaluates a single interpolation expression against the process environment.
///
/// Surrounding whitespace is insignificant, so `${{ env.NAME }}` and
/// `${{env.NAME}}` mean the same thing.
///
/// # Errors
///
/// Returns a [`human_errors::Kind::User`] error when the expression is not of
/// the form `env.NAME`. An *unset* variable is not an error — see the
/// [module documentation](self).
///
/// # Example
///
/// ```
/// # use rustak_core::config::env::resolve;
/// // An unset variable keeps its marker so that whoever needs the value can
/// // explain what it was for.
/// assert_eq!(
///     resolve(" env.RUSTAK_DOC_DEFINITELY_NOT_SET ").unwrap(),
///     "${{ env.RUSTAK_DOC_DEFINITELY_NOT_SET }}",
/// );
///
/// // Anything that is not `env.NAME` is a mistake we can name.
/// assert!(resolve("secrets.API_KEY").is_err());
/// ```
pub fn resolve(expression: &str) -> Result<String, human_errors::Error> {
    let expression = expression.trim();

    let Some(name) = expression.strip_prefix("env.") else {
        return Err(human_errors::user(
            format!("We do not know how to evaluate the expression '{expression}'."),
            ADVICE_UNKNOWN_EXPRESSION,
        ));
    };

    Ok(std::env::var(name).unwrap_or_else(|_| format!("${{{{ {expression} }}}}")))
}

/// Reports whether a loaded value still contains an unevaluated expression.
///
/// A value that does is one whose environment variable was not set. Types that
/// hold credentials use this to refuse the value with a message naming the key,
/// rather than treating the literal `${{ env.… }}` text as a secret.
///
/// ```
/// # use rustak_core::config::env::is_unresolved;
/// assert!(is_unresolved("${{ env.RUSTAK_SECRET_KEY }}"));
/// assert!(!is_unresolved("a-real-value"));
/// ```
pub fn is_unresolved(value: &str) -> bool {
    value.contains(UNRESOLVED_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A variable name nothing sets, on any platform.
    const UNSET: &str = "RUSTAK_CONFIG_ENV_TEST_MISSING";

    #[test]
    fn a_set_variable_is_substituted() {
        // `PATH` rather than a variable of our own: `std::env::set_var` is
        // `unsafe` in this edition and the crate forbids `unsafe`, and a test
        // that mutated the process environment would be unsound alongside the
        // other tests in this binary anyway. `PATH` is set for every process on
        // every platform rustak runs on, so it demonstrates substitution
        // without anybody having to arrange it.
        let expected = std::env::var("PATH").expect("PATH is set for every process");

        assert_eq!(resolve("env.PATH").unwrap(), expected);
        assert!(!is_unresolved(&resolve("env.PATH").unwrap()));
    }

    #[test]
    fn an_unset_variable_keeps_its_marker_rather_than_becoming_empty() {
        // The distinction this protects: an empty string is a *credential* that
        // silently became nothing, whereas the marker is a value that whoever
        // consumes it can refuse by name.
        let resolved = resolve(&format!("env.{UNSET}")).unwrap();

        assert_eq!(resolved, format!("${{{{ env.{UNSET} }}}}"));
        assert!(is_unresolved(&resolved));
    }

    #[test]
    fn an_expression_we_do_not_understand_is_refused_and_says_so() {
        let Err(err) = resolve("secrets.API_KEY") else {
            panic!("an unknown expression namespace should not resolve");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains("secrets.API_KEY"), "{err}");
    }

    #[test]
    fn whitespace_around_an_expression_is_insignificant() {
        // `${{env.X}}` and `${{ env.X }}` are both things people write, and the
        // parser hands us whatever was between the braces.
        let expected = std::env::var("PATH").expect("PATH is set for every process");

        for expression in ["env.PATH", " env.PATH ", "\tenv.PATH\n"] {
            assert_eq!(resolve(expression).unwrap(), expected, "{expression:?}");
        }
    }
}
