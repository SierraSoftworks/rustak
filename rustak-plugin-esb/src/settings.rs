//! The `[settings]` table. Every key is documented with its default in
//! `config.example.toml`, which this crate's tests load so the two cannot drift.

use std::path::PathBuf;
use std::time::Duration;

use rustak_client::feed::Area;
use rustak_core::config::duration;
use rustak_core::prelude::*;

use crate::outage::OutageKind;
use crate::scope::Scope;
use crate::sources::powercheck::{DEFAULT_BASE_URL, DEFAULT_DETAILS_PER_TICK, DEFAULT_POLL};
use crate::sources::{OutageFeed, PowerCheckFeed, ReplayFeed};

/// How long a marker lives on a map without being republished.
pub const STALE: Duration = Duration::from_secs(900);

/// How often an unchanged marker is republished.
pub const REFRESH: Duration = Duration::from_secs(300);

/// Where the outages come from.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// A file of outages, for demonstrations and the test suite.
    Replay {
        /// Newline-delimited JSON; see `outages.example.ndjson`.
        path: PathBuf,
    },

    /// ESB Networks' PowerCheck API.
    #[serde(rename = "powercheck")]
    PowerCheck {
        /// The `API-Subscription-Key` the PowerCheck site sends. Required.
        #[serde(deserialize_with = "secret")]
        api_key: Secret,

        /// How often the list is asked for. Default `"5m"`, floor `"1m"`.
        #[serde(default = "default_poll", with = "duration::humane")]
        poll: chrono::Duration,

        /// How many outages have their details fetched per tick. Default 10;
        /// 0 publishes markers with a type and a position and nothing else.
        #[serde(default = "default_details_per_tick")]
        details_per_tick: usize,

        /// Where the API lives. Default: ESB's own.
        #[serde(default = "default_base_url")]
        base_url: String,
    },
}

impl Source {
    /// Opens the upstream this setting names.
    ///
    /// # Errors
    ///
    /// Whatever the source could not do, as something the operator can fix.
    pub fn open(&self, scope: Scope) -> Result<Box<dyn OutageFeed>, Error> {
        match self {
            Self::Replay { path } => Ok(Box::new(ReplayFeed::open(path)?)),
            Self::PowerCheck {
                api_key,
                poll,
                details_per_tick,
                base_url,
            } => Ok(Box::new(PowerCheckFeed::open(
                base_url,
                api_key,
                scope,
                poll.to_std().unwrap_or(DEFAULT_POLL),
                *details_per_tick,
            )?)),
        }
    }

    /// What to call this kind of source in a heartbeat.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Replay { .. } => "replay",
            Self::PowerCheck { .. } => "powercheck",
        }
    }
}

/// `[settings]` — what this sidecar watches, and how long its markers live.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Where to look. Default: everywhere, which for ESB is Ireland.
    #[serde(default)]
    pub area: Area,

    /// Which kinds of outage get a marker. Default: all of them.
    #[serde(default = "default_include")]
    pub include: Vec<OutageKind>,

    /// How long a marker lives without being republished. Default `"15m"`.
    #[serde(default = "default_stale", with = "duration::humane")]
    pub stale: chrono::Duration,

    /// How often an unchanged marker is republished. Default `"5m"`.
    #[serde(default = "default_refresh", with = "duration::humane")]
    pub refresh: chrono::Duration,

    /// The upstream. Required.
    pub source: Source,
}

impl Settings {
    /// `stale`, falling back rather than failing on a negative typo.
    #[must_use]
    pub fn stale(&self) -> Duration {
        self.stale.to_std().unwrap_or(STALE)
    }

    /// `refresh`, likewise.
    #[must_use]
    pub fn refresh(&self) -> Duration {
        self.refresh.to_std().unwrap_or(REFRESH)
    }
}

impl Default for Settings {
    /// Only reached by a file with no `[settings]` table: it replays
    /// `outages.ndjson`, and says so by name when that file is not there.
    fn default() -> Self {
        Self {
            area: Area::default(),
            include: default_include(),
            stale: default_stale(),
            refresh: default_refresh(),
            source: Source::Replay {
                path: PathBuf::from("outages.ndjson"),
            },
        }
    }
}

