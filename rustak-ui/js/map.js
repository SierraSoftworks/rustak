// The map itself: MapLibre GL drawing OpenStreetMap tiles, with milsymbol
// drawing MIL-STD-2525 symbols on top. Everything else — what is on the map,
// what it looks like, what the pop-over says — is decided in Rust
// (`src/pages/map`), which hands this file GeoJSON that is ready to draw.
//
// That split is deliberate. This file knows about pixels and nothing about
// CoT. Drawing keeps to it: Rust decides what the clicks so far add up to and
// hands back a sketch to draw (`showSketch`), and this file says only what
// the pointer did — where it clicked and whether that was on one of the
// sketch's handles, where it is hovering, which handle it dragged where. The
// questions that matter — what a drawing is, which channel it is published
// to, and as what — stay with the code that can answer them.
//
// Both libraries are served by rustak from /vendor (see scripts/vendor.mjs)
// and are fetched the first time a map is opened, not with the console.

const VENDOR = "/vendor";

// Layers a click may land on, topmost first. Labels are not among them: a
// callsign is as wide as several markers, so a label that could be clicked
// would take the clicks meant for whatever it happens to be drawn across.
const HIT_LAYERS = ["symbols", "dots", "shape-lines", "shape-fills"];

// How close to a handle, in pixels, counts as on it: a fingertip's worth.
const HANDLE_REACH = 9;

const SKETCH = "#14664f";

const LABEL_FONT = '600 12px system-ui, -apple-system, "Segoe UI", sans-serif';

let libraries;
function stylesheet(href) {
  return new Promise((resolve, reject) => {
    const link = Object.assign(document.createElement("link"), { rel: "stylesheet", href });
    link.addEventListener("load", resolve);
    link.addEventListener("error", () => reject(new Error(`Could not load ${href}.`)));
    document.head.append(link);
  });
}

function load() {
  libraries ??= Promise.all([
    import(`${VENDOR}/maplibre-gl/maplibre-gl.mjs`),
    // A UMD build: evaluated as a module it finds no loader and leaves itself
    // on `globalThis.ms`.
    import(`${VENDOR}/milsymbol/milsymbol.js`),
    stylesheet(`${VENDOR}/maplibre-gl/maplibre-gl.css`),
  ]).then(([maplibre]) => ({ maplibre: maplibre.Map ? maplibre : maplibre.default, ms: globalThis.ms }));

  // A failure is not remembered: the next map opened tries again.
  libraries.catch(() => (libraries = undefined));

  return libraries;
}

const ratio = () => Math.min(globalThis.devicePixelRatio || 1, 2);

// A MIL-STD-2525 symbol for `sidc:<code>[:<direction>[:<fallback>]]`, with the
// point it marks moved to the middle of the image — which is where MapLibre
// anchors an icon, and not where milsymbol leaves it once a direction arrow is
// attached.
//
// `code` is whatever the sender asked for, in whichever edition it wrote it:
// milsymbol reads 2525C letters and 2525D/E numbers alike. `fallback` is the
// letter code the CoT type implies, for a code milsymbol has no drawing for. A
// direction of 0 is none, which is how a fallback can follow it.
function symbolImage(ms, id) {
  const [, code, direction, fallback] = id.split(":");
  const options = { size: 22, ...(Number(direction) > 0 ? { direction: Number(direction) } : {}) };

  let symbol = new ms.Symbol(code, options);
  if (!symbol.isValid() && fallback) {
    symbol = new ms.Symbol(fallback, options);
  }
  if (!symbol.isValid()) {
    // A function nobody has drawn an icon for still has an affiliation and a
    // battle dimension, and the frame alone says both.
    const letters = /^\d/.test(code) ? fallback : code;
    symbol = letters ? new ms.Symbol(letters.slice(0, 4).padEnd(15, "-"), options) : symbol;
  }

  const scale = ratio();
  const drawn = symbol.asCanvas(scale);
  const anchor = symbol.getAnchor();
  const half = {
    x: Math.ceil(Math.max(anchor.x * scale, drawn.width - anchor.x * scale)),
    y: Math.ceil(Math.max(anchor.y * scale, drawn.height - anchor.y * scale)),
  };

  const canvas = Object.assign(document.createElement("canvas"), { width: half.x * 2, height: half.y * 2 });
  const context = canvas.getContext("2d");
  context.drawImage(drawn, half.x - anchor.x * scale, half.y - anchor.y * scale);

  return context.getImageData(0, 0, canvas.width, canvas.height);
}

