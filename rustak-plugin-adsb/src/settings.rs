//! The `[settings]` table: what this sidecar watches, and where it reads it.
//!
//! Every key is documented with its default in `config.example.toml`, which
//! this crate's test suite loads so that the two cannot drift apart.

use std::path::PathBuf;
use std::time::Duration;

use rustak_client::feed::{Affiliation, Area, PublishPolicy, Symbology};
use rustak_core::config::duration;
use rustak_core::prelude::*;

use crate::sources::{AdsbFeed, AggregatorFeed, OpenSkyFeed, Provider, ReadsbFeed, ReplayFeed};

/// How long an aircraft stays on a map without another report.
///
/// Shorter than the [`PublishPolicy`] default, which is set for AIS: an
/// aircraft reports once a second, so one that has said nothing for ninety
/// seconds has left the area, landed, or was never really there.
pub const STALE: Duration = Duration::from_secs(90);

/// How far an aircraft moves before it is worth saying so, in metres.
///
/// Twice the module default, because an airliner at cruise covers 25 m in a
/// tenth of a second and a feed that published every 25 m would publish at the
/// rate the receiver hears, which is what `min_interval` exists to stop.
pub const MIN_MOVE_M: f64 = 50.0;

/// How often a `readsb` receiver is read. It is the operator's own hardware and
/// it rewrites the file about once a second.
const READSB_POLL: Duration = Duration::from_secs(1);

/// Where this plugin reads observations from.
///
/// `#[serde(tag = "kind")]`, so a settings file names the upstream it means:
///
/// ```toml
/// [settings.source]
/// kind = "readsb"
/// url_or_path = "http://receiver.lan/data/aircraft.json"
/// poll = "1s"
/// ```
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// A file of tracks, replayed on every tick. What the demonstration and the
    /// integration suite use, and what proves the rest of the plugin works
    /// without an upstream to be down.
    Replay {
        /// The newline-delimited JSON file; see
        /// [`Replay`](rustak_client::feed::Replay) for the format.
        path: PathBuf,
    },

    /// A `readsb`/`dump1090` decoder's own `aircraft.json`, by path or by URL.
    Readsb {
        /// `/run/readsb/aircraft.json`, or `http://receiver/data/aircraft.json`.
        url_or_path: String,

        /// How often to read it. Default: `"1s"`.
        #[serde(default = "readsb_poll", with = "duration::humane")]
        poll: chrono::Duration,
    },

    /// A public aggregator's point endpoint.
    Aggregator {
        /// Which pool of receivers: `adsb_lol`, `adsb_fi` or `airplanes_live`.
        provider: Provider,

        /// How often to ask. Default: the provider's own — `"10s"` for
        /// `adsb_lol` and `airplanes_live`, `"5s"` for `adsb_fi` — and never
        /// faster than two seconds whatever this says.
        ///
        /// A **floor, not a pin**: the source starts here and never asks
        /// faster, but a provider that rate-limits is asked less often for as
        /// long as it does. A `429` with a `Retry-After` twice inside ten polls
        /// raises the interval to what it asked for, for the rest of the
        /// process; two that name no delay raise it by half as much again (to
        /// two minutes at most), and sixty clean polls in a row earn one step
        /// back towards this value.
        #[serde(default, with = "duration::humane_option")]
        poll: Option<chrono::Duration>,
    },

    /// The OpenSky Network's state vectors, anonymously or with an OAuth2
    /// client credential.
    #[serde(rename = "opensky")]
    OpenSky {
        /// The API client's id. Default: none, which is the anonymous tier.
        #[serde(default)]
        client_id: Option<String>,

        /// Its secret. Write it as `"${{ env.NAME }}"`.
        #[serde(default, deserialize_with = "optional_secret")]
        client_secret: Option<Secret>,

        /// How often to ask. Default: `"10s"` anonymously, `"5s"` with a
        /// credential — OpenSky's own two resolutions.
        #[serde(default, with = "duration::humane_option")]
        poll: Option<chrono::Duration>,
    },
}

impl Source {
    /// Opens the upstream this setting names.
    ///
    /// `area` is what the sidecar is watching: the aggregators subscribe with a
    /// centre and a radius and OpenSky with a box, so the area reaches the
    /// upstream as well as the publisher.
    ///
    /// # Errors
    ///
    /// Whatever the source could not do, as something the operator can fix: a
    /// replay file that is missing or malformed names itself, and a URL that
    /// will not parse names the setting it came from.
    pub fn open(&self, area: Area) -> Result<Box<dyn AdsbFeed>, Error> {
        match self {
            Self::Replay { path } => Ok(Box::new(ReplayFeed::open(path)?)),
            Self::Readsb { url_or_path, poll } => Ok(Box::new(ReadsbFeed::open(
                url_or_path,
                std(*poll, READSB_POLL),
            )?)),
            Self::Aggregator { provider, poll } => Ok(Box::new(AggregatorFeed::open(
                *provider,
                area,
                poll.and_then(|poll| poll.to_std().ok()),
            )?)),
            Self::OpenSky {
                client_id,
                client_secret,
                poll,
            } => Ok(Box::new(OpenSkyFeed::open(
                area,
                client_id.clone(),
                client_secret.clone(),
                poll.and_then(|poll| poll.to_std().ok()),
            )?)),
        }
    }

