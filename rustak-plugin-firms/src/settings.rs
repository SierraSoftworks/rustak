//! The `[settings]` table: what this sidecar watches, and where it reads it.
//!
//! Every key is documented with its default in `config.example.toml`, which
//! this crate's test suite loads so that the two cannot drift apart.

use std::path::PathBuf;

use rustak_client::feed::Area;
use rustak_core::config::duration;
use rustak_core::prelude::*;

use crate::hotspots::{Filter, Publish};
use crate::mapping::Display;
use crate::sources::{FirmsFeed, HotspotFeed, ReplayFeed, Sensor};

/// How many days each request covers when a configuration does not say.
///
/// Two, not one: FIRMS' days are UTC calendar days, so "1" just after midnight
/// is almost nothing, and a sidecar restarted then would lose a day's fires
/// that `max_age` says should still be on the map.
pub const DEFAULT_DAYS: u8 = 2;

/// Where this plugin reads detections from.
///
/// `#[serde(tag = "kind")]`, so a settings file names the upstream it means.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// A FIRMS CSV file, replayed. What the demonstration and the tests use.
    Replay {
        /// The file; see [`ReplayFeed`] for the format.
        path: PathBuf,
    },

    /// NASA FIRMS' area API.
    Firms {
        /// The MAP_KEY. Write it as `"${{ env.FIRMS_MAP_KEY }}"`.
        #[serde(deserialize_with = "secret")]
        map_key: Secret,

        /// Which instruments to read. Default: the three VIIRS.
        #[serde(default = "default_sensors")]
        sensors: Vec<Sensor>,

        /// How many days each request covers, 1 to 5. Default: 2.
        #[serde(default = "default_days")]
        days: u8,

        /// How often to ask. Default: `"10m"`, and never faster than `"1m"`.
        #[serde(default, with = "duration::humane_option")]
        poll: Option<chrono::Duration>,

        /// A mirror or proxy to ask instead of FIRMS itself. Default: none.
        #[serde(default)]
        base_url: Option<String>,
    },
}

impl Source {
    /// Opens the upstream this setting names, over the area being watched.
    ///
    /// # Errors
    ///
    /// Whatever the source could not do, as something the operator can fix.
    pub fn open(&self, area: Area) -> Result<Box<dyn HotspotFeed>, Error> {
        match self {
            Self::Replay { path } => Ok(Box::new(ReplayFeed::open(path)?)),
            Self::Firms {
                map_key,
                sensors,
                days,
                poll,
                base_url,
            } => Ok(Box::new(FirmsFeed::open(
                map_key.clone(),
                sensors,
                *days,
                poll.and_then(|poll| poll.to_std().ok()),
                base_url.as_deref(),
                area,
            )?)),
        }
    }

    /// What to call this kind of source in a heartbeat.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Replay { .. } => "replay",
            Self::Firms { .. } => "firms",
        }
    }
}

/// `[settings]` — what this sidecar watches, and how it draws it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Where the feed is looking. Default: the whole world, which on a bad day
    /// is tens of thousands of detections; set one.
    #[serde(default)]
    pub area: Area,

    /// Which detections are worth drawing. Default: all of them.
    #[serde(default)]
    pub filter: Filter,

    /// Markers, footprints or both, and in what colours.
    #[serde(default)]
    pub display: Display,

    /// How long a detection lives, and how often it is said again.
    #[serde(default)]
    pub publish: Publish,

    /// The upstream. Required.
    pub source: Source,
}

impl Default for Settings {
    /// Only reached by a configuration file with no `[settings]` table: it
    /// replays `hotspots.csv` from the working directory, and says so by name
    /// when that file is not there.
    fn default() -> Self {
        Self {
            area: Area::default(),
            filter: Filter::default(),
            display: Display::default(),
            publish: Publish::default(),
            source: Source::Replay {
                path: PathBuf::from("hotspots.csv"),
            },
        }
    }
}

fn default_sensors() -> Vec<Sensor> {
    vec![Sensor::ViirsNoaa20, Sensor::ViirsNoaa21, Sensor::ViirsSnpp]
}

const fn default_days() -> u8 {
    DEFAULT_DAYS
}

/// Reads a string into a [`Secret`], which redacts itself when logged.
fn secret<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Secret, D::Error> {
    Ok(Secret::new(String::deserialize(deserializer)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{ColourBy, Shape};
    use crate::wire::Confidence;
    use rustak_client::sidecar::SidecarConfig;

    fn load(settings: &str) -> Result<Settings, Error> {
        rustak_core::config::load_str::<SidecarConfig<Settings>>(&format!(
            "[service]\nname = \"firms\"\n\n{settings}",
        ))
        .map(|config| config.settings)
    }

    #[test]
    fn a_firms_source_needs_only_its_key() {
        let settings =
            load("[settings.source]\nkind = \"firms\"\nmap_key = \"abc123\"\n").expect("it loads");

        match &settings.source {
            Source::Firms {
                sensors,
                days,
                poll,
                base_url,
                ..
            } => {
                assert_eq!(sensors, &default_sensors());
                assert_eq!(*days, DEFAULT_DAYS);
                assert!(poll.is_none() && base_url.is_none());
            }
            other => panic!("expected FIRMS, got {other:?}"),
        }

        assert_eq!(settings.source.kind(), "firms");
        assert_eq!(settings.display, Display::default());
        assert_eq!(settings.publish, Publish::default());
    }

    #[test]
    fn every_table_reads_what_an_operator_would_write() {
        let settings = load(
            r#"
            [settings.filter]
            min_confidence = "nominal"
            min_frp_mw = 5.0

            [settings.display]
            shape = "both"
            colour_by = "intensity"

            [settings.publish]
            max_age = "12h"
            max_per_tick = 100

            [settings.source]
            kind = "firms"
            map_key = "abc123"
            sensors = ["modis", "landsat"]
            days = 1
            poll = "5m"
            "#,
        )
        .expect("it loads");

        assert_eq!(settings.filter.min_confidence, Confidence::Nominal);
        assert_eq!(settings.display.shape, Shape::Both);
        assert_eq!(settings.display.colour_by, ColourBy::Intensity);
        assert_eq!(settings.publish.max_age, chrono::Duration::hours(12));
        assert_eq!(
            settings.publish.republish,
            Publish::default().republish,
            "a key the table omits keeps its default",
        );
    }

    #[test]
    fn the_map_key_never_prints_itself() {
        let settings =
            load("[settings.source]\nkind = \"firms\"\nmap_key = \"hunter2hunter2\"\n").unwrap();

        assert!(!format!("{settings:?}").contains("hunter2"));
    }

    #[test]
    fn what_is_misspelled_or_missing_is_refused_by_name() {
        for (written, named) in [
            ("[settings.source]\nkind = \"firms\"\n", "map_key"),
            (
                "[settings.source]\nkind = \"firms\"\nmap_key = \"a\"\nsensor = []\n",
                "sensor",
            ),
            (
                "[settings.source]\nkind = \"firms\"\nmap_key = \"a\"\nsensors = [\"goes\"]\n",
                "goes",
            ),
            ("[settings.display]\nshape = \"tiles\"\n", "tiles"),
            ("[settings.filter]\nmin_frp_mw = 1.0\n", "source"),
        ] {
            let err = load(written).expect_err(written);

            assert!(err.to_string().contains(named), "{named}: {err}");
        }
    }
}