// A callsign as an image. MapLibre draws text from glyph tiles, which need a
// font server; the browser already has fonts, for every script, offline.
function labelImage(id) {
  const text = id.slice("label:".length);
  const scale = ratio();
  const canvas = document.createElement("canvas");
  const context = canvas.getContext("2d");

  context.font = LABEL_FONT;
  canvas.width = Math.ceil((context.measureText(text).width + 8) * scale);
  canvas.height = Math.ceil(18 * scale);

  context.scale(scale, scale);
  context.font = LABEL_FONT;
  context.textBaseline = "middle";
  context.lineJoin = "round";
  context.lineWidth = 3;
  context.strokeStyle = "rgba(255, 255, 255, 0.95)";
  context.fillStyle = "#1f2d3d";
  context.strokeText(text, 4, 9);
  context.fillText(text, 4, 9);

  return context.getImageData(0, 0, canvas.width, canvas.height);
}

const fade = ["case", ["get", "stale"], 0.45, 1];
const rendered = (kind) => ["==", ["get", "render"], kind];

function addLayers(map) {
  const empty = { type: "FeatureCollection", features: [] };
  map.addSource("shapes", { type: "geojson", data: empty });
  map.addSource("anchors", { type: "geojson", data: empty });

  map.addLayer({
    id: "shape-fills",
    type: "fill",
    source: "shapes",
    filter: ["==", ["geometry-type"], "Polygon"],
    paint: {
      "fill-color": ["coalesce", ["get", "fill"], ["get", "color"]],
      "fill-opacity": ["*", ["coalesce", ["get", "fillOpacity"], 0.15], fade],
    },
  });
  map.addLayer({
    id: "shape-lines",
    type: "line",
    source: "shapes",
    layout: { "line-join": "round" },
    paint: { "line-color": ["get", "color"], "line-width": ["coalesce", ["get", "width"], 2.5], "line-opacity": fade },
  });
  // Where the selected thing has been: the part travelled by the moment
  // shown, the part still to come, and the fixes along both. Under the
  // markers, so that the thing itself is drawn on top of its own past.
  map.addSource("track", { type: "geojson", data: empty });
  const part = (name) => ["==", ["get", "part"], name];
  map.addLayer({
    id: "track-future",
    type: "line",
    source: "track",
    filter: part("future"),
    paint: { "line-color": ["get", "color"], "line-width": 2, "line-opacity": 0.35, "line-dasharray": [2, 2] },
  });
  map.addLayer({
    id: "track-past",
    type: "line",
    source: "track",
    filter: part("past"),
    layout: { "line-join": "round", "line-cap": "round" },
    paint: { "line-color": ["get", "color"], "line-width": 3, "line-opacity": 0.85 },
  });
  map.addLayer({
    id: "track-fixes",
    type: "circle",
    source: "track",
    filter: part("fix"),
    paint: {
      "circle-radius": 2.5,
      "circle-color": ["get", "color"],
      "circle-stroke-color": "#ffffff",
      "circle-stroke-width": 1,
    },
  });
  map.addLayer({
    id: "selected",
    type: "circle",
    source: "anchors",
    filter: ["==", ["get", "uid"], ""],
    paint: {
      "circle-radius": 20,
      "circle-color": "rgba(20, 102, 79, 0.18)",
      "circle-stroke-color": "#14664f",
      "circle-stroke-width": 2,
    },
  });
  map.addLayer({
    id: "dots",
    type: "circle",
    source: "anchors",
    filter: rendered("dot"),
    paint: {
      "circle-radius": 6.5,
      "circle-color": ["get", "color"],
      "circle-stroke-color": "#ffffff",
      "circle-stroke-width": 2,
      "circle-opacity": fade,
      "circle-stroke-opacity": fade,
    },
  });
  map.addLayer({
    id: "symbols",
    type: "symbol",
    source: "anchors",
    filter: rendered("symbol"),
    layout: { "icon-image": ["get", "icon"], "icon-allow-overlap": true, "icon-ignore-placement": true },
    paint: { "icon-opacity": fade },
  });
  map.addLayer({
    id: "labels",
    type: "symbol",
    source: "anchors",
    filter: ["has", "label"],
    layout: {
      "icon-image": ["get", "label"],
      "icon-anchor": "top",
      "icon-offset": ["case", rendered("symbol"), ["literal", [0, 22]], ["literal", [0, 9]]],
      "icon-padding": 1,
    },
    paint: { "icon-opacity": fade },
  });

  // What is being drawn or reshaped, over everything: the line so far, the
  // area it would enclose, and a handle on each vertex.
  map.addSource("sketch", { type: "geojson", data: empty });
  map.addLayer({
    id: "sketch-fill",
    type: "fill",
    source: "sketch",
    filter: part("fill"),
    paint: { "fill-color": SKETCH, "fill-opacity": 0.12 },
  });
  map.addLayer({
    id: "sketch-line",
    type: "line",
    source: "sketch",
    filter: part("line"),
    layout: { "line-join": "round", "line-cap": "round" },
    paint: { "line-color": SKETCH, "line-width": 2.5, "line-dasharray": [2, 1.5] },
  });
  map.addLayer({
    id: "sketch-handles",
    type: "circle",
    source: "sketch",
    filter: part("handle"),
    paint: {
      "circle-radius": 6,
      "circle-color": "#ffffff",
      "circle-stroke-color": SKETCH,
      "circle-stroke-width": 2.5,
    },
  });
}

