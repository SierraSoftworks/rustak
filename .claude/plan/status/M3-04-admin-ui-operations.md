# M3-04 — Admin UI: profiles, missions, config packages, settings; packages, clients, CoT — complete

Brief: `.claude/plan/briefs/M3-04-admin-ui-operations.md`
Read first: `conventions.md`; design 04 §8.3; status `M2-07-admin-ui-identity.md`,
`M3-02-device-profiles.md`, `M4-02-missions-sync-notify.md`, `M2-08-identity-api-gaps.md`.

Only `rustak-ui/**`, `e2e/tests/**` and this status file were touched. **Nothing under
`rustak-server/**` or `rustak-api/**` was changed** — every DTO wave A needed already existed.
No `git`/`but` commands were run.

**Both waves are done.** Wave A is described first; wave B — which began when
`M3-03-admin-api-packages-clients-cot.md` appeared — is the section "Wave B" below, and it also
replaced wave A's one feature-detected guess with the real endpoint (see "The TLS card stopped being
a guess").

## What was built

### New API client modules (`rustak-ui/src/api/`)

| File | Covers |
|---|---|
| `download.rs` | Files in both directions: `Download`, `get_download`, `post_download`, `upload`/`upload_many` (multipart), `save` |
| `profiles.rs` | `/profiles` CRUD, `/prefs`, `/files`, `/preview`, `/pref-catalog` |
| `missions.rs` | `/missions`, `/{guid}`, `/changes?squashed=`, `/subscriptions/{uid}/role`, unsubscribe, `?deep=`, `/archive` |
| `certificates.rs` | `GET /certificates[?username=&device_uid=]`, `GET /certificates/{id}`, `POST /certificates/{id}/revoke` |
| `config_packages.rs` | `POST /config-packages` → zip |

`api/settings.rs` gained `tls()` and a locally declared `TlsStatus` shape, feature-detected against
a `404`. Wave B replaced both with the real endpoint and `rustak_api::TlsStatus` — see "The TLS card
is feature-detected" below and "The TLS card stopped being a guess" in the wave B section.

### New components (`rustak-ui/src/components/`)

| File | Functional lines | Contents |
|---|---:|---|
| `prefs_editor.rs` | 218 | `PrefsEditor` (typed rows, class select, catalogue autocomplete and note) and `problem_with`, the three rules the server validates by |
| `xml_view.rs` | 148 | `XmlView` and `tokenise`: a verbatim, coloured XML document emitted as Yew text nodes |
| `file_drop.rs` | 116 | `FileDrop`: a `<label>` wrapping a real `<input type=file>`, with a drop handler on top |
| `role_badge.rs` | 55 | `RoleBadge`, `role_label`, `role_description`, `role_options` |

`components/form/input.rs` gained a `list` prop on `TextInput` (one line plus its doc), so the
preference key field can point at a `<datalist>`.

### New pages (`rustak-ui/src/pages/`)

| File | Functional lines | Contents |
|---|---:|---|
| `profiles.rs` | 285 | The list and the create form |
| `profile_editor.rs` | 66 | `/admin/profiles/{id}`: heading, back link and the three cards |
| `profile_delivery.rs` | 259 | The Delivery card: description, tool, the three flags, channels, Save, Preview, Delete, and `difference` |
| `profile_prefs.rs` | 137 | The Preferences card: draft, Save, Revert |
| `profile_files.rs` | 190 | The Files card: delivered path, drop zone, list, remove |
| `missions.rs` | 172 | The listing, with the filter |
| `mission_detail.rs` | 150 | `/admin/missions/{guid}`: the guid refusal, heading, tabs |
| `mission_overview.rs` | 175 | The Overview tab, the archive download and the delete |
| `mission_tabs.rs` | 168 | Subscribers (role dropdown, remove) and the Layers tree |
| `mission_changes.rs` | 128 | The Changes tab and its squashed toggle |
| `settings_tls.rs` | 173 | The TLS/ACME card (rewritten in wave B against the real DTO, plus Renew now) |
| `panels/certificate.rs` | 122 | `CertificateDetails`: state, fingerprint, expiry, source, reason and revoke |
| `panels/config_package.rs` | 133 | The Configuration package tab on an account |

`pages/load.rs` gained `Downloading` and `use_download`, shared by the profile preview, the mission
archive and the configuration package.

