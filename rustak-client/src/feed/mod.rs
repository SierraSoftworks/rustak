//! Information feeds: turning an open data source into tracks on a map.
//!
//! AIS gives ships and ADS-B gives aircraft, and from a TAK server's point of
//! view they are the same plugin twice: subscribe to a source over an area,
//! turn each observation into a CoT track, publish it on a channel at a rate a
//! phone can carry, and let it go stale when the source stops reporting it.
//! Everything in that sentence except "subscribe to a source" is here, so that
//! a feed plugin is its parsing and nothing else.
//!
//! | Piece | What it is |
//! |---|---|
//! | [`Track`] | One observation: an id, a position, a kind, and what to show |
//! | [`TrackKind`] | What the thing is, and the CoT type that says so |
//! | [`Affiliation`] | Whose side it is on, as far as an open feed can know |
//! | [`Area`] | Where the feed is looking — what it subscribes with, and what the publisher re-checks |
//! | [`PublishPolicy`] | How often the same track may be repeated, and how long it lives |
//! | [`FeedPublisher`] | The rate limiter, the expiry and the counters |
//! | [`Feed`] | An upstream, polled once per sidecar tick |
//! | [`Replay`] | A [`Feed`] over a file of tracks, for demonstrations and tests |
//!
//! # A whole feed plugin
//!
//! ```no_run
//! use rustak_client::feed::{Affiliation, Feed, FeedPublisher, PublishPolicy, Replay};
//! use rustak_client::sidecar::{Sidecar, SidecarContext, SidecarEvent, async_trait};
//! use rustak_core::prelude::*;
//! use rustak_cot::Event;
//!
//! #[derive(Default)]
//! struct Ships {
//!     feed: Option<Replay>,
//!     publisher: Option<FeedPublisher>,
//! }
//!
//! #[async_trait]
//! impl Sidecar for Ships {
//!     const NAME: &'static str = env!("CARGO_PKG_NAME");
//!     const VERSION: &'static str = env!("CARGO_PKG_VERSION");
//!     type Settings = rustak_client::sidecar::NoSettings;
//!
//!     async fn start(&mut self, _ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
//!         self.feed = Some(Replay::open("tracks.ndjson")?);
//!         self.publisher = Some(FeedPublisher::new(
//!             PublishPolicy::default(),
//!             Affiliation::Unknown,
//!         ));
//!
//!         Ok(())
//!     }
//!
//!     async fn tick(&mut self) -> Result<Vec<Event>, Error> {
//!         let (Some(feed), Some(publisher)) = (&mut self.feed, &mut self.publisher) else {
//!             return Ok(Vec::new());
//!         };
//!
//!         // An upstream that is down is a log line, never a stopped sidecar.
//!         match feed.poll().await {
//!             Ok(tracks) => tracks.into_iter().for_each(|track| {
//!                 publisher.offer(track);
//!             }),
//!             Err(err) => warn!(source = feed.name(), "The feed did not answer: {err}"),
//!         }
//!
//!         publisher.tick();
//!
//!         Ok(publisher.drain())
//!     }
//! }
//! ```
//!
//! # What a feed deliberately does not do
//!
//! It never sends a delete. TAK clients expire a track by the `stale` on the
//! last event they were given, so a sidecar that is stopped, killed or
//! disconnected leaves a map that empties itself over the next two minutes
//! rather than one full of ghosts — and a feed that had to send a delete per
//! track would be a feed that floods a channel every time it restarts.
//!
//! It also publishes no `<contact endpoint>`. A ship is a thing on the map, not
//! a chat peer, and an endpoint would invite an operator's client to try to
//! reach it.

mod area;
mod kind;
mod policy;
mod publish;
mod replay;
mod source;
mod symbol;
mod track;

pub use area::{Area, distance_m};
pub use kind::{Affiliation, AircraftClass, TrackKind, VesselClass};
pub use policy::PublishPolicy;
pub use publish::{FeedCounters, FeedPublisher};
pub use replay::Replay;
pub use source::Feed;
pub use symbol::Symbology;
pub use track::Track;