class MapHandle {
  constructor(maplibre, ms, map, onPick, onSketch) {
    Object.assign(this, { maplibre, ms, map, onPick, onSketch });
    // "draw" while clicks add to a sketch, "edit" while its handles may be
    // dragged, and empty otherwise. See `showSketch`.
    this.sketching = "";
    // The handle being dragged, and what is waiting to be said about the
    // pointer on the next frame.
    this.dragging = null;
    this.report = null;
    this.features = new Map();
    // What is drawn instead of `features` while a moment in the past is
    // shown, or null for the live map. See `freeze`.
    this.frozen = null;
    this.selected = null;
    // Set while this file is itself moving or closing the pop-over. See `quietly`.
    this.quiet = false;
    this.frame = null;
    this.content = Object.assign(document.createElement("div"), { className: "map-popover" });
    this.popup = new maplibre.Popup({ closeOnClick: false, maxWidth: "22rem", offset: 16 });
    this.popup.setDOMContent(this.content);
    // The pop-over's own close button, and nothing else: see `quietly`.
    this.popup.on("close", () => this.quiet || this.pick([], [0, 0]));

    // Symbols and labels are drawn the first time a feature asks for one, and
    // the name says what to draw: there is no sprite sheet to keep in step with
    // what happens to be on the map.
    const add = (id, image) => map.hasImage(id) || map.addImage(id, image, { pixelRatio: ratio() });
    map.setMissingStyleImageResolver((id) => {
      if (id.startsWith("label:")) {
        return add(id, labelImage(id));
      }
      return id.startsWith("sidc:") ? add(id, symbolImage(ms, id)) : undefined;
    });

    // A tool's cursor, which wins over the pointer shown over a feature.
    this.cursor = "";
    map.on("click", (event) => {
      // A click while drawing is a vertex, whatever happens to be under it.
      const drawing = this.sketching === "draw";
      const uids = drawing ? [] : this.hits(event.point);
      this.pick(uids, event.lngLat.toArray(), drawing ? this.handleAt(event.point)?.role : undefined);
    });
    map.on("mousemove", (event) => {
      const over = this.sketching === "edit" && this.handleAt(event.point) ? "move" : "";
      map.getCanvas().style.cursor = this.cursor || over || (this.hits(event.point).length > 0 ? "pointer" : "");

      if (this.sketching === "draw") {
        this.say({ hover: event.lngLat.toArray() });
      }
    });

    // Dragging a handle. Refusing the event's default is what keeps the map
    // from panning under it.
    const grab = (event) => {
      const handle = this.sketching === "edit" && !(event.points?.length > 1) && this.handleAt(event.point);
      if (handle) {
        event.preventDefault();
        this.dragging = { index: handle.index, at: event.lngLat.toArray() };
      }
    };
    const drag = (event) => {
      if (this.dragging) {
        this.dragging.at = event.lngLat.toArray();
        this.say({ drag: { ...this.dragging, done: false } });
      }
    };
    // Let go anywhere, the map or not: a drag that ends over a panel has
    // still ended.
    this.drop = () => {
      if (this.dragging) {
        this.say({ drag: { ...this.dragging, done: true } }, true);
        this.dragging = null;
      }
    };
    map.on("mousedown", grab);
    map.on("touchstart", grab);
    map.on("mousemove", drag);
    map.on("touchmove", drag);
    for (const released of ["mouseup", "touchend", "touchcancel"]) {
      globalThis.addEventListener(released, this.drop);
    }
  }

