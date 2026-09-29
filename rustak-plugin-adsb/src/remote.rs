//! What an administrator may change about this sidecar from the Services page.
//!
//! Every feed plugin offers the shared [`FeedConfig`] — the area and the
//! symbology. This one adds a setting of its own: the longest a refused
//! faster interval is left alone before it is probed again (M10-05's cap,
//! three hours unless somebody chose otherwise), because how many refusals a
//! day are worth paying to notice a relaxed limit is an operator's call.
//!
//! # Why minutes, and not `"3h"`
//!
//! The file writes it as a duration (`probe_max_wait = "3h"`). The Services
//! page draws its form from the schema here, and it draws a whole number with
//! bounds as a number input that shows the range and marks a value outside
//! it before anybody saves; a string would be a bare text box whose range
//! nobody sees until the save is refused. So the document carries a whole
//! number of minutes, and the key says so.

use std::time::Duration;

use rustak_api::{ConfigIssue, ConfigValidation};
use rustak_client::feed::FeedConfig;
use rustak_client::sidecar::parse_config;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::sources::probe_max_wait_out_of_range;

/// Where `probe_max_wait_minutes` sits in the document, for an issue about it.
const POINTER: &str = "/probe_max_wait_minutes";

/// The ADS-B sidecar's server-side configuration.
///
/// Unknown keys are tolerated, as [`FeedConfig`] tolerates them.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct AdsbConfig {
    /// The area and the symbology, as every feed plugin reads them.
    #[serde(flatten)]
    pub feed: FeedConfig,

    /// How long, at most, this sidecar waits before trying again a faster poll
    /// interval the provider refused. Each refused try doubles the wait up to
    /// this. Longer means fewer refusals; shorter means a provider that relaxed
    /// its limit is followed down sooner. Leave unset to use the configuration
    /// file's `probe_max_wait` (three hours unless it says otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        title = "Longest wait before retrying a refused poll interval (minutes)",
        range(min = 1, max = 1440),
        extend("default" = 180)
    )]
    pub probe_max_wait_minutes: Option<u32>,
}

impl AdsbConfig {
    /// `probe_max_wait_minutes` as a span, when the document sets it.
    #[must_use]
    pub fn probe_max_wait(&self) -> Option<Duration> {
        self.probe_max_wait_minutes
            .map(|minutes| Duration::from_secs(u64::from(minutes) * 60))
    }

    /// This sidecar's whole `validate_config`.
    ///
    /// `configured` is the interval the open source was configured with, which
    /// the shortest wait is measured in. Before a source is open it is not
    /// known, and only the bounds that hold for every source are checked: one
    /// clean run at one second, and a day.
    #[must_use]
    pub fn check(config: &serde_json::Value, configured: Option<Duration>) -> ConfigValidation {
        let mut verdict = FeedConfig::check(config);

        let read = match parse_config::<Self>(config) {
            Ok(read) => read,
            Err(refusal) => return refusal,
        };

        let fastest = configured.unwrap_or(Duration::from_secs(1));
        if let Some(why) = read
            .probe_max_wait()
            .and_then(|wait| probe_max_wait_out_of_range("probe_max_wait_minutes", wait, fastest))
        {
            verdict.issues.push(ConfigIssue::at(POINTER, why));
        }

        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_client::sidecar::schema_for;
    use serde_json::json;

    fn issues(config: serde_json::Value, configured: Option<u64>) -> Vec<ConfigIssue> {
        AdsbConfig::check(&config, configured.map(Duration::from_secs)).issues
    }

    #[test]
    fn the_schema_offers_the_wait_as_bounded_minutes_beside_the_feed_settings() {
        let schema = schema_for::<AdsbConfig>();
        let wait = &schema["properties"]["probe_max_wait_minutes"];

        assert_eq!(wait["minimum"], 1, "{schema}");
        assert_eq!(wait["maximum"], 1440, "{schema}");
        assert_eq!(wait["default"], 180, "{schema}");
        assert!(
            wait["title"]
                .as_str()
                .is_some_and(|t| t.contains("minutes"))
        );
        assert!(
            wait["description"]
                .as_str()
                .is_some_and(|d| d.contains("refused"))
        );
        assert!(
            schema["properties"]["area"].is_object()
                && schema["properties"]["symbology"].is_object(),
            "the shared settings are still offered: {schema}",
        );
        assert!(
            !schema["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|key| key == "probe_max_wait_minutes")),
            "optional: an empty field gives the choice back to the file",
        );
    }

    #[test]
    fn a_document_reads_back_with_or_without_the_wait() {
        let read: AdsbConfig =
            serde_json::from_value(json!({ "probe_max_wait_minutes": 360 })).expect("it reads");

        assert_eq!(read.probe_max_wait(), Some(Duration::from_secs(6 * 3600)));
        assert_eq!(AdsbConfig::default().probe_max_wait(), None);
        assert!(issues(json!({ "probe_max_wait_minutes": 360 }), Some(10)).is_empty());
        assert!(issues(json!({}), Some(10)).is_empty());
    }

    #[test]
    fn a_wait_longer_than_a_day_is_refused_by_name() {
        let found = issues(json!({ "probe_max_wait_minutes": 1441 }), Some(10));

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path.as_deref(), Some(POINTER));
        assert!(
            found[0].message.contains("`probe_max_wait_minutes`"),
            "{found:?}"
        );
        assert!(found[0].message.contains("1440 minutes"), "{found:?}");
    }

    #[test]
    fn a_wait_shorter_than_one_clean_run_at_the_configured_interval_is_refused() {
        // 60 polls at 10s is ten minutes: nine is no wait at all.
        let found = issues(json!({ "probe_max_wait_minutes": 9 }), Some(10));

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path.as_deref(), Some(POINTER));
        assert!(found[0].message.contains("10 minutes"), "{found:?}");
        assert!(issues(json!({ "probe_max_wait_minutes": 10 }), Some(10)).is_empty());

        // Before a source is open, only what holds for every source.
        assert!(issues(json!({ "probe_max_wait_minutes": 9 }), None).is_empty());
    }

    #[test]
    fn the_feed_settings_are_still_checked() {
        let found = issues(
            json!({ "area": { "kind": "circle", "lat": 0.0, "lon": 0.0, "radius_km": 0.0 } }),
            Some(10),
        );

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path.as_deref(), Some("/area/radius_km"));
        assert!(!issues(json!({ "probe_max_wait_minutes": "3h" }), Some(10)).is_empty());
    }
}
