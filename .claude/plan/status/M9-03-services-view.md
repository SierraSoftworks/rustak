# M9-03 — A working Services page in the admin console — complete

Brief: `.claude/plan/briefs/M9-03-services-view.md`
Read first: `conventions.md`; `rustak-api/src/service.rs`; `rustak-server/src/web/api/services.rs`,
`plugins/{auth,registry,health}.rs`; `rustak-ui/src/pages/{euds,cot_browser,cot_drawer,dashboard,stubs,load}.rs`;
`rustak-ui/src/{api/mod.rs,fixtures/{mod,missions}.rs,components/*}`; status M7-04 (UI unit tests, `?demo` flags,
the stale-bundle trap); `e2e/tests/{navigation,settings,helpers}.ts`; `docs/plugins.md` → "Registering with the server".

The `AddOns` stub is gone. **Settings → Services** (still at `/admin/settings/add-ons`) is a real page: a
sorted list, a detail drawer with the heartbeat's `metrics` as a table, a Configuration editor and a Remove
action, with demo fixtures, unit tests and four Playwright specs behind it.

No `git`/`but` write command was run. Nothing under `rustak-plugin-ais/**`, `rustak-plugin-adsb/**`, the
workspace `Cargo.toml`, `.claude/plan/{plan,backlog}.md` or CI files was touched.

## 1. What landed

**The list (`rustak-ui/src/pages/services.rs`).** One row per registration: display name over `name`,
version, last heartbeat and registered-at as relative times with the absolute on `title`, capabilities as
tags, and a state pill. `ordered()` sorts what `ServiceState::needs_attention()` flags first and worst
first inside that — `ServiceState::ALL` is "worst last", so its index is the severity, except that
*Unknown* ("Not reporting") sits at the head of that list while still needing attention, which is why
attention is the first sort key rather than severity alone. An attention row also carries a rule down its
left edge, so the state never depends on colour alone. Empty state names how a sidecar registers and links
to `docs/plugins.md#registering-with-the-server`.

*Refresh.* A re-armed `Timeout` rather than an interval (the pattern `euds.rs` uses, for the same reason: a
timer that fires mid-request stacks requests on a slow link), plus a tick counter so the timer re-arms even
when the request was skipped. `document.hidden` is what makes "while the page is visible" true rather than
aspirational — a console left open on another tab keeps the timer and skips the fetch. Manual refresh is
`use_refresh_action`, which puts the button in the shell's shared title row like every other list page.

**The drawer (`pages/service_detail.rs`).** Under the list, the way `cot_drawer.rs` sits under the
situation browser. Everything the row has plus the reported endpoints, the metrics table, and a Remove
behind `ConfirmButton` whose question says what survives (the account, the certificate, the channels) and
that a running sidecar re-registers. **Keyed on the service name** so choosing a different row *remounts*
it: `use_resource` fetches on mount and on reload, so without the key the configuration panel would still
be showing the previous service's document. (The same latent trap exists in `cot_drawer`; not this brief's
to change, noted below.)

**The metrics renderer (`pages/service_metrics.rs`).** `read_metrics` flattens a heartbeat's `metrics` into
`MetricRow { key, value, depth, group }`, or answers `Raw(text)` for a value that is not an object, or
`None` for null/`{}`. Flat keys become rows, a nested object becomes a heading with its fields indented one
level, an array becomes its elements comma-joined, and an object inside an array is the one place compact
JSON is the honest answer. Every value reaches the DOM through Yew's `{value}`; nothing on the path builds
markup, and a string that looks like a tag stays that string.

**Configuration (`pages/service_config.rs`).** `GET`s `/services/{name}/config`, shows it pretty-printed in
a monospaced textarea, refuses what the server would refuse *before* the request (must be JSON, must be an
object), and disables Save until something changed. The "changed since load" guard is a **re-read before
the write**: `PUT` is a replacement and this page polls, so a save first fetches the stored document and
refuses when it is not the one this panel loaded — the alternative is a silent overwrite of somebody
else's settings, which is the one failure here nobody would notice. On success the panel reloads from the
response and says the service picks it up on its next tick.

**Dashboard tile.** A fourth card in the existing `.dashboard` grid: registered count, how many need
attention, and a link to the page.