  // The sketch's handle under a point, as `{ index, role }`, the last one
  // drawn first: finishing is what a second click on it means.
  handleAt({ x, y }) {
    const box = [[x - HANDLE_REACH, y - HANDLE_REACH], [x + HANDLE_REACH, y + HANDLE_REACH]];
    const found = this.map.queryRenderedFeatures(box, { layers: ["sketch-handles"] }).map((handle) => handle.properties);
    return found.find((handle) => handle.role === "last") ?? found[0];
  }

  // Says what the pointer did over a sketch, once a frame at most — or at
  // once, for the last word on a drag, which must not be dropped.
  say(report, now = false) {
    this.report = report;
    const send = () => {
      this.telling = null;
      const said = this.report;
      this.report = null;
      if (said) {
        this.onSketch(JSON.stringify(said));
      }
    };

    if (now) {
      cancelAnimationFrame(this.telling);
      send();
    } else {
      this.telling ??= requestAnimationFrame(send);
    }
  }

  // Every uid under a point, topmost first, with a few pixels' grace for a
  // finger or a thin line. A drawing is under the point once however many of
  // its layers are: its anchor, its outline and its fill are one thing.
  hits({ x, y }) {
    const box = [[x - 5, y - 5], [x + 5, y + 5]];
    const found = this.map.queryRenderedFeatures(box, { layers: HIT_LAYERS });
    return [...new Set(found.map((feature) => feature.properties.uid))];
  }

  // Says what was clicked, and where. None is a click on the bare map; one is
  // a selection; several is a question only the person clicking can answer,
  // and Rust asks it with `choose`.
  pick(uids, at, near) {
    this.onPick(JSON.stringify({ uids, at, near: near === "between" ? undefined : near }));
  }

  // What is on the map right now: the live features, or the moment frozen
  // over them.
  shown() {
    return this.frozen ?? this.features;
  }

  // A feature by uid, from what is shown, or from what is live when what is
  // shown is a moment that it is not part of — the roster names live things.
  lookup(uid) {
    return this.shown().get(uid) ?? this.features.get(uid);
  }

