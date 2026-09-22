//! The configuration as text: what a service that registered no schema is
//! edited as, and what any service *can* be edited as.

/// What is wrong with the text somebody typed, if anything.
///
/// The same two rules the server applies, applied before the request rather
/// than after it: it has to be JSON, and it has to be an object.
pub fn config_problem(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return Some(
            "A configuration is a JSON object. Write {} for a service with nothing to set."
                .to_string(),
        );
    }

    match serde_json::from_str::<serde_json::Value>(text) {
        Err(err) => Some(format!("That is not JSON we can read: {err}.")),
        Ok(value) if !value.is_object() => Some(
            "A service's configuration has to be a JSON object — a set of keys, wrapped in { }."
                .to_string(),
        ),
        Ok(_) => None,
    }
}

/// The stored configuration as the editor shows it.
pub fn as_text(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_json_object_is_a_configuration() {
        assert_eq!(config_problem(r#"{ "interval_seconds": 30 }"#), None);
        assert_eq!(config_problem("{}"), None);

        assert!(
            config_problem("   ")
                .expect("a blank box is not an object")
                .contains("{}")
        );
        assert!(
            config_problem("{ oops }")
                .expect("that is not JSON")
                .starts_with("That is not JSON we can read")
        );
        // Valid JSON, but not something the server would store.
        assert!(
            config_problem("[1, 2, 3]")
                .expect("an array is not an object")
                .contains("JSON object")
        );
        assert!(config_problem("42").is_some());
    }

    #[test]
    fn the_editor_shows_the_stored_document_rather_than_one_long_line() {
        let text = as_text(&serde_json::json!({ "interval_seconds": 30 }));

        assert!(text.contains('\n'), "{text}");
        assert_eq!(config_problem(&text), None);
    }
}