**Demo fixtures (`fixtures/services.rs`).** Its own `thread_local` store, like `fixtures/missions.rs`.
Three services — healthy `rustak-plugin-ais` with `offered`/`published`/`suppressed`/`expired`/`tracked`
and `source { kind: "aisstream", state: "connected" }`; degraded `rustak-plugin-adsb` with a message and
`source.state: "reconnecting"`; unhealthy `rustak-plugin-example` whose last heartbeat is 47 minutes old —
plus a configuration for the AIS feed, and mutations for save and remove.

**Route and navigation.** `Route::AddOns` → `Route::Services`, heading "Services", sidebar label
"Services". **The path is unchanged** (`/admin/settings/add-ons`): the page was called Add-ons until the
sidecars it lists existed, and a path that changes breaks every bookmark of it. `pages/stubs.rs` is
deleted — it held only this one stub.

## 2. The server needed no change

`GET /api/v1/services/{name}/config` already answers an administrator: `owned()` gates on
`Caller::owns`, which is `is_admin() || user_id == service.user_id`, and `plugins::auth::caller` falls
through from the service-token check to `bearer`, so the console's own RS256 access token resolves. The
brief's "fix the authorization if it refuses administrators today" was therefore not needed, and **nothing
in `rustak-server/src/plugins/**` or `rustak-api/src/service.rs` was modified**.

One test was added to `rustak-server/src/web/api/services.rs` (additive, tests only) so that the console's
dependency on that read is written down rather than incidental:
`an_administrator_reads_a_services_configuration_and_nobody_else_does` — the administrator `GET`s back what
they `PUT`, and a signed-in account that neither administers nor owns the registration gets `404` (not
`403`, which would be an existence oracle, R-01 M16).

## 3. Tests

| Where | What |
|---|---|
| `pages/service_metrics.rs` | flat object → one row per key; nested → heading + indented fields; array → comma-joined, empty → em dash, object-in-array → compact JSON; non-object → `Raw`; a string that looks like markup stays that string, at both nesting levels |
| `pages/services.rs` | attention sorts first and worst first, with *Not reporting* above healthy; no state that needs attention is ever drawn with the `Ok` tone |
| `pages/service_config.rs` | only a JSON object is a configuration (blank, malformed, array, bare number all refused); the editor shows a pretty-printed document |
| `src/util.rs` | `short_duration` at every unit boundary and with a negative; `short_relative` past/future/now; `optional_relative`'s em dash |
| `fixtures/services.rs` | the three fixtures cover the three states and two need attention; the feed metrics carry a nested object; configuration is stored, read back, and removed with its service |
| `rustak-server/src/web/api/services.rs` | the admin config read above |
| `e2e/tests/services.spec.ts` | ordering + pills + attention rule + the row's message and tags; the detail's endpoints and metrics table (including "no braces reach the page"); edit → refuse an array → save → "Saved."; remove → the row and the drawer go |
| `e2e/tests/navigation.spec.ts` | the sidebar line is now `["Services", "Services"]` |

`rustak-ui`: **70 tests** (51 at M7-04, 55 before this brief, +15 here).

## 4. Exit checks

Run on 2026-09-20.

```
$ cd rustak-ui && cargo fmt --check
(clean)

$ cargo clippy --all-targets --target wasm32-unknown-unknown -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.20s

$ cargo test
test result: ok. 70 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ trunk build
2026-09-20T14:52:20.750898Z  INFO ✅ success

$ cargo test -p rustak-server
test result: ok. 1843 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 38.10s
(+ 29 integration suites, all ok; 0 failed anywhere)

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.90s

$ ./scripts/check-file-length.sh
(clean)

$ cd e2e && npm run typecheck
> tsc --noEmit
(clean)

$ RUSTAK_E2E_CHROMIUM="…/chromium-1234/chrome-mac-arm64/…/Google Chrome for Testing" npx playwright test
  ✓  32 [chromium] › tests/services.spec.ts:31:1 › the list puts what needs attention above what does not (404ms)
  ✓  33 [chromium] › tests/services.spec.ts:62:1 › a service's detail shows its endpoints and its metrics as a table (582ms)
  ✓  34 [chromium] › tests/services.spec.ts:89:1 › an administrator edits a service's configuration and is told it was saved (494ms)
  ✓  35 [chromium] › tests/services.spec.ts:116:1 › removing a registration takes its row off the list (462ms)
  40 passed (27.1s)
```

