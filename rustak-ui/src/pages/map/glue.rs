//! The Rust side of `js/map.js`.
//!
//! The boundary is narrow on purpose and everything that crosses it is either
//! a string of JSON or a uid: the JavaScript knows how to draw GeoJSON and
//! report a click, and knows nothing about CoT, channels or sessions. See the
//! head of `js/map.js` for what that buys when the map becomes editable.

use wasm_bindgen::prelude::*;
use web_sys::{Element, HtmlElement};

use super::focus::Pick;

#[wasm_bindgen(module = "/js/map.js")]
extern "C" {
    type MapHandle;

    #[wasm_bindgen(catch, js_name = createMap)]
    async fn create_map(
        container: &HtmlElement,
        options: &str,
        on_pick: &Closure<dyn Fn(String)>,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(method)]
    fn apply(this: &MapHandle, upserts: &str, removes: &str);

    #[wasm_bindgen(method)]
    fn select(this: &MapHandle, uid: Option<String>);

    #[wasm_bindgen(method)]
    fn choose(this: &MapHandle, lon: f64, lat: f64);

    #[wasm_bindgen(method, js_name = popoverElement)]
    fn popover_element(this: &MapHandle) -> Element;

    #[wasm_bindgen(method, js_name = flyTo)]
    fn fly_to(this: &MapHandle, uid: &str);

    #[wasm_bindgen(method, js_name = fitAll)]
    fn fit_all(this: &MapHandle);

    #[wasm_bindgen(method)]
    fn destroy(this: &MapHandle);
}

/// Where the base map comes from.
///
/// OpenStreetMap's own tile server: free, global, and asking only for the
/// attribution below and a light touch — which an admin console is. It is a
/// parameter rather than a constant inside the JavaScript because the next
/// thing an installation without internet access will want is to point it
/// somewhere else.
#[derive(serde::Serialize)]
pub struct Basemap {
    pub tiles: [&'static str; 1],
    pub attribution: &'static str,
    #[serde(rename = "maxZoom")]
    pub max_zoom: u8,
    /// `[lon, lat]`, and only until the first snapshot says where things are.
    pub center: [f64; 2],
    pub zoom: f64,
}

impl Default for Basemap {
    fn default() -> Self {
        Self {
            tiles: ["https://tile.openstreetmap.org/{z}/{x}/{y}.png"],
            attribution: "© <a href=\"https://www.openstreetmap.org/copyright\" \
                          target=\"_blank\" rel=\"noreferrer\">OpenStreetMap</a> contributors",
            max_zoom: 19,
            center: [0.0, 25.0],
            zoom: 1.4,
        }
    }
}

/// A map on the page. Dropping it takes the map away.
pub struct Map {
    handle: MapHandle,

    /// Held because the JavaScript calls it for as long as the map exists.
    _on_pick: Closure<dyn Fn(String)>,
}

impl Map {
    /// Draws a map into `container`, fetching the libraries if this is the
    /// first one.
    ///
    /// # Errors
    ///
    /// Whatever the browser said, as a sentence: the libraries did not load,
    /// or WebGL is not available.
    pub async fn create(
        container: &HtmlElement,
        basemap: &Basemap,
        on_pick: impl Fn(Pick) + 'static,
    ) -> Result<Self, String> {
        // A click the glue described in a way this build cannot read is a
        // click on nothing, which closes the pop-over rather than guessing.
        let on_pick = Closure::<dyn Fn(String)>::new(move |described: String| {
            on_pick(serde_json::from_str(&described).unwrap_or(Pick {
                uids: Vec::new(),
                at: [0.0, 0.0],
            }));
        });
        let options = serde_json::to_string(basemap).map_err(|err| err.to_string())?;

        let handle = create_map(container, &options, &on_pick)
            .await
            .map_err(|err| {
                js_sys::Reflect::get(&err, &JsValue::from_str("message"))
                    .ok()
                    .and_then(|message| message.as_string())
                    .unwrap_or_else(|| "The map could not be started.".to_string())
            })?;

        Ok(Self {
            handle: handle.unchecked_into(),
            _on_pick: on_pick,
        })
    }

    /// Draws `upserts` — what [`render::draw`](super::render::draw) produced —
    /// and forgets `removes`.
    pub fn apply(&self, upserts: &[serde_json::Value], removes: &[String]) {
        if upserts.is_empty() && removes.is_empty() {
            return;
        }

        self.handle.apply(
            &serde_json::Value::from(upserts).to_string(),
            &serde_json::Value::from(removes).to_string(),
        );
    }

    /// Opens the pop-over on a uid, or closes it.
    pub fn select(&self, uid: Option<&str>) {
        self.handle.select(uid.map(str::to_owned));
    }

    /// Opens the pop-over on a place, `[lon, lat]`, for the chooser.
    pub fn choose(&self, at: [f64; 2]) {
        self.handle.choose(at[0], at[1]);
    }

    /// The element the pop-over's content is portalled into.
    pub fn popover_element(&self) -> Element {
        self.handle.popover_element()
    }

    pub fn fly_to(&self, uid: &str) {
        self.handle.fly_to(uid);
    }

    pub fn fit_all(&self) {
        self.handle.fit_all();
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        self.handle.destroy();
    }
}