`pages/stubs.rs` lost `Missions` and `Profiles`; `Packages` and `Services` remain (`Packages` is
wave B's).

### Routes

`app.rs` gained `/admin/missions/:guid` (`MissionDetail`) and `/admin/profiles/:id`
(`ProfileEditor`), each with its own heading. The navigation strip is unchanged: both are row
destinations rather than top-level ones, so `navigation.spec.ts` needed nothing.

`user_detail.rs` gained a fifth tab, **Package**.

### Demo fixtures

Three new stores rather than chapters of `fixtures/store.rs`, because nothing in them is reachable
from anything in it — a profile has no user, device or credential in it, so the two would only ever
share a `RefCell`:

- `fixtures/profiles.rs` — three profiles (enrolment, tool-scoped, inactive), their preferences and
  files, the ten-key catalogue, and the eleven mutating operations behind the editor.
- `fixtures/missions.rs` — three missions (ordinary; password-protected, invite-only, read-only by
  default; already deleted), their subscriptions, a nested layer tree, a change log that exercises
  the squash, and a naive `squash` with two unit tests.
- `fixtures/certificates.rs` — four certificates lined up with each fixture device's
  `last_certificate_id` (one expiring within the month, one revoked), the configuration package and
  the TLS status.

`fixtures/mod.rs` gained `empty_zip()`: demo mode has no package builder behind it, so every
download there is a *valid* but empty archive rather than arbitrary bytes — a browser handed one
opens it and finds it empty instead of reporting a corrupt file.

### Styles

`styles.scss` gained a section 5.9 for the six new lists, the layer tree, the certificate strip,
the preference rows, the drop zone, the channel chips and the XML view, reusing the five entity
mixins M2-07 introduced, plus responsive rules that collapse every new row to one column.

## Decisions worth recording

### A download is fetched, then saved

Every one of these endpoints is behind the bearer token the application holds in `sessionStorage`,
and a browser attaches no such header to a navigation — so none of them can be a plain `<a href>`
or a form `action`. `api/download.rs` fetches the body like any other call, then turns it into a
`Blob`, names it with `URL.createObjectURL` and clicks a detached anchor at it, revoking the object
URL immediately afterwards because it pins the whole body in memory for as long as the document
lives.

Two steps rather than one, and deliberately: a browser that has already been told to download
something cannot then be told the request failed. `GET /profiles/{id}/preview` answers `400` with
"that profile has no preferences and no files, so a device would receive nothing", which is worth
reading — so the bytes are fetched first and only saved once there is something to save.
`profiles.spec.ts` asserts both halves.

### The upload never reads the file

`FormData` takes the browser's own `File` object, so a profile file goes from the drop zone to the
socket without its bytes being copied into the wasm heap. That is why `download::upload` takes a
`web_sys::File` rather than a `Vec<u8>`, and why `FileDrop` emits `Vec<web_sys::File>`.

`send_form` is a sibling of `api::send` rather than a branch inside it: `send` sets
`Content-Type: application/json` from the body it serialises, and a multipart request must leave
that header alone so the browser can write its own boundary into it. It repeats the single 401
renewal, which is nine lines and states the same contract.

### The drop zone is a label around a real file input

A drop zone on its own is unreachable from a keyboard and invisible to a screen reader; a file
input on its own ignores the gesture most people reach for first. `FileDrop` is a `<label>`
wrapping `<input type="file">`, so the keyboard, the focus ring and the accessible name come from
the browser and the drop handler is an addition rather than a replacement. The input is
*visually hidden* rather than `display: none`, which would take it out of the focus order.

The input is emptied after every choice, because choosing the same file twice raises no `change`
event and a repeat upload would silently do nothing.

### `XmlView` never reaches the DOM as markup

The document being shown came from a client, which makes it exactly the kind of value that must not
be injected as HTML. `tokenise` splits the text into runs and each is rendered through Yew's
`{value}` interpolation. The unit tests pin the invariant that matters: **the tokens concatenate
back to exactly the input** for a well-formed document, a malformed one, an unterminated tag and an
empty string — a viewer that dropped or reordered a character would be showing an operator
something the client did not send.

It is deliberately not a parser: it never rejects anything, because a malformed document is
precisely the one somebody is trying to read.

### Preferences are replaced, never patched

`PUT /profiles/{id}/prefs` takes the whole list, and the editor sends the whole list. The `.pref`
document renders entries in stored order, so a delta would have to carry a position as well as a
value, and two administrators editing at once would interleave rather than conflict. Sending the
whole list means the second save loses to the first *visibly*.

`problem_with` applies the same three rules the server validates by — blank key, duplicate key,
value the class could not hold — so Save is disabled for exactly the reasons a request would be
refused, and a typo is caught while it is still being typed.

### The profile row is patched with only what moved

The server refuses an update that would do nothing, so sending the whole row would turn "Save" with
nothing changed into an error rather than a no-op — and sending a field that had not changed would
overwrite whatever somebody else did to it in the meantime. `profile_delivery::difference` has four
unit tests, including that *clearing* the channel list is a change (it means "everybody") and that
an empty string where there was nothing is not.

### Deleting a mission does not re-read it

`GET /api/v1/missions/{guid}` answers `410 Gone` for a mission that has been deleted, so reloading
after a delete would replace a page that knows what happened with one that could not say — and
`use_resource` keeps the last good data through a failed reload, so it would have gone on showing
the mission as though nothing had happened. The Overview therefore renders the *outcome of the
call*: the delete sets a local `deleted_at` and the page says so. This was found by
`missions.spec.ts` before it was found by reading.

### The TLS card is feature-detected — and then was not

*Superseded by wave B, and left here because the approach is the point.*

`GET /api/v1/settings/tls` did not exist when wave A was written. `api::settings::tls` treated a
`404` as "this build does not have that" — a fact about the server rather than a failure of the
request — and answered `Ok(None)`, on which `TlsCard` rendered nothing at all: the rest of the
Settings page stayed useful, and the card would appear the day the endpoint did.

`TlsStatus` was therefore declared in `rustak-ui` rather than `rustak-api`, with **every field
optional** and `#[serde(default)]`, because it was a guess at a shape and a guess that refused to
deserialise would turn a working page into a broken one. Three unit tests pinned that: an empty
body, a partial body and a body carrying a field we had not predicted all parsed. Demo mode
answered it, so the card could be reviewed before the endpoint existed.

The endpoint landed eight hours later, which is the argument for having written the card anyway —
and the real DTO is a better one than the guess, so the guess is gone rather than kept beside it.

### The certificate is its own subject on a device row

M2-07 could only say "Certificate #11"; M2-08's endpoints close that. `DevicesPanel` reads
`GET /certificates[?username=]` **once for the whole list** and matches each device by
`last_certificate_id`, falling back to the newest certificate issued to the same uid — one request
rather than an N+1 on a page whose whole job is to be scanned.

Revoking is a separate action from forgetting, and the row now offers both with the difference
stated: forgetting removes what we knew about a client and leaves its certificate working; revoking
refuses it at the next handshake and drops the connections already holding it. The reason is chosen
*before* the confirmation rather than defaulted, because "revoked" on its own does not tell an
administrator six months later whether a device was lost or a certificate simply replaced.
`credential_revoked` and `user_disabled` are not offered: the server sets both itself when the
cascade runs, so choosing one would be describing a cause rather than applying one.

### The configuration package never offers a keystore

`include_client_cert: true` is refused by the server (M3-02 deviation 3) because rustak never holds
a device's private key. A control the server always refuses is worse than none, so the panel does
not offer one and says why in an `Info` alert instead. The credential picker offers only **live
client passwords**, because that is the only kind the request can assert.

### A profile's channels are not a `GroupsPicker`

`GroupsPicker` edits a *membership*, which has a direction and a source behind it. A profile's
channel list is a plain set of names, and offering Write/Read switches for it would be offering a
choice the endpoint has nowhere to put. `profile_delivery::channel_picker` is one switch per
channel, and says "none selected means everybody".

## Deviations from the brief, and why

1. **No `MapSourcePicker`.** Design §8.1 lists a built-in catalogue of map-source XML templates and
   M3-02 explicitly left it out: "there is no verified source for what those templates should
   contain, so it is left for the UI brief to specify". There is no `/api/v1/profiles/map-sources`
   to read one from, and inventing the templates here would be inventing wire content. A map source
   is uploaded as a profile file like any other, which the Files card does.
2. **The certificate work landed as `panels/certificate.rs`, not inside `panels/devices.rs`.**
   Adding it inline took that file past the 300-line limit, and the certificate is a different
   subject with a different action from the device row it sits on.
3. **Three files were split that the brief named as one.** `profile_editor.rs` →
   `profile_delivery` / `profile_prefs` / `profile_files`; `mission_detail.rs` →
   `mission_overview`; `mission_tabs.rs` → `mission_changes`. Each split is along an endpoint
   boundary rather than a line count: the three profile cards are three requests with three failure
   modes, and a single Save over all of them would either half-apply or have to be undone.
4. **The `deleted_at` rendering on a mission row is currently unreachable.** See gap 2.
5. **`XmlView` has no caller in this wave.** It is wave B's (`cot_browser`), so it is exercised by
   the control gallery and by five unit tests, with an `#![allow(dead_code)]` explaining that a
   release build — which does not contain the gallery — would otherwise warn.

## Server endpoint gaps found (`rustak-server/**` was not touched)

1. ~~**No `GET /api/v1/settings/tls`.**~~ **Closed during this brief.** M2-10 added the endpoint
   and `rustak_api::TlsStatus` while M3-03 was being written, so wave B deleted the local guess and
   the feature detection with it. The shape M2-10 shipped is better than the one guessed here: it
   distinguishes `TlsSource` from `TlsCertificateState`, counts consecutive failed orders, and
   carries the authority's own error — which is what makes the "Renew now" button worth having.
2. **`GET /api/v1/missions` cannot list deleted missions.** `web/api/missions.rs::list` passes
   `MissionFilter::default()`, whose `include_deleted` is `false`, and there is no query parameter
   to change it. A mission's row is deliberately kept after deletion so that a client syncing late
   is told it went — but no operator can see that it happened, which is most of the value of
   keeping it. `MissionSummary` already carries `deleted_at` and `missions_view::summary` fills it
   in, so this is one parameter: `?include_deleted=true`. The mission row renders the "Deleted"
   pill today and it is dead until then; `missions.spec.ts` asserts what the server actually does
   and names the gap.
3. **No `GET /api/v1/missions/{guid}` for a deleted mission.** It answers `410`, which is right for
   a client and wrong for an operator asking "what was this and when did it go". A
   `?include_deleted=true` here too would let the detail page open one. See "Deleting a mission does
   not re-read it" above for what the UI does instead.
4. **Nothing creates a mission through the admin API.** `e2e/tests/missions.spec.ts` creates one
   with `PUT /Marti/api/missions/{name}`, which is right — that is the path a TAK client takes — but
   it means an operator cannot make one from the console. Probably correct; recorded because it was
   a real decision rather than an oversight.
5. **`GET /api/v1/missions/{guid}` reports `item_count: 0` on every layer.**
   `web/api/missions.rs::get` hard-codes it (`item_count: 0`) while `MissionLayerSummary` carries
   the field, so the layer tree shows "0 items" for a layer that has some. One join.
6. **`POST /api/v1/config-packages` has no dry run.** The only way to find out whether a package can
   be built for an account is to download one. Not worth an endpoint on its own; noted because the
   panel has to offer the button before it knows.
7. **`ApiError::Gone` carries a wizard-specific message.** `api/mod.rs` maps every `410` to
   `ApiError::Gone`, whose `Display` is "This server has already been set up." — which is the right
   sentence for `/setup/*` and the wrong one for a deleted mission. Not changed here because it is
   shared with M0's wizard; worth a per-call message when a second `410` arrives.

During wave A `./scripts/check-file-length.sh` failed on `rustak-server/src/config/validate.rs`
(313 functional lines), another agent's file; it passes now. Every file in `rustak-ui` is under the
limit (longest: `pages/profiles.rs` 285, `pages/groups.rs` 277 from M2-07, `pages/clients.rs` 273,
`pages/package_row.rs` 263, `pages/demo.rs` 260, `pages/profile_delivery.rs` 259).

## Exit checks

Run on the final tree, with both waves in it.

```
$ cd rustak-ui && trunk build
2026-09-18T19:12:24.942020Z  INFO 🚀 Starting trunk 0.21.14
2026-09-18T19:12:24.942687Z  INFO 📦 starting build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s
2026-09-18T19:12:26.199041Z  INFO applying new distribution
2026-09-18T19:12:26.200409Z  INFO ✅ success

$ cargo clippy --all-targets --target wasm32-unknown-unknown -- -D warnings
    Checking rustak-ui v0.1.0 (…/rustak-ui)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.53s

$ cargo test
running 42 tests
test result: ok. 42 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ cargo fmt --check
(clean, no output)

$ ./scripts/check-file-length.sh
(exit 0)
```

The 42 are: the eight M2-07 left, and 34 this brief added —
`components/{prefs_editor 5, xml_view 5, role_badge 3}`,
`pages/{profile_editor→profile_delivery 4, profile_files 1, missions 2, mission_tabs 1,
mission_changes 1, settings_tls 3, package_row 2, cot_browser 1}`,
`pages/panels/certificate 2`, `fixtures/{missions 2, packages 2}`.

`trunk build --release` and `cargo build --release --target wasm32-unknown-unknown` are clean too,
which matters separately because the demo branches and the control gallery both vanish there — that
is what surfaced `XmlView` as release-only dead code (wave A deviation 5). Release bundle:
`*_bg.wasm` 3 298 163 B, `styles-*.css` 44 050 B, `*.js` 62 298 B (M2-07 recorded 2 095 217 B of
wasm, before this brief's twenty-four pages and four components).

**File lengths.** Every file in `rustak-ui` is under 300 functional lines; the longest are
`pages/profiles.rs` 285, `pages/groups.rs` 277 (M2-07's), `pages/clients.rs` 273,
`pages/package_row.rs` 263, `pages/demo.rs` 260 and `pages/profile_delivery.rs` 259. Three files
were split during wave A and one during wave B to get there, each along an endpoint boundary rather
than at a line count — see wave A deviation 3.

### End to end

Built in the order the launcher requires (`trunk build`, then `cargo build -p rustak-server`; the UI
is embedded by `include_dir!` at compile time). The server compiled first time on every attempt
across both waves — no wait-and-retry was needed, and nothing under `rustak-server/**` was edited.

```
$ cd e2e && npm run typecheck
> tsc --noEmit
(clean)

$ RUSTAK_E2E_CHROMIUM="…/chromium-1234/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing" npx playwright test
Running 29 tests using 1 worker
  ✓   1 [setup] › setup.spec.ts › the first-run wizard turns a token on disk into an administrator who can sign in (1.1s)
  ✓   2 [setup] › setup.spec.ts › the wizard closes itself for good once it has been completed (374ms)
  ✓   3 [chromium] › auth.spec.ts › a browser holding no passkey for this server cannot sign in, and is not told why (515ms)
  ✓   4 [chromium] › auth.spec.ts › a passkey registered for one host is refused at another (595ms)
  ✓   5 [chromium] › auth.spec.ts › an administrator signs in with a passkey, and signing out ends the session (537ms)
  ✓   6 [chromium] › auth.spec.ts › a passkey the browser cannot offer on its own is reached by naming the account (594ms)
  ✓   7 [chromium] › identity.spec.ts › an administrator creates an account, and it opens on its own page (594ms)
  ✓   8 [chromium] › identity.spec.ts › minting an enrolment token shows the secret once, with a scannable QR code (879ms)
  ✓   9 [chromium] › identity.spec.ts › a credential can be revoked, and says so afterwards (909ms)
  ✓  10 [chromium] › identity.spec.ts › a channel is created and granted to an account (1.3s)
  ✓  11 [chromium] › identity.spec.ts › anybody can mint an enrolment token for their own phone (528ms)
  ✓  12 [chromium] › live.spec.ts › a server with nothing connected says so rather than failing (404ms)
  ✓  13 [chromium] › live.spec.ts › the situational-awareness browser is empty rather than broken (405ms)
  ✓  14 [chromium] › live.spec.ts › both live pages are reachable from the navigation strip (578ms)
  ✓  15 [chromium] › missions.spec.ts › a mission a client created is listed, opened, and deleted (873ms)
  ✓  16 [chromium] › missions.spec.ts › an address that is not a mission identifier says so rather than failing to load (352ms)
  ✓  17 [chromium] › navigation.spec.ts › every destination in the navigation strip opens the page it names (1.3s)
  ✓  18 [chromium] › navigation.spec.ts › an account's own page is a deep link, and the strip still says where it is (508ms)
  ✓  19 [chromium] › navigation.spec.ts › a name no account could have says so rather than failing to load (336ms)
  ✓  20 [chromium] › navigation.spec.ts › a deep link into the console is served by the single-page fallback (407ms)
  ✓  21 [chromium] › navigation.spec.ts › an address nothing matches reaches the application's own not-found page (319ms)
  ✓  22 [chromium] › navigation.spec.ts › the landing page gets out of the way of somebody already signed in (405ms)
  ✓  23 [chromium] › packages.spec.ts › a package is uploaded, given a channel, downloaded and deleted (798ms)
  ✓  24 [chromium] › packages.spec.ts › a package upload with no file is not a request at all (428ms)
  ✓  25 [chromium] › profiles.spec.ts › a profile is created, given a preference, and previewed as the package a device receives (900ms)
  ✓  26 [chromium] › profiles.spec.ts › a profile with nothing in it says so rather than handing a device an empty package (807ms)
  ✓  27 [chromium] › smoke.spec.ts › robots.txt is served before the SPA catch-all (5ms)
  ✓  28 [chromium] › smoke.spec.ts › the API reports its own health, and says nothing about the storage behind it (5ms)
  ✓  29 [chromium] › smoke.spec.ts › the application boots and renders (343ms)

  29 passed (21.5s)
```

Four new spec files, nine tests: `profiles.spec.ts` (2), `missions.spec.ts` (2),
`packages.spec.ts` (2), `live.spec.ts` (3). `navigation.spec.ts` needed no change — its existing
test walks the whole strip, so the two new destinations are covered by it.

Five things they found that no unit test would have:

- **"Saved." vanished the moment it appeared.** The preferences card cleared its confirmation in
  the effect that reads the reload — the reload its own save had just asked for. The notice is now
  cleared by the draft diverging instead, which is what actually makes it stale.
- **Deleting a mission left the page showing it as though nothing had happened.** The reload
  answered `410` and `use_resource` keeps the last good data through a failed reload. The Overview
  now renders the outcome of the call — see the decision above.
- **`GET /api/v1/missions` hides deleted missions**, so the "Deleted" pill the row renders is
  currently unreachable. Gap 2.
- **The mission's creator is auto-subscribed as its owner.** That turned the subscribers assertion
  from an empty-state check into a real one: the spec now changes a role through the dropdown,
  reads it back from the server, and unsubscribes the device.
- **A package uploaded with no channel lands in `__ANON__`**, not in nobody's. The success notice
  said the opposite until the test disagreed with it.

## Wave B — packages, live clients, the CoT browser, and the settings that landed with them

Started when `.claude/plan/status/M3-03-admin-api-packages-clients-cot.md` appeared, having polled
for it every two minutes throughout wave A. `rustak-api` and `rustak-server` were re-read first;
everything below is written against what M3-03 actually mounted rather than against design 04.

### New API client modules

| File | Functional lines | Covers |
|---|---:|---|
| `api/packages.rs` | 73 | `PackageFilter`, list, patch, remove, content, multipart create |
| `api/clients.rs` | 27 | list, history, disconnect, set-incognito |
| `api/cot.rs` | 41 | `CotFilter`, list, get, history, forget |
| `api/settings.rs` | 36 | rewritten: `tls`, `renew_tls`, `files`, `set_files`, `marti` |

`api/download.rs`'s `upload` became `upload_many(path, file, fields)` with `upload` as the
one-field case, because a package upload carries four text fields beside the file and a profile
file carries one.

### New pages

| File | Functional lines | Contents |
|---|---:|---|
| `pages/packages.rs` | 202 | The listing, the free-text and mission-package filters, and the upload card |
| `pages/package_row.rs` | 263 | One row and its inline editor: rename, install-on-enrolment, channels, download, delete |
| `pages/clients.rs` | 273 | Connected now with the five-second refresh, incognito and disconnect; and the last day's history |
| `pages/cot_browser.rs` | 215 | The latest message per uid, with the three filters the endpoint takes |
| `pages/cot_drawer.rs` | 137 | One uid: the stored XML, the last hour, and Forget |
| `pages/settings_files.rs` | 139 | The upload ceiling and the read-only Marti settings |

`pages/stubs.rs` is now `Services` alone — the last stub M3 owed.

### Navigation

Two new destinations, **Live** and **Situation**, at the front of the strip: they are the two
pages that answer "what is happening right now", and everything after them answers "how is this
installation set up". `navigation.spec.ts`'s existing test walks the whole strip, so both are
covered by it without a line being added. The strip also now keeps Missions and Profiles selected
while a mission or a profile is open, the way Users already did for an account.

### Decisions worth recording

#### The refresh is a re-armed timeout, not an interval

An interval fires whether or not the last request came back. On a slow link that stacks requests
up, and a page left open would hold *more* connections the slower the server got — which is the
opposite of what anybody wants from a page they opened because something looked wrong. `clients.rs`
arms one `Timeout` after each render that is not busy, so there is never more than one request in
flight and the next one is scheduled from the moment the last finished.

It is switchable off, and says the interval in its own label, because a console left on a wall
display should not be a request every five seconds for a week.

#### A package's editor is on the row

Everything in it is one `PATCH` against the row it sits under, and moving it to a page of its own
would mean carrying the row's identity to a second place for no gain. Each channel toggle is its
own request carrying the **whole** set, because `PackageUpdate::groups` is a replacement — sending
only the one that changed would silently revoke the others.

The delete confirmation says "every row holding these bytes goes", because a hash may back several
rows and "delete this package" and "delete this file everywhere it appears" are different promises.

#### An upload that names no channel is in everybody's

Found in the browser rather than read: the server puts a new package in `__ANON__`. The success
notice says so — "an upload that names no channel lands in the one everybody holds. Narrow it on
the row below if it is not for everybody" — rather than the reverse, which is what the copy said
before the end-to-end test disagreed with it.

#### The CoT drawer shows the document, not a summary of it

A `<detail>` a plugin wrote, a stale time a minute in the past, a callsign with a trailing space:
none of these is visible in any summary, and all of them explain a marker that is missing. So the
drawer is `XmlView` over the stored bytes — which is what wave A's component was written for — with
the last hour listed under it and Forget at the bottom.

`Stale` and `Current` are a pill rather than a date, because "would a client still be drawing
this?" is the question, and it is a comparison against *now* that the reader should not have to do.

#### The TLS card stopped being a guess

Wave A wrote `TlsStatus` in `rustak-ui` and feature-detected the endpoint, because
`GET /api/v1/settings/tls` did not exist. It does now (M2-10 added it while M3-03 was being
written), so the local struct is gone and the card reads `rustak_api::TlsStatus`: `TlsSource`,
`TlsCertificateState`, the ACME directory and challenge, the attempt count and the authority's own
error. The 404 branch went with it — the UI is compiled into the server binary, so there is no
version to skew against.

The card gained **Renew now**, because the useful moment for ordering a certificate is exactly when
the last order failed: the card shows the authority's error, an operator fixes what it named, and
the next scheduled attempt is otherwise hours away. It is offered only for ACME, since nothing else
has anything to renew.

#### The upload ceiling is the only setting on the page that can be changed

And only when `config.toml` does not pin it: a `PUT` while it does is a `409`, so the field is
disabled with the reason stated rather than offering an edit that would be refused. The help text
says the number is advertised to clients *and* enforced on every upload, because that is why it is
a limit rather than a suggestion.

### Deviations from the brief, and why

1. **No `page`/`limit` controls on either listing.** Both endpoints page, and both default to 100
   with a ceiling of 200. The filters are what an operator actually reaches for at these volumes,
   and a paging control that appeared before anybody had 100 packages would be furniture. The
   client modules do not send the parameters; adding them is one line each when a listing gets long
   enough to need it.
2. **`GET /clients/history` is not filtered by `secago` from the UI.** It asks for a day, which is
   what "was it ever here?" means. A window control belongs with the paging above.
3. **The package upload form does not offer channels or a tool.** Both are on the row a moment
   later, and a drop zone with four fields above it is a form somebody has to finish before they
   can drop anything. The API client takes both, so the form is one field away if it turns out to
   be wanted.
4. **`clients.rs` is one file at 273 lines rather than two.** The connected list and the history
   list are the same shape and the same page; splitting them would have separated two components
   that differ by one pill.
5. **No end-to-end test with a real stream connection.** `/clients` with a socket needs an EUD, and
   M3-03 came to the same conclusion for the same reason (its deviation 10). `live.spec.ts` asserts
   what *is* reachable without one — that an installation with nothing connected says so rather
   than erroring, that the refresh switch works, that both pages are in the strip and that the CoT
   filters survive an empty list — and the rows themselves are walked against the fixtures.

### Server endpoint gaps found in wave B

8. **`POST /api/v1/clients/{uid}/incognito` answers with the request, not the client.** So a page
   that wants the resulting row has to re-read the whole list. Answering with the updated
   `ConnectedClient` would make the toggle one request instead of two.
9. **Nothing reports the stream listener's own state.** `GET /clients` answers `[]` both for "the
   listener is running and nobody is connected" and for "there is no listener", and those are
   different things for an operator to be told — the first is quiet, the second is a configuration
   problem. The page says the ambiguous thing because it has to. A field on `GET /settings/marti`,
   or a `/settings/stream`, would close it.
10. **`GET /api/v1/cot` has no time window.** `type`, `callsign` and `group` only, so "what came in
    during the last ten minutes" cannot be asked — which is the question during an exercise. The
    per-uid history takes `secago`/`start`/`end`; the listing taking the same three would be
    consistent.
11. **A package's `expiration` is epoch milliseconds with a negative meaning never.** Right on the
    wire — it is what TAK stores and what every client reads — but it means the UI has to know the
    convention to render or clear one. This page shows expirations and does not offer to change
    them, which is the smaller half of the problem; a later brief adding an expiry editor should
    consider a nullable RFC 3339 field on `PackageUpdate` beside the integer.

### Walkthrough (`trunk serve` on :8081, `?demo`, 1280×900 and 375×812)

Both waves, in one pass at the end.

| Page | What rendered |
|---|---|
| `/admin/profiles?demo` | Create form (name, description, the two delivery switches) over three profiles — "On enrolment · Everybody · 4 preferences · 0 files", "On connect · tool: public · Command", and an inactive one with the `Inactive` pill. |
| `/admin/profiles/2?demo` | Heading "Command · 2 preferences · 1 files · changed …", the Delivery card (description, tool, Active/On enrolment/On connect, four channel chips with Command on), Save / Preview package / Delete; Preferences with `atakRoleType`/`coord_display_pref`, their class selects and the catalogue's own note including ATAK's default; Files with the delivered-path box, the drop zone and `maps/command-overlay.xml`. |
| `/admin/missions?demo` | Filter box and three rows: Operation Kettle with its counts and the `Subscriber` default-role pill; Rescue 12 with `Password` / `Invite only` / `Read-only`; Stand-down dimmed with `Deleted`. |
| `/admin/missions/{guid}?demo` | Overview facts, the `Also delete…` switch above Download archive / Delete. **Subscribers**: three rows with Connected/Offline, a role dropdown each and Remove. **Changes**: five entries with Created/Added/Removed pills, callsigns, CoT types and coordinates; flipping "Squashed" re-fetched and collapsed them to three, the removed-then-re-added marker appearing once. **Layers**: Markers (4 items) with North sector (2 items) nested under it, and Attachments. |
| `/admin/packages?demo` | The upload card (name, keywords, drop zone) over three rows with size, MIME type, submitter, channels, mission and keywords; `Mission package` and `On enrolment` pills where they apply. Edit opened the row's own editor: a rename box, the enrolment switch and four channel switches with Command on. |
| `/admin/clients?demo` | "Refresh every 5 seconds" on by default; two connected rows with team, role, TAKV, protocol, `ip:port`, connected and last-event times and the out/in channel phrase, each with an Incognito switch (one on) and Disconnect. Under it, the last day's history with the two connected marked `Connected` and the two that are not marked `Offline`. |
| `/admin/cot?demo` | The three filters, then three rows — QUINN, RAO and a CONTACT 1 marker — each with its CoT type, team, coordinates, sent and received times, channels, and a Current/Stale pill. Selecting one tinted the row and opened the drawer: the stored event coloured by `XmlView` (comment, escaped `&amp;` and all), the received/stale line, three earlier positions, and Forget. |
| `/admin/devices?demo` | Each row carries its certificate: `Active` pill, fingerprint, "Expires in 343d", source, a reason dropdown and Revoke. The revoked service certificate shows `Revoked` and offers neither. |
| `/admin/users/avery?demo` → Package | Client picker (ATAK and WinTAK / iTAK), the client-password picker, the "No keystore travels in this package" note and Download package. |
| `/admin/settings?demo` | **Transport security**: the red "3 orders in a row have failed" alert with the authority's own message, the `Last order failed` pill, host names, validity, renewal, directory, challenge and last attempt, and **Renew now** — which turned the card green, cleared the error and moved the validity window to a fresh 89 days. **Enterprise Sync**: the upload limit (400 MB, editable because the fixture does not pin it), the public host and the cross-origin answer. |
| `/demo/controls?demo` | The new components: the drop zone in both states, the three role badges, and a CoT event in `XmlView` — 115 spans across all six token classes. |
| 375×812 | Every new row collapses to one column. Four fixes came out of these passes: the mission delete switch and its button group are both `inline-flex` and shared a line, with the label running under the buttons (now a `stacked-actions` column); the device row had three grid columns for four things, so "Forget" wrapped below (now four, the certificate having its own); `.prefs-editor` stretched "Add a preference" into a bar across the card (now `align-items: flex-start`, with the list opting back in); and the CoT fixtures were all stale at once, which hid the distinction the pill exists to draw (the fixture's stale time is now ten minutes after its send time, so recent reports are current and older ones are not). |

## Files

**New (35):**
`rustak-ui/src/api/{certificates,clients,config_packages,cot,download,missions,packages,profiles}.rs`;
`rustak-ui/src/components/{file_drop,prefs_editor,role_badge,xml_view}.rs`;
`rustak-ui/src/fixtures/{certificates,missions,packages,profiles}.rs`;
`rustak-ui/src/pages/{clients,cot_browser,cot_drawer,mission_changes,mission_detail,mission_overview,mission_tabs,missions,package_row,packages,profile_delivery,profile_editor,profile_files,profile_prefs,profiles,settings_files,settings_tls}.rs`;
`rustak-ui/src/pages/panels/{certificate,config_package}.rs`;
`e2e/tests/{live,missions,packages,profiles}.spec.ts`; this file.

**Changed (14):** `rustak-ui/Cargo.toml` (web-sys features for files and downloads; no new crates,
so `Cargo.lock` is unchanged); `rustak-ui/styles.scss`; `rustak-ui/src/app.rs`;
`rustak-ui/src/api/{mod,settings}.rs`;
`rustak-ui/src/components/{admin_shell,mod,status_pill}.rs` (the two new destinations, and
`StatusTone: Debug`); `rustak-ui/src/components/form/input.rs` (`TextInput`'s `list`);
`rustak-ui/src/fixtures/mod.rs`;
`rustak-ui/src/pages/{demo,load,mod,settings,stubs,user_detail}.rs`;
`rustak-ui/src/pages/panels/{mod,devices}.rs`.

**Not changed:** `rustak-api/**`, `rustak-server/**`, every other crate.