  // `upserts` is a JSON array of `{ uid, anchor, shape }`, the last two being
  // GeoJSON features; `removes` is a JSON array of uids.
  apply(upserts, removes) {
    for (const uid of JSON.parse(removes)) {
      this.features.delete(uid);
    }
    for (const feature of JSON.parse(upserts)) {
      this.features.set(feature.uid, feature);
    }

    // One redraw per frame however many updates arrived in it.
    this.frame ??= requestAnimationFrame(() => this.draw());
  }

  // Draws `upserts` — a JSON array as `apply` takes, a moment in the past —
  // *instead of* the live features, or the live features again for null.
  // The live ones keep arriving underneath either way, so returning to live
  // is a redraw and not a reload.
  freeze(upserts) {
    this.frozen = upserts == null ? null : new Map(JSON.parse(upserts).map((feature) => [feature.uid, feature]));
    this.frame ??= requestAnimationFrame(() => this.draw());
  }

  // `geojson` is a FeatureCollection of the track's parts — lines with a
  // `part` of "past" or "future", and a MultiPoint of "fix" — or null for
  // no track.
  showTrack(geojson) {
    const collection = geojson ? JSON.parse(geojson) : { type: "FeatureCollection", features: [] };
    this.map.getSource("track")?.setData(collection);
  }

  draw() {
    this.frame = null;
    const all = [...this.shown().values()];
    const collection = (features) => ({ type: "FeatureCollection", features });

    this.map.getSource("anchors")?.setData(collection(all.map((feature) => feature.anchor)));
    this.map.getSource("shapes")?.setData(collection(all.flatMap((feature) => feature.shape ?? [])));

    // What a test, or somebody with the inspector open, can read without WebGL.
    this.map.getContainer().dataset.features = String(all.length);
    this.map.getContainer().dataset.shapes = String(all.filter((feature) => feature.shape).length);
  }

  // Draws what is being drawn or reshaped — a FeatureCollection of parts, a
  // "line", a "fill" and a "handle" for each vertex — or takes it away for
  // null. `mode` is "draw" while clicks add to it and "edit" while its
  // handles may be dragged.
  showSketch(geojson, mode) {
    const collection = geojson ? JSON.parse(geojson) : { type: "FeatureCollection", features: [] };
    this.map.getSource("sketch")?.setData(collection);
    this.sketching = geojson ? mode : "";

    // The second click of a double click finishes a drawing; it should not
    // also zoom the map.
    this.map.doubleClickZoom[this.sketching === "draw" ? "disable" : "enable"]();

    const handles = collection.features.filter((feature) => feature.properties.part === "handle");
    this.map.getContainer().dataset.sketch = geojson ? String(handles.length) : "";
  }

  // `cursor` is a CSS cursor name for a tool that is not selection, or empty.
  setCursor(cursor) {
    this.cursor = cursor;
    this.map.getCanvas().style.cursor = cursor;
  }

  // Where the pop-over's content is rendered. Rust portals the chooser into it.
  popoverElement() {
    return this.content;
  }

  // Moves or closes the pop-over without it counting as the reader closing
  // it. MapLibre fires `close` from `remove()`, and `addTo()` on a pop-over
  // that is already open removes it first — so re-anchoring an open pop-over
  // looks, from the event alone, exactly like somebody pressing its ×. Without
  // this, going from one feature to another, or from the chooser to what was
  // chosen, would report a dismissal and close what had just been opened.
  quietly(change) {
    this.quiet = true;
    try {
      change();
    } finally {
      this.quiet = false;
    }
  }

  // Marks a feature as the one in focus, or none. What is said about it is
  // said beside the map, not over it, so the pop-over closes either way.
  select(uid) {
    const feature = this.lookup(uid);
    this.selected = feature ? uid : null;
    this.map.setFilter("selected", ["==", ["get", "uid"], this.selected ?? ""]);
    this.quietly(() => this.popup.remove());
  }

  // Opens the pop-over on a place rather than on a feature, for the chooser.
  // It stays where the click was: the things under it may be moving, and a
  // list that followed one of them would be choosing on the reader's behalf.
  choose(lon, lat) {
    this.selected = null;
    this.map.setFilter("selected", ["==", ["get", "uid"], ""]);
    this.quietly(() => this.popup.setLngLat([lon, lat]).addTo(this.map));
    this.settle();
  }