fn default_include() -> Vec<OutageKind> {
    OutageKind::ALL.to_vec()
}

fn chrono_of(value: Duration) -> chrono::Duration {
    chrono::Duration::from_std(value).unwrap_or_else(|_| chrono::Duration::zero())
}

fn default_stale() -> chrono::Duration {
    chrono_of(STALE)
}

fn default_refresh() -> chrono::Duration {
    chrono_of(REFRESH)
}

fn default_poll() -> chrono::Duration {
    chrono_of(DEFAULT_POLL)
}

const fn default_details_per_tick() -> usize {
    DEFAULT_DETAILS_PER_TICK
}

fn default_base_url() -> String {
    DEFAULT_BASE_URL.to_string()
}

/// Reads a string into a [`Secret`], which redacts itself when logged.
fn secret<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Secret, D::Error> {
    Ok(Secret::new(String::deserialize(deserializer)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_client::sidecar::SidecarConfig;

    fn load(settings: &str) -> Result<SidecarConfig<Settings>, Error> {
        rustak_core::config::load_str(&format!("[service]\nname = \"esb\"\n\n{settings}"))
    }

    #[test]
    fn a_powercheck_source_needs_only_a_key() {
        let settings = load("[settings.source]\nkind = \"powercheck\"\napi_key = \"hunter2\"\n")
            .expect("it loads")
            .settings;

        assert_eq!(settings.source.kind(), "powercheck");
        assert_eq!(settings.include, OutageKind::ALL.to_vec());
        assert_eq!(settings.stale(), STALE);
        assert_eq!(settings.refresh(), REFRESH);

        let Source::PowerCheck {
            poll,
            details_per_tick,
            base_url,
            ..
        } = &settings.source
        else {
            panic!("expected PowerCheck, got {:?}", settings.source);
        };
        assert_eq!(poll.to_std().ok(), Some(DEFAULT_POLL));
        assert_eq!(*details_per_tick, DEFAULT_DETAILS_PER_TICK);
        assert_eq!(base_url, DEFAULT_BASE_URL);

        assert!(
            settings.source.open(Scope::default()).is_ok(),
            "without a request"
        );
    }

    #[test]
    fn a_file_with_no_settings_table_replays_a_file_from_the_working_directory() {
        let settings = load("").expect("it loads").settings;

        assert_eq!(settings.source.kind(), "replay");
        assert_eq!(settings.include, OutageKind::ALL.to_vec());
        assert_eq!((settings.stale(), settings.refresh()), (STALE, REFRESH));
        assert!(
            settings.source.open(Scope::default()).is_err(),
            "and says so by name when that file is not there",
        );
    }

    #[test]
    fn a_negative_duration_is_a_typo_rather_than_a_reason_not_to_start() {
        let settings = Settings {
            stale: chrono::Duration::seconds(-1),
            refresh: chrono::Duration::seconds(-1),
            ..Settings::default()
        };

        assert_eq!((settings.stale(), settings.refresh()), (STALE, REFRESH));
    }

    #[test]
    fn the_key_never_prints_itself() {
        let settings = load("[settings.source]\nkind = \"powercheck\"\napi_key = \"hunter2\"\n")
            .expect("it loads")
            .settings;

        assert!(!format!("{settings:?}").contains("hunter2"));
    }

    #[test]
    fn the_kinds_worth_a_marker_are_a_setting() {
        let settings = load(
            "[settings]\ninclude = [\"fault\"]\n\n[settings.source]\nkind = \"replay\"\npath = \"x\"\n",
        )
        .expect("it loads")
        .settings;

        assert_eq!(settings.include, vec![OutageKind::Fault]);
    }

    #[test]
    fn mistakes_are_refused_by_name() {
        for (wrong, names) in [
            ("[settings.source]\nkind = \"powercheck\"\n", "api_key"),
            (
                "[settings.source]\nkind = \"powercheck\"\napi_key = \"k\"\npol = \"5m\"\n",
                "pol",
            ),
            ("[settings]\ninclude = [\"fault\"]\n", "source"),
        ] {
            let err = load(wrong).expect_err(wrong);

            assert!(err.to_string().contains(names), "{wrong}: {err}");
        }
    }
}