`cargo fmt --check` at the workspace root is **not** clean, and none of it is this brief's:
`rustak-plugin-ais/src/sources/udp.rs` and `rustak-server/tests/feed_sidecars.rs`, both M9-01/M9-02's
in-flight files. `rustak-server/src/web/api/services.rs` — the only server file touched here — is clean.
`trunk build` then `cargo build -p rustak-server` was run before **both** Playwright runs, because the
server embeds `rustak-ui/dist` at compile time (M7-04's stale-bundle trap).

The page was also read in a real browser against `trunk serve` + `?demo` at 1280×900: the list, the
ordering, the attention rule, the endpoints, the nested metrics table, the configuration editor and the
dashboard tile all render as intended.

## 5. Notes for whoever is next

- **`cot_drawer` has the latent staleness this brief's drawer avoids.** `use_resource` re-fetches on mount
  and on `reload` only, so a component that takes an identifier as a prop keeps the previous
  identifier's data when the prop changes. `ServiceDetail` is keyed to force a remount;
  `Situation`'s `<CotDrawer uid=… />` is not, and switching directly between two rows there would show the
  first row's document until something reloads. Out of scope here (`pages/cot_browser.rs` is not this
  brief's file) — worth a `key` or a `use_effect_with(props.uid)`-driven reload.
- **`metrics` renders in key order, not report order.** `serde_json`'s default `Map` is a `BTreeMap`, so a
  plugin cannot choose the row order. `docs/plugins.md` now says so. Turning on `preserve_order` in
  `rustak-api` and `rustak-ui` would change that, at the cost of a feature flag two crates have to agree on.
- **No SSE.** The brief said none, and `GET /services` is one cheap read; if a fleet ever gets large enough
  for a ten-second poll to matter, `/api/v1/events` already exists and the page would subscribe rather than
  poll.
- **`docs/plugins.md`** gained a "Monitoring a sidecar" subsection (what the page shows, and a table of how
  each shape of `metrics` renders), the `metrics` sentence is now accurate rather than aspirational, and
  the per-service configuration section says that an administrator may read as well as write. `README.md`
  needed no change — its highlights do not enumerate the console's pages.

## Files changed

| File | What |
|---|---|
| `rustak-ui/src/pages/services.rs` | new: the list, the ordering, the tone map, the visibility-aware poll |
| `rustak-ui/src/pages/service_detail.rs` | new: the drawer, the endpoints, Remove |
| `rustak-ui/src/pages/service_metrics.rs` | new: `read_metrics`, `MetricsTable` |
| `rustak-ui/src/pages/service_config.rs` | new: the Configuration panel and its validation |
| `rustak-ui/src/api/services.rs` | new: `list`, `config`, `set_config`, `remove` |
| `rustak-ui/src/fixtures/services.rs` | new: three services, one configuration, save/remove |
| `rustak-ui/src/pages/stubs.rs` | deleted (it held only `AddOns`) |
| `rustak-ui/src/{app,components/admin_shell}.rs` | `Route::AddOns` → `Route::Services`; heading and sidebar label; path unchanged |
| `rustak-ui/src/pages/{mod,dashboard}.rs` | module list; the Services tile |
| `rustak-ui/src/{api/mod,fixtures/mod}.rs` | one module line each |
| `rustak-ui/src/util.rs` | tests for the relative-time helpers |
| `rustak-ui/styles.scss` | section 5.12: `.tag`, `.service-list`, `.service-row`, `.service-detail`, `.metrics`, `.panel-note--ok`, and `.service-row` in the narrow-screen collapse |
| `rustak-server/src/web/api/services.rs` | one test (no handler or authorization change) |
| `e2e/tests/services.spec.ts` | new: four specs against `?demo` |
| `e2e/tests/navigation.spec.ts` | the sidebar label, and the header comment about the last stub |
| `docs/plugins.md` | "Monitoring a sidecar"; the `metrics` sentence; admin reads as well as writes |
