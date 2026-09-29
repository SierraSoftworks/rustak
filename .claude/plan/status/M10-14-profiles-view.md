# M10-14 — The Profiles view: spaced toggles, and a preferences editor built from a schema

Brief: `.claude/plan/briefs/M10-14-profiles-view.md` (main checkout). Rules: `M10-00-wave-rules.md`.

## What changed and why

### 1. Toggle spacing
The fault was in the shared field styling: `.field__control` is a flex column with no gap, so any
field holding more than one control stacked them touching. It now has `gap: 0.6rem`, and a switch
inside it keeps its own width (`align-self: flex-start`) instead of stretching into a full-width click
target. One rule in `styles.scss`, so every such field is fixed at once.

Every stacked group of toggles in the console, checked:

| Where | Container | Before | Now |
|---|---|---|---|
| Profile page → Delivery → "Delivered" (3 switches) | `Field` | touching | fixed by the rule |
| Profiles list → create card → "Delivered" (2 switches) | `Field` | touching | fixed by the rule |
| Control gallery (`pages/demo.rs`, 1 switch) | `Field` | fine | unchanged (one child) |
| Profile page → Channels, package channels | `.channel-chips` (flex, gap) | fine | unchanged |
| Account → profile panel toggles | `.profile-panel__toggles` (flex, gap) | fine | unchanged |
| Channel picker Write/Read, group member rows | row grids with gaps | fine | unchanged |
| EUD list page actions, package filter, mission changes, mission overview (`.stacked-actions`) | page actions / card / flex column with gap | fine | unchanged |
| Service config form "Set" switch | `.config-form__group > .switch` margin | fine | unchanged |

