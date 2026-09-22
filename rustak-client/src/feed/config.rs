//! What an administrator may change about a feed sidecar from the admin UI.
//!
//! A feed plugin's `[settings]` table is baked into a container image; the
//! configuration the server holds is where an operator moves the box without a
//! redeploy. This is the type that document is read into, and — because the
//! admin UI draws its form from the schema derived here — the one place that
//! says which keys exist.
//!
//! It is deliberately small. Everything else about a feed (its upstream, its
//! credentials, its publishing policy) stays in the file, because changing it
//! means reopening a source with material the server has no business holding.

use rustak_api::{ConfigIssue, ConfigValidation};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Area, Symbology};
use crate::sidecar::parse_config;

/// A feed sidecar's server-side configuration.
///
/// Unknown keys are tolerated rather than refused: a document written before
/// this type existed may carry some, and they were always ignored.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct FeedConfig {
    /// Where the feed is looking. Leave unset to use the area in the sidecar's
    /// own configuration file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<Area>,

    /// Which MIL-STD-2525 symbol code each track carries beside its CoT type.
    /// Leave unset to use the one in the sidecar's own configuration file.
    ///
    /// A device draws a bare type from its 2525C tables whatever edition it is
    /// set to, so a fleet on 2525D chooses that here to see these tracks in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbology: Option<Symbology>,
}

impl FeedConfig {
    /// A feed plugin's whole `Sidecar::validate_config`: can this build read the
    /// document, and does the area in it describe somewhere.
    #[must_use]
    pub fn check(config: &serde_json::Value) -> ConfigValidation {
        match parse_config::<Self>(config) {
            Ok(read) => ConfigValidation::rejected(read.problems()),
            Err(refusal) => refusal,
        }
    }

    /// What the schema's per-field ranges cannot say, because it is a relation
    /// between fields or a bound JSON Schema has no attribute for here.
    fn problems(&self) -> Vec<ConfigIssue> {
        match self.area {
            Some(Area::Bbox { south, north, .. }) if south > north => vec![ConfigIssue::at(
                "/area/south",
                "The southern edge is north of the northern edge. (West may be greater than \
                 east: that is a box across the anti-meridian.)",
            )],
            Some(Area::Circle { radius_km, .. }) if radius_km <= 0.0 => vec![ConfigIssue::at(
                "/area/radius_km",
                "A circle needs a radius greater than zero.",
            )],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar::schema_for;

    #[test]
    fn the_schema_offers_both_kinds_of_area_by_their_tag() {
        let schema = schema_for::<FeedConfig>().to_string();

        for expected in ["\"bbox\"", "\"circle\"", "radius_km", "\"maximum\":90"] {
            assert!(schema.contains(expected), "{expected}: {schema}");
        }
    }

    #[test]
    fn the_schema_offers_every_edition_and_the_document_reads_back() {
        let schema = schema_for::<FeedConfig>().to_string();

        for expected in ["\"none\"", "\"2525c\"", "\"2525d\"", "MIL-STD-2525D"] {
            assert!(schema.contains(expected), "{expected}: {schema}");
        }

        let read: FeedConfig = serde_json::from_value(serde_json::json!({ "symbology": "2525d" }))
            .expect("a document");
        assert_eq!(read.symbology, Some(Symbology::Milstd2525D));
        assert!(FeedConfig::check(&serde_json::json!({ "symbology": "2525d" })).is_valid());
        assert!(!FeedConfig::check(&serde_json::json!({ "symbology": "2525e" })).is_valid());
    }

    #[test]
    fn an_area_that_describes_nowhere_is_refused_at_the_field_that_is_wrong() {
        for (config, path) in [
            (serde_json::json!({}), None),
            (
                serde_json::json!({ "area": { "kind": "circle", "lat": 51.5, "lon": -0.5, "radius_km": 120.0 } }),
                None,
            ),
            // Fiji: west of east is a box across the anti-meridian, not a mistake.
            (
                serde_json::json!({ "area": { "kind": "bbox", "south": -19.0, "west": 176.0, "north": -16.0, "east": -178.0 } }),
                None,
            ),
            (
                serde_json::json!({ "area": { "kind": "bbox", "south": 52.0, "west": 0.0, "north": 50.0, "east": 1.0 } }),
                Some("/area/south"),
            ),
            (
                serde_json::json!({ "area": { "kind": "circle", "lat": 0.0, "lon": 0.0, "radius_km": 0.0 } }),
                Some("/area/radius_km"),
            ),
        ] {
            let verdict = FeedConfig::check(&config);

            assert_eq!(
                verdict
                    .issues
                    .first()
                    .and_then(|issue| issue.path.as_deref()),
                path,
                "{config}",
            );
        }
    }

    #[test]
    fn a_document_this_build_cannot_read_is_refused_rather_than_ignored() {
        let verdict = FeedConfig::check(&serde_json::json!({ "area": "the whole world" }));

        assert!(!verdict.is_valid());
    }
}