    /// What to call this kind of source in a heartbeat, before one is open.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Replay { .. } => "replay",
            Self::Readsb { .. } => "readsb",
            Self::Aggregator { .. } => "aggregator",
            Self::OpenSky { .. } => "opensky",
        }
    }
}

/// `[settings]` — what this sidecar watches, and how loudly it says so.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Where the feed is looking. Default: the whole world.
    #[serde(default)]
    pub area: Area,

    /// How often an aircraft may be republished, and how long it lives.
    ///
    /// Default: the [`PublishPolicy`] defaults with a `stale` of 90 seconds and
    /// a `min_move_m` of 50. **A `[settings.publish]` table that omits a key
    /// gets the module default for it** rather than this plugin's, which is why
    /// the example file writes every key out: a partial table is filled in key
    /// by key, not from this function.
    #[serde(default = "default_publish")]
    pub publish: PublishPolicy,

    /// What these tracks are to the operator. Default: `unknown`, because open
    /// ADS-B says nothing about whose side an airframe is on.
    #[serde(default)]
    pub affiliation: Affiliation,

    /// Which MIL-STD-2525 symbol code each track carries beside its CoT type:
    /// `none`, `2525c` or `2525d`. Default: `none`, the type alone.
    ///
    /// A device draws a bare type from the 2525C tables whatever edition it is
    /// set to, so a fleet on 2525D sets this to have these tracks drawn in the
    /// edition its own markers are.
    #[serde(default)]
    pub symbology: Symbology,

    /// The upstream. Required, because choosing one is the whole deployment
    /// decision.
    pub source: Source,
}

impl Default for Settings {
    /// Only reached by a configuration file with no `[settings]` table, which
    /// is a sidecar that has not been told where to look: it replays
    /// `tracks.ndjson` from the working directory, and says so by name when
    /// that file is not there.
    fn default() -> Self {
        Self {
            area: Area::default(),
            publish: default_publish(),
            affiliation: Affiliation::default(),
            symbology: Symbology::default(),
            source: Source::Replay {
                path: PathBuf::from("tracks.ndjson"),
            },
        }
    }
}

/// The publishing policy an ADS-B deployment starts from.
pub fn default_publish() -> PublishPolicy {
    PublishPolicy {
        min_move_m: MIN_MOVE_M,
        ..PublishPolicy::default().with_stale(STALE)
    }
}

/// A [`chrono::Duration`] as a [`std::time::Duration`], falling back rather
/// than failing: a negative interval in a file is a typo, not a reason to
/// refuse to start.
fn std(value: chrono::Duration, fallback: Duration) -> Duration {
    value.to_std().unwrap_or(fallback)
}

/// The default for [`Source::Readsb`]'s `poll`.
fn readsb_poll() -> chrono::Duration {
    chrono::Duration::from_std(READSB_POLL).unwrap_or_else(|_| chrono::Duration::seconds(1))
}

