//! `rustak-plugin-ais` — ships on the map, from open AIS data.
//!
//! AIS is what vessels broadcast about themselves over VHF: an MMSI, a name, a
//! position, a speed and a course, every few seconds under way and every few
//! minutes at anchor. Coastal receivers and public aggregators put that on the
//! internet for nothing, and this sidecar is what turns it into CoT tracks on a
//! rustak channel.
//!
//! # What is here
//!
//! | Module | What it does |
//! |---|---|
//! | [`sources`] | The upstreams: AISStream.io, a receiver of your own over UDP, and a replay file |
//! | [`mapping`] | AIS's vocabulary — ship types, navigational statuses, sentinels — as a [`Track`](rustak_client::feed::Track) |
//! | [`vessels`] | The per-MMSI memory that joins a position report to the name that arrives six minutes later |
//! | [`status`] | What the admin UI's Services page shows about this feed |
//!
//! Everything else — the CoT type, the area filter, the publishing rate, the
//! staleness — lives in [`rustak_client::feed`] and is shared with the ADS-B
//! plugin.
//!
//! # Two staleness horizons
//!
//! A vessel at anchor reports every three minutes and a vessel under way every
//! few seconds, so one staleness cannot suit both: too short and the anchored
//! hull flickers, too long and a track that stopped being reported sits on the
//! map for ten minutes pretending to be current. Anchored, moored and aground
//! vessels therefore get `[settings.publish] stale`, and everything else gets
//! [`Settings::under_way_stale`] — see `config.example.toml`.
//!
//! # Running it
//!
//! ```text
//! rustak-plugin-ais --config plugin.toml [--env .env] [--check]
//! ```
//!
//! See `config.example.toml` for every setting with its default, and
//! `docs/plugins.md` for the sidecar contract this follows.

pub mod mapping;
pub mod sources;
pub mod status;
pub mod vessels;

use std::collections::HashSet;
use std::time::Duration;

use chrono::Utc;
use rustak_api::Heartbeat;
use rustak_client::feed::{
    Affiliation, Area, Feed, FeedCounters, FeedPublisher, PublishPolicy, Symbology,
};
use rustak_client::sidecar::{ServiceSettings, Sidecar, SidecarContext, SidecarEvent, async_trait};
use rustak_core::config::duration;
use rustak_core::prelude::*;
use rustak_cot::Event;

use sources::{Source, SourceContext};
use status::{ConnectionRx, FeedStatus};

/// How long a vessel that is under way stays on a map without another report.
fn default_under_way_stale() -> chrono::Duration {
    chrono::Duration::seconds(120)
}

/// The publishing policy this plugin starts from.
///
/// Not [`PublishPolicy::default`]: AIS wants a longer memory than the module
/// default, because that is what an anchored vessel reporting every three
/// minutes needs, and a `max_interval` under it so that such a vessel is
/// refreshed before it expires.
fn default_publish() -> PublishPolicy {
    PublishPolicy {
        stale: chrono::Duration::seconds(600),
        max_interval: chrono::Duration::seconds(180),
        ..PublishPolicy::default()
    }
}

/// `[settings]` — what this sidecar watches, and how loudly it says so.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Where the feed is looking. Default: the whole world.
    #[serde(default)]
    pub area: Area,

    /// How often a vessel may be republished, and how long an *anchored* one
    /// lives. Default: `stale = "10m"`, `min_interval = "5s"`,
    /// `max_interval = "3m"`, `min_move_m = 25`, `max_tracks = 5000`.
    #[serde(default = "default_publish")]
    pub publish: PublishPolicy,

    /// How long a vessel that is **not** anchored, moored or aground stays on a
    /// map without another report. Default: `"2m"`.
    ///
    /// Raised to `max_interval + min_interval` when that is longer, and logged
    /// at `info` when it is: a track whose staleness is shorter than the gap
    /// between two refreshes drops off the map and comes back.
    #[serde(default = "default_under_way_stale", with = "duration::humane")]
    pub under_way_stale: chrono::Duration,

    /// What these tracks are to the operator. Default: `unknown`, because open
    /// AIS says nothing about whose side a hull is on.
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
            under_way_stale: default_under_way_stale(),
            affiliation: Affiliation::default(),
            symbology: Symbology::default(),
            source: Source::Replay {
                path: "tracks.ndjson".into(),
            },
        }
    }
}

