// The map itself: MapLibre GL drawing OpenStreetMap tiles, with milsymbol
// drawing MIL-STD-2525 symbols on top. Everything else — what is on the map,
// what it looks like, what the pop-over says — is decided in Rust
// (`src/pages/map`), which hands this file GeoJSON that is ready to draw.
//
// That split is deliberate. This file knows about pixels and nothing about
// CoT, so the day the map becomes editable it grows a drawing tool (terra-draw
// speaks MapLibre) that hands finished geometry *back* across the same seam,
// and the questions that matter — which channel or mission a new feature is
// published to, and as what — stay with the code that can answer them.
//
// Both libraries are served by rustak from /vendor (see scripts/vendor.mjs)
// and are fetched the first time a map is opened, not with the console.

const VENDOR = "/vendor";

// Layers a click may land on, topmost first.
const HIT_LAYERS = ["symbols", "dots", "labels", "shape-lines", "shape-fills"];

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

// A MIL-STD-2525 symbol for `sidc:<code>[:<direction>]`, with the point it
// marks moved to the middle of the image — which is where MapLibre anchors an
// icon, and not where milsymbol leaves it once a direction arrow is attached.
function symbolImage(ms, id) {
  const [, sidc, direction] = id.split(":");
  const options = { size: 22, ...(direction ? { direction: Number(direction) } : {}) };

  let symbol = new ms.Symbol(sidc, options);
  if (!symbol.isValid()) {
    // A function nobody has drawn an icon for still has an affiliation and a
    // battle dimension, and the frame alone says both.
    symbol = new ms.Symbol(sidc.slice(0, 4).padEnd(15, "-"), options);
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
    paint: { "fill-color": ["get", "color"], "fill-opacity": 0.15 },
  });
  map.addLayer({
    id: "shape-lines",
    type: "line",
    source: "shapes",
    paint: { "line-color": ["get", "color"], "line-width": 2.5, "line-opacity": fade },
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
}

class MapHandle {
  constructor(maplibre, ms, map, onSelect) {
    Object.assign(this, { maplibre, ms, map, onSelect });
    this.features = new Map();
    this.selected = null;
    this.frame = null;
    this.content = Object.assign(document.createElement("div"), { className: "map-popover" });
    this.popup = new maplibre.Popup({ closeOnClick: false, maxWidth: "22rem", offset: 16 });
    this.popup.setDOMContent(this.content);
    // Only the pop-over's own close button reaches here with something still
    // selected; every other way of closing it clears `selected` first.
    this.popup.on("close", () => this.selected !== null && this.onSelect(undefined));

    // Symbols and labels are drawn the first time a feature asks for one, and
    // the name says what to draw: there is no sprite sheet to keep in step with
    // what happens to be on the map.
    map.setMissingStyleImageResolver((id) => {
      const image = id.startsWith("sidc:") ? symbolImage(ms, id) : id.startsWith("label:") ? labelImage(id) : null;
      if (image && !map.hasImage(id)) {
        map.addImage(id, image, { pixelRatio: ratio() });
      }
    });

    map.on("click", (event) => this.onSelect(this.hit(event.point)));
    map.on("mousemove", (event) => {
      map.getCanvas().style.cursor = this.hit(event.point) ? "pointer" : "";
    });
  }

  // The uid under a point, with a few pixels' grace for a finger or a thin line.
  hit({ x, y }) {
    const box = [[x - 5, y - 5], [x + 5, y + 5]];
    return this.map.queryRenderedFeatures(box, { layers: HIT_LAYERS })[0]?.properties.uid;
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

  draw() {
    this.frame = null;
    const all = [...this.features.values()];
    const collection = (features) => ({ type: "FeatureCollection", features });

    this.map.getSource("anchors")?.setData(collection(all.map((feature) => feature.anchor)));
    this.map.getSource("shapes")?.setData(collection(all.flatMap((feature) => feature.shape ?? [])));

    // What a test, or somebody with the inspector open, can read without WebGL.
    this.map.getContainer().dataset.features = String(all.length);

    const selected = this.features.get(this.selected);
    if (selected) {
      this.popup.setLngLat(selected.anchor.geometry.coordinates);
    }
  }

  // Where the pop-over's content is rendered. Rust portals into it.
  popoverElement() {
    return this.content;
  }

  select(uid) {
    const feature = this.features.get(uid);
    this.selected = feature ? uid : null;
    this.map.setFilter("selected", ["==", ["get", "uid"], this.selected ?? ""]);

    if (feature) {
      this.popup.setLngLat(feature.anchor.geometry.coordinates).addTo(this.map);

      // Once the map has stopped moving and the pop-over has been laid out.
      const settled = () => requestAnimationFrame(() => this.reveal());
      if (this.map.isMoving()) {
        this.map.once("moveend", settled);
      } else {
        settled();
      }
    } else {
      this.popup.remove();
    }
  }

  // Nudges the map so the whole pop-over is inside it. MapLibre picks the side
  // of the point with the most room, which near an edge is still not enough.
  reveal() {
    const box = this.popup.getElement()?.getBoundingClientRect();
    if (!box || this.selected === null) {
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
    const feature = this.features.get(uid);
    if (feature) {
      this.map.easeTo({ center: feature.anchor.geometry.coordinates, zoom: Math.max(this.map.getZoom(), 12) });
    }
  }

  fitAll() {
    if (this.features.size === 0) {
      return;
    }

    const bounds = new this.maplibre.LngLatBounds();
    for (const { anchor } of this.features.values()) {
      bounds.extend(anchor.geometry.coordinates);
    }
    this.map.fitBounds(bounds, { padding: 64, maxZoom: 14, animate: false });
  }

  destroy() {
    cancelAnimationFrame(this.frame);
    this.popup.remove();
    this.map.remove();
  }
}

// `options` is JSON: `{ tiles: [url], attribution, maxZoom, center: [lon, lat], zoom }`.
// `onSelect` is called with a uid when something is clicked and with
// `undefined` when nothing was, or the pop-over was closed.
export async function createMap(container, options, onSelect) {
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
  map.addControl(new maplibre.NavigationControl({ showCompass: false }), "top-left");
  map.addControl(new maplibre.ScaleControl(), "bottom-left");

  // The style is inline, so this does not wait on the tile server: a map with
  // no route to one still draws everything rustak knows about.
  if (!map.isStyleLoaded()) {
    await new Promise((resolve) => map.once("style.load", resolve));
  }
  addLayers(map);

  return new MapHandle(maplibre, ms, map, onSelect);
}