"Add a preference" now sits in its own block under a rule, and the Save / Revert row (with the
list's problem note) is in `.prefs-card__actions`, under another rule with 1.25 rem padding.

### 2. One schema-driven form, used by both pages
`rustak-ui/src/pages/service_config/{form,groups,inputs,schema}.rs` moved unchanged (plain `mv`, no
edits beyond the module path) to `rustak-ui/src/components/schema_form/`, with a new `mod.rs`
exporting `SchemaNode` and `schema`. The Services page (`service_config/mod.rs`, which keeps
`json.rs`), the account's display panel and the new preference editor all import
`crate::components::SchemaNode`. Default id scope, class names and markup are untouched, so the
Services page renders identically; `services.spec.ts` passes untouched.

### 3. The catalogue, as a JSON Schema
New `rustak-api/src/pref_catalog.rs`: the catalogue's one home. `KNOWN` holds 13 typed entries
(key, class, group, title, description, ATAK default, choices, examples), `schema()` renders them as
a JSON Schema (`type`, `title`, `description`, typed `default`, `oneOf` of `const`+`title`,
`examples`, `minimum`/`maximum` for Integer/Long, plus `x-atak-class` and `x-atak-group`), and
`to_json` / `from_json` / `class_of` convert between a stored text value and what a form edits.
It lives in `rustak-api` (wasm-safe) so the console's offline demo fixture serves the very same
schema instead of carrying its own copy (the old fixture had a divergent ten-entry copy).

`GET /api/v1/profiles/pref-catalog` (same path; no route-table change) now answers with that schema
instead of a `[PrefCatalogEntry]` list. `PrefCatalogEntry` is removed from `rustak-api`; the UI was
its only consumer. `rustak-server/src/profiles/catalog.rs` is now the thin server view of it.

**Every entry cites a source** (comment beside it in `pref_catalog.rs`):

| Key | Class | Values / default | Source |
|---|---|---|---|
| `deviceProfileEnableOnConnect` | Boolean | default false | research 07 §2.4; compat/profiles.md §4 |
| `displayServerConnectionWidget` | Boolean | — | research 04 (what OpenTAKServer sends) |
| `prefs_enable_channels` | Boolean | default false | research 07 §5.4; 06 §10.3, 04 |
| `locationCallsign` | String | — | research 06 §10.3; 02 |
| `locationTeam` | String | example Cyan | research 06 §10.3; 02 (`__group name`) |
| `atakRoleType` | String | example Team Member | research 06 §10.3; 02 (`__group role`) |
| `locationReportingStrategy` | String | Dynamic (default) / Constant | research 07 §3.7 |
| `dispatchLocationCotExternal` | Boolean | — | research 07 §3.7, §7.2 |
| `dispatchLocationHidden` | Boolean | — | research 07 §7.2 |
| `appMgmtEnableUpdateServer` | Boolean | — | research 04 (what OTS sends) |
| `atakUpdateServerUrl` | String | example URL | research 04 (what OTS sends) |
| `repoStartupSync` | Boolean | — | research 04 (what OTS sends) |
| `symbologyProvider` | String | 2525C / 2525D | `rustak-server/src/profiles/account.rs` |

**Dropped** from the old 21-key catalogue because nothing in `compat/`, `research/` or the code
cites them: `locationUnitType`, `coord_display_pref`, `alt_display_pref`, `speed_unit_pref`,
`rab_rng_units_pref`, `atakControlOtherUnitsBubble`, `dexControlEnabled`, `atakLongPressMap`,
`constantReportingRateUnreliable`, `dynamicReportingRateStationaryUnreliable`, `expireEverything`,
`enableNonStreamingConnections`, `network_quic_enabled`, and `loctionReportingStrategy` — whose
"ATAK's own misspelling" claim research 07 §3.7 contradicts (`locationReportingStrategy`). Stored
profiles holding any of them keep them; they are now drawn as "other" rows.

**Changed classes:** `deviceProfileEnableOnConnect`, `displayServerConnectionWidget` and
`prefs_enable_channels` were catalogued as String; the research says ATAK reads the first and third
as booleans, and OpenTAKServer sends the first two as Boolean. The catalogue now says Boolean. This
changes nothing a device receives (see 5) — a stored String entry stays a String — but the editor
now shows such an entry as disagreeing (see 4).

### 4. The preferences editor
`rustak-ui/src/components/prefs_editor.rs` became `prefs_editor/{mod,model,rows}.rs`.

- **Known keys** are added from a searchable `TreeSelect` ("Add a preference"), filed by group,
  titled, searchable by key, skipping keys already in the list. A new entry takes the catalogue's
  class and ATAK's default (or the emptiest value of its kind). It is drawn by `SchemaNode`: a select
  for `oneOf`, a switch for a boolean, a number input with bounds for Integer/Long, text otherwise;
  label = title, help = description + "ATAK's own default is …" + "For example: …"; underneath,
  `key · sent as <Class>`. The class is shown, never asked for.
- **Other keys**: "Add another preference", or type a key into the picker's search (offered as "Use
  … as typed" when it has no spaces, is not catalogued and is not already listed). Key, class and
  value are free, in the same row grid as known rows.
- **Disagreement, never silent rewriting** (`model::Catalog::row`): an entry is drawn with its
  schema's control only when the key is catalogued *and* stored as the catalogue's class. Otherwise
  it is an "other" row; if the catalogue names a different class a note says so, with a button
  "Send it as a <Class>" that converts only when clicked (keeping the value when the new class can
  hold it, else the catalogue's starting value). A known row whose value is outside its `oneOf` or
  bounds keeps the value exactly and shows a note as the field's error; nothing is changed until the
  operator changes it.
- Delivery order: entries are edited in place and appended; no reordering existed before and none
  was added.

### 5. What devices receive does not change
The `.pref` renderer and the prefs repository are untouched; the catalogue is never consulted on the
delivery path. New contract test
`the_catalogue_never_changes_the_bytes_a_device_is_sent` stores a catalogued key with its class, a
free key (Integer), a disagreeing class (`prefs_enable_channels` as String) and a disagreeing,
markup-bearing value, and asserts the preview's `.pref` document byte for byte.

## Decisions
- Catalogue data in `rustak-api`, not the server, so that the demo fixture and the server serve the
  same schema (brief: "one home, the UI does not carry its own copy").
- Kept the endpoint path, changed its body to the schema; no route-table edit.
- Required marker hidden inside preference rows (`.pref-row .field__required`), because every row
  has a value and the marker on each said nothing; the shared `SchemaNode` was not given a new prop.
- The per-host keys `prefs_enable_channels_host-<host>` / `…_hierarchy_host-<host>` are documented
  (research 07 §5.4) but are key *patterns*; not catalogued (see Open).

## Files
Added:
- `rustak-api/src/pref_catalog.rs`
- `rustak-ui/src/components/schema_form/mod.rs`
- `rustak-ui/src/components/prefs_editor/model.rs`, `rows.rs`
- `.claude/plan/status/M10-14-profiles-view.md`

Moved (content unchanged): `rustak-ui/src/pages/service_config/{form,groups,inputs,schema}.rs` →
`rustak-ui/src/components/schema_form/`; `rustak-ui/src/components/prefs_editor.rs` →
`rustak-ui/src/components/prefs_editor/mod.rs` (rewritten).

Changed:
- `rustak-api/src/profile.rs` (removed `PrefCatalogEntry` and its test half)
- `rustak-api/src/lib.rs` — **shared**: `pub mod pref_catalog;`, re-export line
- `rustak-server/src/profiles/catalog.rs`, `rustak-server/src/web/api/profiles.rs` (handler body)
- `rustak-server/tests/profiles_contract.rs` (catalogue assertion; new byte test)
- `rustak-ui/src/components/mod.rs` — **shared**: `mod schema_form;`, `pub use schema_form::SchemaNode;`
- `rustak-ui/src/pages/service_config/mod.rs` (module list and import)
- `rustak-ui/src/pages/panels/display.rs` — outside my list: one import line
- `rustak-ui/src/pages/profile_prefs.rs`, `rustak-ui/src/api/profiles.rs`, `rustak-ui/src/fixtures/profiles.rs`
- `rustak-ui/styles.scss` — **shared**: `.field__control` rule; the `.prefs-editor`/`.pref-row` block
  replaced by a self-contained block (plus `.prefs-card__actions` and its own media query)
- `e2e/tests/profiles.spec.ts`, `e2e/tests/small-screens.spec.ts` (one appended test loop)

Not touched: route tables, `web/api/mod.rs`, the API client's module list.

## Tests, and a host ten times slower
- `rustak-api` `pref_catalog` (4 tests), `rustak-ui` `prefs_editor::model` (6), `rows` (1),
  `prefs_editor` (4 kept), the moved `schema_form::schema` (4): pure functions, no clock.
- Contract test: in-process actix app, asserts bytes and statuses only; no time bound.
- e2e `profiles.spec.ts`: picks two catalogued keys through the picker, sets a select and a
  switch, saves, reloads and reads them back; adds a free Integer key, sees the class refusal, fixes
  it, saves, reloads. `small-screens.spec.ts`: at 1280/768/390 px the demo enrolment profile's
  preference card (both row kinds) does not scroll sideways and its controls fit. All waits are
  Playwright's default auto-waits; nothing asserts a duration.

## Checks run
See the final report for exit-check results. UI checks (`rustfmt --check`, clippy on wasm32 with
`-D warnings`, host `cargo test`, `trunk build`) passed. e2e: `profiles`, `services` (untouched),
`preferences`, `small-screens` — 21 passed; `tsc --noEmit` clean.

Looked at in the browser (server started as `e2e/scripts/start-server.mjs` does, demo profile
`/admin/profiles/1?demo`): desktop — spaced "Delivered" toggles, disagreeing rows with the note and
button, "Send it as a Boolean" turning a row into a switch, the picker search and a new "Reporting
strategy" select with its default hint, separated add block and Save row; phone (375 px) — the
free row's fields stack, no sideways scroll.

How the UI was built here: `rustak-ui` cannot be built in place from a worktree nested in the main
checkout (cargo walks up to the main checkout's workspace, which neither includes nor excludes the
nested path). `cargo clippy/test` ran with `--manifest-path` through a symlink outside the tree;
`trunk build` ran in a copy (Cargo.toml, rustak-api, rustak-ui, docs) in the session scratchpad,
and its `dist/` was copied back for the server build. CI is unaffected.

## Follow-up: what a key means is not how it is sent (orchestrator review)

The first pass drew the demo "Enrolment defaults" profile — what this server's own enrolment
profile sends — as three disagreements, because it knew one class per key. But a String holding
`"true"` for a boolean-valued key is a documented, working wire form: TAK Server (research 06 §10.3)
and OpenTAKServer (research 04) send `prefs_enable_channels` that way, and so does
`profiles/prefs.rs::enrollment_defaults` for all three keys.

- **Catalogue** (`rustak-api/src/pref_catalog.rs`): `KnownPref.class` became `reads` (the class ATAK
  reads — the value schema and the control) and `sent_as` (the classes it is known to travel as, the
  first given to new entries). Schema keywords are now `x-atak-reads` and `x-atak-classes`
  (`x-atak-class` is gone); `reads_of` and `classes_of` are new, and `class_of` now means the class a
  new entry gets. `deviceProfileEnableOnConnect`, `displayServerConnectionWidget` and
  `prefs_enable_channels` are `sent_as: [String, Boolean]`, String first because rustak's enrolment
  defaults send String; sources cited per key. A server unit test holds every key the enrolment
  defaults send to the class new entries get.
- **Editor** (`prefs_editor/model.rs`): a known row is a catalogued key stored as any accepted class
  whose value fits what ATAK reads. The control edits the *meaning*; `model::edited` keeps the stored
  class, so toggling a String entry writes `"true"`/`"false"` into a String, and an untouched entry is
  written back exactly as stored. A disagreement note and its button now appear only for a class not
  in `x-atak-classes` ("Send it as a <preferred>") or a boolean-valued key holding something a switch
  cannot show ("Replace it with false", keeping the stored class). An enum or bounds mismatch stays a
  known row with a note, as before. New entries from the picker take the preferred class.
- **Typing a key** (open question 3): an "other" row keeps its key text locally and hands it back
  when the input loses the focus (class and value edits carry the typed key too), so a row no longer
  turns into a known one mid-word. It resyncs when the stored key changes from outside (a row above
  removed, a revert).
- **Tests**: model tests for an accepted-String boolean (known row, toggle, class kept on edit,
  unchanged when untouched), an unaccepted class, a non-boolean in a boolean key, and the preferred
  class for new entries; the contract byte test gained `displayServerConnectionWidget` as a String
  `"false"` and `dispatchLocationHidden` as an unaccepted Integer; `profiles.spec.ts` gained "the
  enrolment defaults are drawn as toggles, not as disagreements" (demo profile 1: three checked
  toggles, four `sent as String`, no note, no "Send it as" button); `small-screens.spec.ts` now uses
  demo profile 2, which still mixes a known and an other row.

Files changed in the follow-up: `rustak-api/src/pref_catalog.rs`,
`rustak-server/src/profiles/catalog.rs`, `rustak-server/tests/profiles_contract.rs`,
`rustak-ui/src/components/prefs_editor/{mod,model,rows}.rs`, `e2e/tests/profiles.spec.ts`,
`e2e/tests/small-screens.spec.ts`, this note. No shared file touched in the follow-up.

The UI was built this time in a copy outside any repository, as the orchestrator suggested.

## Open
- `prefs_enable_channels_host-<host>` and `prefs_enable_channels_hierarchy_host-<host>`: cited
  facts, but pattern keys; they would need `patternProperties` and a picker that asks for the host.
- Resolved by the follow-up: String and Boolean are both accepted for the three boolean-valued keys
  servers are documented sending as Strings; the other boolean keys accept only Boolean, since no
  source shows them sent otherwise.
- Resolved by the follow-up: an "other" row no longer turns into a known row mid-typing.
- ATAK's public documentation was not consulted (no network research done); the catalogue is
  limited to repository sources.