  // Reveals the pop-over once the map has stopped moving and it has been laid
  // out.
  settle() {
    const settled = () => requestAnimationFrame(() => this.reveal());
    if (this.map.isMoving()) {
      this.map.once("moveend", settled);
    } else {
      settled();
    }
  }

  // Nudges the map so the whole pop-over is inside it. MapLibre picks the side
  // of the point with the most room, which near an edge is still not enough.
  reveal() {
    const box = this.popup.getElement()?.getBoundingClientRect();
    if (!box || !this.popup.isOpen()) {
      return;
    }

    const frame = this.map.getContainer().getBoundingClientRect();
    const beyond = (low, high) => Math.min(0, low - 12) + Math.max(0, high + 12);
    const x = beyond(box.left - frame.left, box.right - frame.right);
    const y = beyond(box.top - frame.top, box.bottom - frame.bottom);

    if (x !== 0 || y !== 0) {
      this.map.panBy([x, y], { duration: 250 });
    }
  }

  flyTo(uid) {
    const feature = this.lookup(uid);
    if (feature) {
      this.map.easeTo({ center: feature.anchor.geometry.coordinates, zoom: Math.max(this.map.getZoom(), 12) });
    }
  }

  fitAll() {
    if (this.shown().size === 0) {
      return;
    }

    const bounds = new this.maplibre.LngLatBounds();
    for (const { anchor } of this.shown().values()) {
      bounds.extend(anchor.geometry.coordinates);
    }
    this.map.fitBounds(bounds, { padding: 64, maxZoom: 14, animate: false });
  }

  destroy() {
    for (const released of ["mouseup", "touchend", "touchcancel"]) {
      globalThis.removeEventListener(released, this.drop);
    }
    cancelAnimationFrame(this.telling);
    cancelAnimationFrame(this.frame);
    this.quietly(() => this.popup.remove());
    this.map.remove();
  }
}

// `options` is JSON: `{ tiles: [url], attribution, maxZoom, center: [lon, lat], zoom }`.
// `onPick` is called with JSON, `{ uids: [uid], at: [lon, lat] }`, for every
// click on the map: the uids of everything under it, topmost first. It is also
// called with no uids when the pop-over is closed. While something is being
// drawn the uids are empty and `near` says whether the click was on the
// sketch's "first" or "last" handle.
// `onSketch` is called with JSON for what the pointer does over a sketch:
// `{ hover: [lon, lat] }` while drawing, and `{ drag: { index, at, done } }`
// while a handle is dragged.
export async function createMap(container, options, onPick, onSketch) {
  const { maplibre, ms } = await load();
  const { tiles, attribution, maxZoom, center, zoom } = JSON.parse(options);

  const map = new maplibre.Map({
    container,
    center,
    zoom,
    maxZoom: 19,
    // Turning the map with two fingers is how a globe works and not how
    // anybody reads a bearing off a TAK map.
    dragRotate: false,
    pitchWithRotate: false,
    style: {
      version: 8,
      sources: { basemap: { type: "raster", tiles, tileSize: 256, maxzoom: maxZoom, attribution } },
      layers: [{ id: "basemap", type: "raster", source: "basemap" }],
    },
  });
  map.touchZoomRotate.disableRotation();
  // Bottom right, clear of the object list over the top-left corner.
  map.addControl(new maplibre.NavigationControl({ showCompass: false }), "bottom-right");
  map.addControl(new maplibre.ScaleControl(), "bottom-left");

  // The style is inline, so this does not wait on the tile server: a map with
  // no route to one still draws everything rustak knows about.
  if (!map.isStyleLoaded()) {
    await new Promise((resolve) => map.once("style.load", resolve));
  }
  addLayers(map);

  return new MapHandle(maplibre, ms, map, onPick, onSketch);
}