impl Settings {
    /// How long a vessel that is under way lives on a map, never shorter than
    /// the gap between two refreshes of one that is not moving.
    #[must_use]
    pub fn under_way_stale(&self) -> Duration {
        let asked = self
            .under_way_stale
            .to_std()
            .unwrap_or_else(|_| Duration::from_secs(120));

        asked.max(self.publish.max_interval() + self.publish.min_interval())
    }
}

/// The plugin: one upstream, one publisher, and what it has done so far.
#[derive(Default)]
pub struct AisSidecar {
    context: Option<SidecarContext<Settings>>,
    publisher: Option<FeedPublisher>,
    feed: Option<Box<dyn Feed>>,
    connection: Option<ConnectionRx>,
    under_way_stale: Duration,

    /// The server's copy of this service's configuration, and when it is next
    /// worth reading.
    configured: ServiceSettings,

    /// The area in effect right now, whichever of the two it came from.
    area: Area,

    /// Where that area came from, for the line that says which is in effect.
    area_from: &'static str,
}

/// What [`AisSidecar::area_from`] says for each of the two.
const FROM_FILE: &str = "the configuration file";
const FROM_SERVER: &str = "an administrator, through the control API";

impl AisSidecar {
    /// What this feed has offered, published, suppressed and expired — for a
    /// heartbeat, or for a test that would rather assert than read a log.
    #[must_use]
    pub fn counters(&self) -> FeedCounters {
        self.publisher
            .as_ref()
            .map_or_else(FeedCounters::default, FeedPublisher::counters)
    }

    /// The area an administrator set in the admin UI, when they have set a new
    /// one.
    ///
    /// Server-side configuration wins over the file, because the file is baked
    /// into a container image and the UI is where an operator moves the box
    /// without a redeploy. Anything unreadable is a warning and the area
    /// already in effect, never a sidecar that will not start.
    ///
    /// It is asked **again after start-up**, because the read at start-up is
    /// the one most likely to fail — the control link may not have a credential
    /// yet, which is how the first live deployment lost an administrator's area
    /// for the whole life of a process. [`ServiceSettings`] owns the cadence.
    async fn configured_area(&mut self) -> Option<Area> {
        let context = self.context.clone()?;

        self.configured
            .setting::<Settings, Area>(&context, "area")
            .await
            .filter(|area| *area != self.area)
    }

    /// Opens the source and the publisher for `area`.
    ///
    /// The same lines whether this is a start or an administrator moving the
    /// box: an AIS feed subscribes with its area, so changing one means opening
    /// the source again.
    fn watch(
        &mut self,
        settings: &Settings,
        area: Area,
        from: &'static str,
        shutdown: &Shutdown,
    ) -> Result<(), Error> {
        let (connection, state) = status::connection();

        self.feed = Some(settings.source.open(SourceContext {
            area,
            policy: settings.publish,
            shutdown: shutdown.clone(),
            connection,
        })?);
        self.connection = Some(state);
        self.publisher = Some(
            FeedPublisher::new(settings.publish, settings.affiliation)
                .with_symbology(settings.symbology)
                .with_area(area),
        );
        self.area = area;
        self.area_from = from;

        Ok(())
    }

    /// Everything the harness should write, with each vessel's own staleness.
    ///
    /// The publisher stamps every event with one `stale`, which is the right
    /// shape for a feed whose tracks are alike; AIS's are not, so a vessel that
    /// is not anchored has its staleness shortened here. `stationary` is what
    /// was offered in this same tick, which is everything the publisher can
    /// have buffered.
    fn drain(&mut self, stationary: &HashSet<String>) -> Vec<Event> {
        let Some(publisher) = &mut self.publisher else {
            return Vec::new();
        };

        let under_way = self.under_way_stale;
        let mut events = publisher.drain();

        for event in &mut events {
            if !stationary.contains(&event.uid) {
                event.stale = event.time.stale_after(under_way);
            }
        }

        events
    }