/// Reads an optional string into a [`Secret`], which redacts itself when
/// logged. The same shape `rustak_client::sidecar::config` uses for the service
/// token, and for the same reason.
fn optional_secret<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Secret>, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.map(Secret::new))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_client::sidecar::SidecarConfig;

    fn settings(source: &str) -> Settings {
        rustak_core::config::load_str::<SidecarConfig<Settings>>(&format!(
            "[service]\nname = \"adsb\"\n\n{source}",
        ))
        .expect("the configuration loads")
        .settings
    }

    #[test]
    fn a_readsb_source_takes_a_path_or_a_url_and_a_poll_interval() {
        let settings = settings(
            "[settings.source]\nkind = \"readsb\"\nurl_or_path = \"/run/readsb/aircraft.json\"\npoll = \"2s\"\n",
        );

        match &settings.source {
            Source::Readsb { url_or_path, poll } => {
                assert_eq!(url_or_path, "/run/readsb/aircraft.json");
                assert_eq!(*poll, chrono::Duration::seconds(2));
            }
            other => panic!("expected a readsb source, got {other:?}"),
        }
    }

    #[test]
    fn a_readsb_source_that_omits_the_poll_reads_the_file_every_second() {
        let settings = settings(
            "[settings.source]\nkind = \"readsb\"\nurl_or_path = \"/run/readsb/aircraft.json\"\n",
        );

        match &settings.source {
            Source::Readsb { poll, .. } => assert_eq!(*poll, chrono::Duration::seconds(1)),
            other => panic!("expected a readsb source, got {other:?}"),
        }
    }

    #[test]
    fn each_aggregator_is_named_by_its_provider() {
        for (written, expected) in [
            ("adsb_lol", Provider::AdsbLol),
            ("adsb_fi", Provider::AdsbFi),
            ("airplanes_live", Provider::AirplanesLive),
        ] {
            let settings = settings(&format!(
                "[settings.source]\nkind = \"aggregator\"\nprovider = \"{written}\"\n",
            ));

            match &settings.source {
                Source::Aggregator { provider, poll } => {
                    assert_eq!(*provider, expected);
                    assert!(
                        poll.is_none(),
                        "resolved from the provider, not from a default written here",
                    );
                }
                other => panic!("expected an aggregator, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_aggregator_that_names_no_poll_opens_at_the_providers_own_rate() {
        // The default is the provider's, so the setting has to be resolved
        // rather than defaulted: a `5s` written here would have put adsb.lol
        // back on the cadence it refused.
        for (written, expected) in [("adsb_lol", 10), ("adsb_fi", 5), ("airplanes_live", 10)] {
            let feed = settings(&format!(
                "[settings.source]\nkind = \"aggregator\"\nprovider = \"{written}\"\n",
            ))
            .source
            .open(Area::default())
            .expect("it opens");

            assert_eq!(
                feed.state().interval(),
                Duration::from_secs(expected),
                "{written}",
            );
        }
    }

    #[test]
    fn an_aggregator_poll_faster_than_the_floor_is_clamped_rather_than_refused() {
        let feed = settings(
            "[settings.source]\nkind = \"aggregator\"\nprovider = \"adsb_lol\"\npoll = \"1s\"\n",
        )
        .source
        .open(Area::default())
        .expect("it opens");

        assert_eq!(feed.state().interval(), Duration::from_secs(2));
    }

    #[test]
    fn an_opensky_source_is_anonymous_until_it_is_given_a_credential() {
        let settings = settings("[settings.source]\nkind = \"opensky\"\n");

        match &settings.source {
            Source::OpenSky {
                client_id,
                client_secret,
                poll,
            } => {
                assert!(client_id.is_none());
                assert!(client_secret.is_none());
                assert!(poll.is_none(), "resolved from the tier, not from the file");
            }
            other => panic!("expected OpenSky, got {other:?}"),
        }
    }

    #[test]
    fn an_opensky_credential_never_prints_itself() {
        let settings = settings(
            "[settings.source]\nkind = \"opensky\"\nclient_id = \"rustak-api-client\"\nclient_secret = \"hunter2\"\npoll = \"5s\"\n",
        );

        let rendered = format!("{:?}", settings.source);

        assert!(
            rendered.contains("rustak-api-client"),
            "the id is not a secret"
        );
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("Secret(***)"), "{rendered}");
    }

    #[test]
    fn a_misspelled_source_key_is_refused_by_name() {
        let refused = rustak_core::config::load_str::<SidecarConfig<Settings>>(
            "[service]\nname = \"adsb\"\n\n[settings.source]\nkind = \"readsb\"\nurl = \"/x\"\n",
        );

        let err = refused.expect_err("`url` is not `url_or_path`");

        assert!(err.to_string().contains("url"), "{err}");
    }

    #[test]
    fn an_unknown_source_kind_is_refused() {
        let refused = rustak_core::config::load_str::<SidecarConfig<Settings>>(
            "[service]\nname = \"adsb\"\n\n[settings.source]\nkind = \"flightradar\"\n",
        );

        assert!(refused.is_err(), "there is no such source");
    }

    #[test]
    fn the_plugin_defaults_are_the_ones_this_brief_asks_for() {
        let policy = default_publish();

        assert_eq!(policy.stale(), STALE);
        assert_eq!(policy.min_interval(), Duration::from_secs(5));
        assert_eq!(policy.max_interval(), Duration::from_secs(60));
        assert!((policy.min_move_m - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn every_source_answers_what_kind_it_is() {
        assert_eq!(
            Settings::default().source.kind(),
            "replay",
            "a sidecar that was told nothing replays a file",
        );
        assert_eq!(
            settings("[settings.source]\nkind = \"readsb\"\nurl_or_path = \"/x\"\n")
                .source
                .kind(),
            "readsb",
        );
        assert_eq!(
            settings("[settings.source]\nkind = \"aggregator\"\nprovider = \"adsb_fi\"\n")
                .source
                .kind(),
            "aggregator",
        );
        assert_eq!(
            settings("[settings.source]\nkind = \"opensky\"\n")
                .source
                .kind(),
            "opensky",
        );
    }

    #[test]
    fn a_live_source_opens_without_reaching_its_upstream() {
        // A receiver that is down at start-up is an outage rather than a
        // configuration error: `open` builds the client and returns.
        let source = settings(
            "[settings.source]\nkind = \"readsb\"\nurl_or_path = \"http://receiver.lan/data/aircraft.json\"\n",
        )
        .source;

        let feed = source.open(Area::default()).expect("it opens");

        assert!(!feed.state().ever_connected());
        assert_eq!(
            feed.state().name(),
            "http://receiver.lan/data/aircraft.json",
        );
    }
}
