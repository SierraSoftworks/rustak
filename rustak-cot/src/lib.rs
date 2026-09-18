//! CoT XML model and clean-room TAK Protocol v1 codecs.
//!
//! This crate has no I/O and no server dependencies: it is the shared,
//! wasm-safe vocabulary for talking about Cursor-on-Target events, used by
//! both `rustak-server` and `rustak-client`.
//!
//! # The model
//!
//! An [`Event`] is the whole message: a fixed envelope, a required [`Point`],
//! and an open-ended [`Detail`] tree of [`Element`]s, [`Node::Text`], CDATA
//! and comments. The tree keeps element order, attribute order and values, so
//! a message rustak relays carries everything the sender wrote — including the
//! parts rustak does not understand.
//!
//! Known `<detail>` children have typed views ([`TypedDetail`]) that read and
//! write the tree without disturbing anything else:
//!
//! ```
//! use rustak_cot::detail::{Contact, TypedDetail};
//! use rustak_cot::{CotTime, Event, xml};
//! use std::time::Duration;
//!
//! let event = Event::builder("a-f-G-U-C", "UID-A")
//!     .how("m-g")
//!     .point(51.5074, -0.1278)
//!     .time(CotTime::from_millis(1_789_646_400_000))
//!     .stale_after(Duration::from_secs(60))
//!     .typed(&Contact::new("ALPHA").with_endpoint("*:-1:stcp"))
//!     .build();
//!
//! let bytes = xml::write(&event);
//! assert_eq!(xml::parse(&bytes).unwrap(), event);
//! assert_eq!(event.callsign(), Some("ALPHA"));
//! ```
//!
//! # What ships when
//!
//! M0 shipped the generated-protobuf pipeline (see [`proto`]). M1 adds the
//! event model, the XML parser/writer and the typed details; the wire codecs,
//! protobuf conversion and control-message templates follow in the same
//! milestone.

pub mod detail;
pub mod error;
pub mod event;
pub mod proto;
pub mod time;
pub mod types;
pub mod xml;

pub use detail::{Detail, Element, Node, TypedDetail};
pub use error::{ConvertError, ParseError};
pub use event::{Event, EventBuilder, Point};
pub use time::CotTime;
pub mod codec;
pub mod msgs;
pub mod negotiate;