    /// What this sidecar would report about itself right now.
    fn snapshot(&self) -> FeedStatus {
        FeedStatus {
            source: self
                .feed
                .as_ref()
                .map_or_else(|| "none".to_string(), |feed| feed.name().to_string()),
            connection: self
                .connection
                .as_ref()
                .map(|state| state.borrow().clone())
                .unwrap_or_default(),
            counters: self.counters(),
            tracked: self.publisher.as_ref().map_or(0, FeedPublisher::tracked),
        }
    }
}

#[async_trait]
impl Sidecar for AisSidecar {
    const NAME: &'static str = env!("CARGO_PKG_NAME");
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    type Settings = Settings;

    async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        self.area = ctx.settings().area;
        self.area_from = FROM_FILE;
        self.context = Some(ctx.clone());

        let (area, from) = match self.configured_area().await {
            Some(area) => (area, FROM_SERVER),
            None => (self.area, FROM_FILE),
        };
        let settings = ctx.settings();

        self.under_way_stale = settings.under_way_stale();

        if self.under_way_stale
            > settings
                .under_way_stale
                .to_std()
                .unwrap_or(self.under_way_stale)
        {
            info!(
                stale = ?self.under_way_stale,
                "A vessel under way is refreshed less often than it would expire; \
                 its staleness has been raised to match.",
            );
        }

        // Before anything else: a source that cannot be opened is a setting the
        // operator got wrong, and the one thing `start` should refuse over.
        self.watch(settings, area, from, ctx.shutdown())?;

        info!(
            uid = %ctx.identity().uid(),
            source = settings.source.kind(),
            ?area,
            area_from = self.area_from,
            affiliation = ?settings.affiliation,
            symbology = ?settings.symbology,
            "The AIS sidecar is watching.",
        );

