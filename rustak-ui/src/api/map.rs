//! The map's reads: everything current, what changes afterwards, and where
//! one thing has been.
//!
//! [`features`] and [`history`] are ordinary requests. [`open`] is not: it is a Server-Sent
//! Events response read off a `fetch` body (see [`sse`](super::sse) for why it
//! is not an `EventSource`), and it stays open for as long as the page does.
//! A page opens the feed *first* and reads the snapshot second, so anything
//! relayed in between arrives twice rather than not at all, and merges by the
//! event's own time.

use std::collections::VecDeque;

use futures::future::{Either, select};
use gloo_timers::future::TimeoutFuture;
use js_sys::{Reflect, Uint8Array};
use rustak_api::{MapFeature, MapUpdate};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::ReadableStreamDefaultReader;

use crate::api::sse::Parser;
use crate::api::{ApiError, Verb, error_from_response, get_json, send};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// How long the feed may be silent before it is taken for dead.
///
/// The server writes a comment into an idle stream every twenty seconds, so
/// two of those missed is a connection that has gone without saying so — a
/// laptop that slept, a proxy that gave up.
const SILENCE_MS: u32 = 45_000;

/// Everything current that this account may see.
pub async fn features() -> Result<Vec<MapFeature>, ApiError> {
    demo!(Ok(fixtures::map_features()));

    get_json("/map/features").await
}

/// Where one uid has been over the last `secago` seconds, oldest first. The
/// server keeps the newest fixes when there are more than it will answer with.
pub async fn history(uid: &str, secago: i64) -> Result<Vec<MapFeature>, ApiError> {
    demo!(Ok(fixtures::map_history(uid, secago)));

    get_json(&format!(
        "/map/features/{}/history?secago={secago}",
        urlencode(uid)
    ))
    .await
}

/// Opens the live feed.
pub async fn open() -> Result<Feed, ApiError> {
    demo!(Ok(Feed {
        source: Source::Demo(0),
        queue: VecDeque::new(),
    }));

    let response = send::<()>(Verb::Get, "/map/events", None).await?;
    if !response.ok() {
        return Err(error_from_response(response).await);
    }

    let body = response
        .body()
        .ok_or_else(|| ApiError::Network("The live feed had no body to read.".to_string()))?;

    Ok(Feed {
        source: Source::Live {
            reader: body.get_reader().unchecked_into(),
            parser: Parser::default(),
        },
        queue: VecDeque::new(),
    })
}

/// An open feed.
pub struct Feed {
    source: Source,
    queue: VecDeque<MapUpdate>,
}

enum Source {
    Live {
        reader: ReadableStreamDefaultReader,
        parser: Parser,
    },

    /// Demo mode's feed: the fixtures, moving. The number is how many times it
    /// has ticked.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    Demo(u32),
}

/// Ends a feed from outside the task that is reading it.
#[derive(Clone)]
pub struct Canceller(Option<ReadableStreamDefaultReader>);

impl Canceller {
    /// Cancels the read in flight, which ends the request behind it.
    pub fn cancel(&self) {
        if let Some(reader) = &self.0 {
            let _ = reader.cancel();
        }
    }
}

/// A feed nobody is reading is a request nobody is reading, and the server
/// holds a place for it until the request ends. Dropping the reader does not
/// end it — only cancelling does — so every way out of reading a feed,
/// including an early return, goes through here.
impl Drop for Feed {
    fn drop(&mut self) {
        self.canceller().cancel();
    }
}

impl Feed {
    pub fn canceller(&self) -> Canceller {
        match &self.source {
            Source::Live { reader, .. } => Canceller(Some(reader.clone())),
            Source::Demo(_) => Canceller(None),
        }
    }

    /// The next update, or [`None`] once the feed has ended — closed by the
    /// server, cancelled by the page, or silent for too long.
    pub async fn next(&mut self) -> Option<MapUpdate> {
        loop {
            if let Some(update) = self.queue.pop_front() {
                return Some(update);
            }

            match &mut self.source {
                Source::Live { reader, parser } => {
                    let bytes = read(reader).await?;

                    // A frame this build has never heard of is skipped rather
                    // than ending the stream, which is what lets the server
                    // grow a new kind of update.
                    self.queue.extend(
                        parser
                            .push(&bytes)
                            .iter()
                            .filter_map(|frame| serde_json::from_str(&frame.data).ok()),
                    );
                }
                Source::Demo(tick) => {
                    TimeoutFuture::new(2_000).await;
                    *tick += 1;

                    #[cfg(debug_assertions)]
                    self.queue.extend(fixtures::map_tick(*tick));
                }
            }
        }
    }
}

/// The next chunk of the body, or [`None`] when there will not be one.
async fn read(reader: &ReadableStreamDefaultReader) -> Option<Vec<u8>> {
    let next = JsFuture::from(reader.read());

    let chunk = match select(next, TimeoutFuture::new(SILENCE_MS)).await {
        Either::Left((chunk, _)) => chunk.ok()?,
        Either::Right(_) => {
            let _ = reader.cancel();
            return None;
        }
    };

    let field = |name: &str| Reflect::get(&chunk, &JsValue::from_str(name)).ok();
    if field("done")?.is_truthy() {
        return None;
    }

    Some(Uint8Array::new(&field("value")?).to_vec())
}