        Ok(())
    }

    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        // An administrator may have moved the box since this started — or the
        // read that would have found it at start-up may simply not have worked
        // yet.
        if let Some(area) = self.configured_area().await
            && let Some(context) = self.context.clone()
        {
            match self.watch(context.settings(), area, FROM_SERVER, context.shutdown()) {
                Ok(()) => info!(
                    ?area,
                    area_from = self.area_from,
                    "The area an administrator set for this service is now in effect.",
                ),
                Err(err) => warn!(
                    error = %err,
                    "Could not open the source for the area an administrator set; the one already in effect stays in effect.",
                ),
            }
        }

        let mut stationary = HashSet::new();

        if let (Some(feed), Some(publisher)) = (&mut self.feed, &mut self.publisher) {
            match feed.poll().await {
                Ok(tracks) => {
                    for track in tracks {
                        if mapping::is_stationary(&track) {
                            stationary.insert(track.id.clone());
                        }

                        publisher.offer(track);
                    }
                }
                // An upstream that is down is an ordinary Tuesday for an open
                // feed: logged, never a stopped sidecar. The tracks it was
                // carrying age out on their own `stale`.
                Err(err) => warn!(source = feed.name(), "The AIS feed did not answer: {err}"),
            }

            publisher.tick();
        }

        Ok(self.drain(&stationary))
    }

    /// The harness asks after every tick, and reports exactly this.
    ///
    /// Everything it answers was worked out during the tick that has just
    /// finished — the source's connection state, the publisher's counters — so
    /// this reads rather than polls. [`None`] only before
    /// [`start`](Sidecar::start) has run, which is the one moment this plugin
    /// has nothing to say.
    async fn health(&mut self) -> Option<Heartbeat> {
        let poll = self.context.as_ref()?.config().sidecar.tick();

        Some(self.snapshot().heartbeat(poll, Utc::now()))
    }

    async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<Event>, Error> {
        match event {
            SidecarEvent::Connected { endpoint } => {
                info!(%endpoint, "Connected; every vessel will be published again.");

                // A reopened connection is a new subscription: the server has
                // none of what went down the old one, so the intervals that
                // would otherwise suppress the next report are cleared.
                if let Some(publisher) = &mut self.publisher {
                    publisher.refresh_all();
                }
            }
            SidecarEvent::Disconnected { reason } => {
                warn!(%reason, "The stream dropped; the harness will reconnect.");
            }
            _ => debug!("An event this sidecar does not handle."),
        }

        Ok(Vec::new())
    }

    async fn stop(&mut self) -> Result<(), Error> {
        let counters = self.counters();

        info!(
            offered = counters.offered,
            published = counters.published,
            suppressed = counters.suppressed,
            expired = counters.expired,
            "The AIS sidecar is stopping.",
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_client::feed::{Track, TrackKind, VesselClass};
    use rustak_client::sidecar::SidecarConfig;

    /// The file an operator is handed, loaded the way the binary loads it.
    const EXAMPLE: &str = include_str!("../config.example.toml");

    /// The demonstration fixture the example configuration names.
    const FIXTURE: &str = include_str!("../tracks.example.ndjson");

    fn config() -> SidecarConfig<Settings> {
        rustak_core::config::load_str(EXAMPLE).expect("config.example.toml should load")
    }

    async fn started(settings: &str) -> AisSidecar {
        let config: SidecarConfig<Settings> =
            rustak_core::config::load_str(&format!("[service]\nname = \"ais\"\n\n{settings}"))
                .expect("the configuration loads");
        let mut sidecar = AisSidecar::default();

        sidecar
            .start(
                SidecarContext::from_config(config, AisSidecar::VERSION, Shutdown::new())
                    .expect("a usable identity"),
            )
            .await
            .expect("the source opens");

        sidecar
    }

    #[test]
    fn the_example_configuration_file_is_one_this_plugin_can_load() {
        let config = config();

        assert_eq!(config.service.name.as_str(), "ais");
        assert_eq!(config.settings.affiliation, Affiliation::Unknown);
        assert_eq!(config.settings.symbology, Symbology::TypeOnly);
        assert!(matches!(config.settings.source, Source::Replay { .. }));
        assert!(config.settings.area.contains(51.95, 4.13));
        assert_eq!(config.settings.publish.stale(), Duration::from_secs(600));
        assert_eq!(
            config.settings.publish.max_interval(),
            Duration::from_secs(180),
        );
        assert_eq!(config.settings.publish.min_move_m, 25.0);
        assert_eq!(
            config.settings.under_way_stale(),
            Duration::from_secs(185),
            "raised off 2m by the three-minute refresh interval",
        );
    }

    #[test]
    fn the_demonstration_fixture_is_one_the_replay_source_reads() {
        // The example file names this fixture, so a change to either that broke
        // the other would ship a plugin whose demonstration does not run.
        let tracks: Vec<Track> = FIXTURE
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .map(|line| serde_json::from_str(line).expect("a fixture line"))
            .collect();

        assert_eq!(tracks.len(), 5);
        assert!(tracks.iter().all(|track| track.id.starts_with("AIS-")));
        assert!(
            tracks
                .iter()
                .all(|track| matches!(track.kind, TrackKind::Vessel(_))),
        );
        assert!(
            tracks
                .iter()
                .any(|track| track.kind == TrackKind::Vessel(VesselClass::Fishing)),
        );
    }

    #[tokio::test]
    async fn a_tick_publishes_the_replayed_vessels_once() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("tracks.ndjson");
        std::fs::write(&path, FIXTURE).expect("the fixture lands");

        let mut sidecar = started(&format!(
            "[settings.source]\nkind = \"replay\"\npath = \"{}\"\n",
            path.display(),
        ))
        .await;

        let published = sidecar.tick().await.expect("the first tick publishes");

        assert_eq!(published.len(), 5);
        assert!(published.iter().all(|event| event.uid.starts_with("AIS-")));
        assert!(
            published
                .iter()
                .all(|event| event.r#type.starts_with("a-u-S"))
        );

        // The second tick offers the same five observations and publishes none
        // of them: the policy is what makes a feed a rate rather than a flood.
        assert!(sidecar.tick().await.unwrap().is_empty());
        assert_eq!(sidecar.counters().published, 5);
        assert_eq!(sidecar.counters().suppressed, 5);

        sidecar.stop().await.unwrap();
    }

    #[tokio::test]
    async fn an_anchored_vessel_lives_five_times_as_long_on_the_map() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("tracks.ndjson");
        let moored = mapping::track(
            &mapping::Position {
                mmsi: 244_660_000,
                position: (51.95, 4.13),
                sog_knots: Some(0.0),
                cog_deg: None,
                heading_deg: None,
                nav_status: Some(5),
                observed_at: chrono::Utc::now(),
            },
            None,
            "replay",
        );
        let under_way = mapping::track(
            &mapping::Position {
                mmsi: 244_660_001,
                position: (51.96, 4.14),
                sog_knots: Some(12.0),
                cog_deg: Some(90.0),
                heading_deg: None,
                nav_status: Some(0),
                observed_at: chrono::Utc::now(),
            },
            None,
            "replay",
        );
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&moored).expect("a fixture line"),
                serde_json::to_string(&under_way).expect("a fixture line"),
            ),
        )
        .expect("the fixture lands");

        let mut sidecar = started(&format!(
            "[settings.source]\nkind = \"replay\"\npath = \"{}\"\n",
            path.display(),
        ))
        .await;

        let published = sidecar.tick().await.expect("both vessels");
        let moored = published
            .iter()
            .find(|event| event.uid == "AIS-244660000")
            .expect("the moored vessel");
        let under_way = published
            .iter()
            .find(|event| event.uid == "AIS-244660001")
            .expect("the vessel under way");

        assert_eq!(moored.stale.millis() - moored.time.millis(), 600_000);
        assert_eq!(
            under_way.stale.millis() - under_way.time.millis(),
            185_000,
            "2m raised to the refresh interval plus the floor",
        );
    }

    #[tokio::test]
    async fn a_reconnection_publishes_every_vessel_again() {
        let mut sidecar = AisSidecar {
            publisher: Some(FeedPublisher::new(
                PublishPolicy::default(),
                Affiliation::Unknown,
            )),
            ..AisSidecar::default()
        };

        let publisher = sidecar.publisher.as_mut().unwrap();
        publisher.offer(Track::new(
            "AIS-1",
            TrackKind::Vessel(VesselClass::Merchant),
            (51.95, 4.13),
            chrono::Utc::now(),
        ));
        let _ = publisher.drain();

        let republished = sidecar
            .on_event(SidecarEvent::Connected {
                endpoint: "ssl://tak.example.com:8089".to_string(),
            })
            .await
            .unwrap();

        assert!(republished.is_empty(), "the next tick carries them");

        let publisher = sidecar.publisher.as_mut().unwrap();

        assert!(
            publisher.offer(Track::new(
                "AIS-1",
                TrackKind::Vessel(VesselClass::Merchant),
                (51.95, 4.13),
                chrono::Utc::now(),
            )),
            "an interval that would have suppressed this was cleared",
        );
    }

    #[test]
    fn a_settings_table_without_a_source_is_refused_by_name() {
        let refused = rustak_core::config::load_str::<SidecarConfig<Settings>>(
            "[service]\nname = \"ais\"\n\n[settings]\naffiliation = \"neutral\"\n",
        );

        let err = refused.expect_err("a sidecar with no upstream is not a sidecar");

        assert!(err.to_string().contains("source"), "{err}");
    }

    #[tokio::test]
    async fn a_running_sidecar_reports_what_it_is_carrying() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("tracks.ndjson");
        std::fs::write(&path, FIXTURE).expect("the fixture lands");

        let mut sidecar = started(&format!(
            "[settings.source]\nkind = \"replay\"\npath = \"{}\"\n",
            path.display(),
        ))
        .await;
        let _ = sidecar.tick().await.expect("a tick");

        // Through the hook rather than the builder: what this answers is what
        // the harness reports, and what the Services page therefore shows.
        let beat = sidecar.health().await.expect("a started sidecar reports");

        assert_eq!(beat.state, rustak_api::ServiceState::Healthy);
        assert_eq!(beat.metrics["source"]["kind"], "replay");
        assert_eq!(beat.metrics["tracked"], 5);
        assert_eq!(beat.metrics["published"], 5);
        assert!(beat.message.is_some());
    }

    #[tokio::test]
    async fn a_sidecar_that_has_not_started_leaves_the_harness_its_floor() {
        // `None` is "nothing to add", which the harness reports as healthy —
        // the window between the process starting and `start` opening a source.
        assert!(AisSidecar::default().health().await.is_none());
    }

    #[test]
    fn an_under_way_staleness_longer_than_the_refresh_is_left_alone() {
        let settings: Settings = toml::from_str(
            "under_way_stale = \"10m\"\n\n[source]\nkind = \"replay\"\npath = \"t.ndjson\"\n",
        )
        .expect("the settings load");

        assert_eq!(settings.under_way_stale(), Duration::from_secs(600));
    }
}
