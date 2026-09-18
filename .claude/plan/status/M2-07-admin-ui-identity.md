# M2-07 — Admin UI for identities, devices, credentials (QR enrolment) and channels — complete

Brief: `.claude/plan/briefs/M2-07-admin-ui-identity.md`
Read first: `conventions.md`; status `M0-13-ui-skeleton.md`, `M0-18-wizard-passkey-fixes.md`,
`M2-02-identity-credentials.md`, `M2-03-enrollment-endpoints.md`; `design/04` §8.3;
`compat/enrollment.md` §5 (the `tak://` QR contract).

Only `rustak-ui/**`, `e2e/tests/**` and this status file were touched. **`rustak-api` was not
changed** — every DTO the brief asked for already existed. No `git`/`but` commands were run.

## What was built

### New API client modules (`rustak-ui/src/api/`)

| File | Covers |
|---|---|
| `credentials.rs` | `GET`/`POST /credentials`, `DELETE /credentials/{id}`, `GET /credentials/{id}/enroll-url` |
| `devices.rs` | `GET /devices[?username=]`, `DELETE /devices/{uid}`, `PUT /devices/{uid}/active-groups` |
| `groups.rs` | `GET`/`POST /groups`, `PATCH`/`DELETE /groups/{name}`, `GET`/`PUT /users/{u}/groups` |

`api/mod.rs` gained `Verb::Put`, `put_json` and `delete_empty` (it had neither; the M0 pages only
ever read and patched). `api/users.rs` gained `create` and a `get` that filters the listing — see
gap 2 below.

### New components (`rustak-ui/src/components/`)

| File | Functional lines | Contents |
|---|---:|---|
| `qr_code.rs` | 61 | `QrCodeView`: `qrcode` → module matrix → one SVG `<path>` of merged horizontal runs, emitted as Yew nodes |
| `secret_reveal.rs` | 90 | `SecretReveal` (show-once panel, explicit dismiss) and `Copyable` (value + copy button + result note) |
| `confirm.rs` | 72 | `ConfirmButton`: the question replaces the button in place, so the answer stays beside the row it is about |
| `groups_picker.rs` | 105 | `GroupsPicker`: every channel with Write/Read switches, folding the server's per-direction rows into one row per channel |

### New pages (`rustak-ui/src/pages/`)

| File | Functional lines | Contents |
|---|---:|---|
| `user_create.rs` | 156 | The "Add an account" form on the Users page |
| `user_detail.rs` | 161 | `/admin/users/{username}`: heading, four tabs, and the "not a username" refusal |
| `groups.rs` | 277 | Channels: create, edit description, delete with confirm, select-to-see-members |
| `group_members.rs` | 183 | One channel's members, composed from `/users` + one `/users/{u}/groups` per account |
| `devices.rs` | 3 | The whole-installation device list |
| `me.rs` | 30 | The signed-in account's own credentials and devices |
| `panels/credentials.rs` | 205 | The credential list, the show-once reveal and the revoke action |
| `panels/mint.rs` | 148 | The mint form, with the client-password compatibility warning |
| `panels/devices.rs` | 183 | The device list, filter and the forget action |
| `panels/channels.rs` | 111 | One account's channel memberships, `PUT` as a whole set |
| `panels/profile.rs` | 176 | Display name, administrator and enabled, with the self-lockout guards |

`pages/panels/` exists because the same three panels appear in three places — an administrator's
view of somebody else, a person's view of their own, and the whole-installation lists — and every
endpoint behind them takes the same `username` argument. One component with one prop keeps "enrol my
phone" and "enrol somebody's phone" the same flow rather than two that drift.

`pages/stubs.rs` lost `Devices`, `Credentials` and `Groups`; `Missions`, `Packages`, `Profiles` and
`Services` remain.

### Demo fixtures

`fixtures/data.rs` gained four channels, four devices, five credentials (one revoked, one spent, one
expiring, one non-expiring service token) and a membership table; `fixtures/store.rs` gained the
fourteen mutating operations behind them, including a `mint_credential` that builds a real
`tak://` URL from a secret that names itself as fake. Every new page renders under `?demo`, and each
was walked in a browser (below).

### Styles

`styles.scss` grew a section 5 block for the four entity lists, the tabs, the inline forms, the
show-once panel, the copyable values and the QR frame, plus responsive rules. The four lists share
five mixins (`entity-list`, `entity-row`, `entity-meta`, `entity-identity`, `entity-error`) rather
than four copies of the same rule, so a device row and a credential row read the same way.

## Decisions worth recording

### `/admin/credentials` is the *self-service* page

`GET /api/v1/credentials` with no `username` answers with the **caller's own**, and there is no
installation-wide listing (gap 8). A "Credentials" page that showed everybody's therefore cannot
exist. Rather than add a twelfth navigation destination, the existing one now renders `pages/me.rs`
— your own credentials, your own devices, and the mint flow — which is exactly what the brief's
item 2 asked for ("so a non-admin can enrol their own phone") and needs no administrative access at
all. An administrator reaches somebody *else's* through `/admin/users/{username}` → Credentials.

### The QR code is drawn, not rendered

`qrcode`'s own `render::svg` produces an SVG document as a **string**, which would have to reach the
DOM through `dangerously_set_inner_html`. `components/qr_code.rs` walks `to_colors()` instead and
emits one `<path>` whose `d` is an attribute value — so the whole component stays inside Yew's
escaping, and there is no route from an encoded string to the page as markup. Horizontal runs are
merged, so a 41×41 code is one DOM node rather than several hundred rectangles. The crate is pinned
`qrcode = { version = "0.14", default-features = false }`: the `image`, `svg` and `pic` renderers
are all off, which leaves the crate with **no dependencies at all** on `wasm32-unknown-unknown`.

### The copy button has a deadline

Found in the browser: a Chromium with `clipboard-write` **denied** does not reject
`navigator.clipboard.writeText` — the promise simply never settles. Without a deadline the button
span silently for ever while somebody held a one-time secret. `util::copy_to_clipboard` now races the
write against a 3-second `TimeoutFuture` and reports the failure, and every `Copyable` shows the
value beside the button so it can be taken by hand either way. Verified in a pane where the
permission is denied: the message appears after three seconds.

### `PUT` is a replacement, so the picker sends the whole set

Both `PUT /users/{u}/groups` and the channel-member toggles send everything the account should hold,
never a delta. `groups_picker::unfold` and `group_members::rewrite` both carry every *other* channel
along — dropping one would silently revoke it — and both drop the memberships whose source is
`Oidc`, because the server refuses those and accepting one would be accepting a change the member's
next sign-in undoes. Three unit tests each.

### Card titles differ from page titles

`pages/devices.rs` passes `title="Enrolled devices"` and `pages/me.rs` passes `"Your credentials"` /
`"Your devices"`, because the shell already shows "Devices" and "Credentials" as the page heading
and two identical headings is both a duplicate for a screen reader and a strict-mode failure for
`navigation.spec.ts`'s `getByRole("heading")`.

## Deviations from the brief, and why

1. **No channel rename.** The brief asks for "list/create/rename/delete". `GroupPatch` carries only
   the description and `web/api/groups.rs` says why in its module docs: every membership, every
   `groups` claim and every client's cached selection refers to a channel by name, so renaming one
   is deleting it and making another. The form says so under the name field rather than offering a
   control the server would refuse.
2. **Email is read-only on the Profile tab.** The brief asks to patch "display name/email/admin/
   enabled". `UserPatch` has no `email` (gap 3). Adding the field to the DTO without the handler
   behind it would be a form that silently discards what somebody typed, so it is shown in the facts
   list and not offered as a field.
3. **The device action is "Forget", not "Revoke".** `DELETE /api/v1/devices/{uid}` removes the row;
   the certificate belongs to the *account* and is taken back by revoking the credential it was
   issued against. The confirm text says exactly that, rather than implying a revocation the call
   does not perform.
4. **No per-device "active channel" toggles.** `PUT /devices/{uid}/active-groups` exists and the
   client module wraps it, but there is no endpoint that *reads* a device's current active set
   (gap 6) — so the UI would have to write in order to find out what it was. The client function is
   present and `#[allow(dead_code)]`, ready for the endpoint.
5. **The Channels tab is Write/Read, not "IN/OUT/active".** "Active" is a per-*device* preference
   rather than a per-account right; there is no such thing on `/users/{u}/groups`.
6. **The members table costs N+1 requests.** There is no `GET /groups/{name}/members` (gap 5), so
   `group_members.rs` reads the account list and then one membership set per account, concurrently
   through `futures::future::join_all`. Fine for the installations M2 targets; worth an endpoint
   before it is not.
7. **No new navigation destination.** See "`/admin/credentials` is the self-service page" above. The
   brief's item 6 says to update `navigation.spec.ts` "for the new destinations"; the strip is
   unchanged, so the spec gained the account page as a **deep link** test instead, plus a test that
   a name no account could have (`__anon__`) says so rather than failing to load.
8. **`api::credentials::enroll_template` is unused.** `POST /credentials` already returns the whole
   `enroll_url`, so the template endpoint has nothing to add while the page still holds the mint
   response. It is written and `#[allow(dead_code)]` so that the one place the `tak://` shape is
   consumed stays in one file.
9. **The Playwright switch test clicks the label, not the checkbox.** `.switch__input` is a
   one-pixel transparent element behind the drawn track, so `locator.check()` is refused by the
   actionability check. The test clicks the switch's own text inside the `<label>`, which is what a
   person does and what the browser forwards, then asserts `toBeChecked()`.

## Server endpoint gaps found (for a follow-up brief — `rustak-server/**` was not touched)

1. **No certificate endpoint.** `rustak_api::Certificate` exists with `fingerprint`, `not_before`,
   `not_after` and `revoked_at`, and `Device::last_certificate_id` points at a row — but there is no
   `GET /api/v1/certificates` or `GET /api/v1/certificates/{id}`, and no way to revoke one directly.
   The brief's "certificate fingerprint/expiry, revoke" on the Devices tab is therefore reduced to
   "Certificate #11" / "No certificate". **This is the biggest gap and the one worth closing first.**
   Suggested: `GET /certificates?username=&kind=`, `GET /certificates/{id}`,
   `DELETE /certificates/{id}` (revoke), reusing `identity::credentials::revoke`'s cascade.
2. **No `GET /api/v1/users/{username}`.** `web/api/users.rs` has `list`, `create` and `patch` only,
   so a page that opens on one account reads the whole listing and filters it client-side
   (`api::users::get`). A single-account read would also let the detail page distinguish "no such
   account" (404) from "you may not see it" (403).
3. **`UserPatch` has no `email`.** `display_name`, `is_admin` and `disabled` only — see deviation 2.
4. **No `GET /api/v1/groups/{name}/members`.** See deviation 6. A member listing endpoint would turn
   N+1 requests into one and would let the page show a member count on each channel row.
5. **No read for `active-groups`.** `PUT /devices/{uid}/active-groups` answers with the resulting
   state, but `GET /devices/{uid}` returns a `Device` with nothing about channels, so the state
   cannot be displayed without writing first. Suggested: `GET /devices/{uid}/active-groups`.
6. **`GET /api/v1/credentials` cannot list the installation's.** `subject::resolve` treats an absent
   `username` as "the caller", so even an administrator has no way to ask "every credential here" —
   which is what an operator auditing outstanding client passwords wants. Suggested: `username=*`,
   or a separate administrative query parameter.
7. **Minor:** `POST /api/v1/users` answers `200` with the created `User`; a `201` with a `Location`
   would be more conventional, but nothing in the UI depends on it and changing it now would be a
   wire change for its own sake. Recorded, not asked for.

## Exit checks

```
$ cd rustak-ui && trunk build
2026-09-18T15:18:50.809774Z  INFO 🚀 Starting trunk 0.21.14
2026-09-18T15:18:50.810185Z  INFO 📦 starting build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.11s
2026-09-18T15:18:51.780632Z  INFO applying new distribution
2026-09-18T15:18:51.781911Z  INFO ✅ success

$ cargo clippy --all-targets --target wasm32-unknown-unknown -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.21s

$ cargo test
running 8 tests
test pages::group_members::tests::a_channel_not_held_at_all_is_added ... ok
test components::groups_picker::tests::a_channel_with_neither_direction_is_not_a_membership ... ok
test components::qr_code::tests::adjacent_modules_become_one_run ... ok
test components::groups_picker::tests::two_single_direction_rows_fold_into_one_channel ... ok
test components::groups_picker::tests::a_membership_the_provider_owns_is_left_out_of_what_is_sent_back ... ok
test pages::group_members::tests::switching_one_channel_off_leaves_the_others_alone ... ok
test pages::group_members::tests::switching_the_last_direction_off_removes_the_membership ... ok
test components::qr_code::tests::an_enrolment_url_encodes ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ cargo fmt --check
(clean, no output)

$ ./scripts/check-file-length.sh
(exit 0)
```

`trunk build --release` and `cargo build --release --target wasm32-unknown-unknown` are clean too,
which matters separately because the demo branches vanish there — that is what surfaced unused
imports in M0-13. Release bundle: `*_bg.wasm` 2 095 217 B, `styles-*.css` 29 905 B, `*.js` 57 037 B
(M0-13 recorded 1 441 826 B of wasm, before this brief's eleven pages, the four components and the
setup CA step another brief added).

`check-file-length.sh` reads `git ls-files`, so it does not yet see this brief's new files. Running
its own `awk` over every non-fixture file in the crate reports nothing over the limit; the longest
this brief owns are `pages/groups.rs` 277, `pages/panels/credentials.rs` 205, `pages/panels/
devices.rs` 183 and `pages/group_members.rs` 183.

### End to end

Built in the order the launcher requires (`trunk build`, then `cargo build -p rustak-server`; the UI
is embedded by `include_dir!` at compile time). The server compiled first time — no wait-and-retry
was needed, and nothing under `rustak-server/**` was edited.

```
$ cd e2e && npm run typecheck
> tsc --noEmit
(clean)

$ RUSTAK_E2E_CHROMIUM="…/chromium-1234/…/Google Chrome for Testing" npx playwright test
Running 20 tests using 1 worker
  ✓   1 [setup] › setup.spec.ts › the first-run wizard turns a token on disk into an administrator who can sign in (1.0s)
  ✓   2 [setup] › setup.spec.ts › the wizard closes itself for good once it has been completed (318ms)
  ✓   3 [chromium] › auth.spec.ts › a browser holding no passkey for this server cannot sign in, and is not told why (538ms)
  ✓   4 [chromium] › auth.spec.ts › a passkey registered for one host is refused at another (587ms)
  ✓   5 [chromium] › auth.spec.ts › an administrator signs in with a passkey, and signing out ends the session (514ms)
  ✓   6 [chromium] › auth.spec.ts › a passkey the browser cannot offer on its own is reached by naming the account (520ms)
  ✓   7 [chromium] › identity.spec.ts › an administrator creates an account, and it opens on its own page (553ms)
  ✓   8 [chromium] › identity.spec.ts › minting an enrolment token shows the secret once, with a scannable QR code (1.1s)
  ✓   9 [chromium] › identity.spec.ts › a credential can be revoked, and says so afterwards (1.2s)
  ✓  10 [chromium] › identity.spec.ts › a channel is created and granted to an account (1.1s)
  ✓  11 [chromium] › identity.spec.ts › anybody can mint an enrolment token for their own phone (719ms)
  ✓  12 [chromium] › navigation.spec.ts › every destination in the navigation strip opens the page it names (1.2s)
  ✓  13 [chromium] › navigation.spec.ts › an account's own page is a deep link, and the strip still says where it is (433ms)
  ✓  14 [chromium] › navigation.spec.ts › a name no account could have says so rather than failing to load (324ms)
  ✓  15 [chromium] › navigation.spec.ts › a deep link into the console is served by the single-page fallback (331ms)
  ✓  16 [chromium] › navigation.spec.ts › an address nothing matches reaches the application's own not-found page (307ms)
  ✓  17 [chromium] › navigation.spec.ts › the landing page gets out of the way of somebody already signed in (392ms)
  ✓  18 [chromium] › smoke.spec.ts › robots.txt is served before the SPA catch-all (16ms)
  ✓  19 [chromium] › smoke.spec.ts › the API reports its own health, and says nothing about the storage behind it (4ms)
  ✓  20 [chromium] › smoke.spec.ts › the application boots and renders (307ms)

  20 passed (16.0s)
```

`e2e/tests/identity.spec.ts` is new (five tests). The QR test asserts the encoded URL and the shown
secret **agree** — `url.contains(encodeURIComponent(secret))` — because a link built from the wrong
token would enrol nothing and would look identical; and it reloads afterwards and asserts the
secret appears nowhere in the document, which is the show-once contract stated as a test.

## Walkthrough (`trunk serve` on :8081, `?demo`, 1280×900 and 375×812)

| Page | What rendered |
|---|---|
| `/admin/users?demo` | "Add an account" (username with live `Username::parse` validation, display name, email, kind) over the four fixture rows, each name now a link. |
| `/admin/users/bhavna?demo` | Heading "Bhavna Rao / bhavna · Person", "All accounts" back link, four tabs. **Profile**: the identity-provider notice, the facts list, the display-name field and the Administrator/Enabled switches. **Devices**: one row. **Credentials**: mint form + the fixture credential. **Channels**: four channels, `Blue Team` greyed with "From single sign-on · set by single sign-on". |
| Mint → reveal | Minted "Bhavna's spare": the show-once panel, `SECRET`, `ENROLMENT LINK` reading `tak://com.atakmap.app/enroll?host=tak.example.com&username=bhavna&token=…`, the QR code beside them, and the ATAK Quick Connect instructions. The list below reloaded with the new row. |
| Client password | Selecting it in the Kind box raises the amber compatibility warning and switches the expiry help to "ninety days". |
| `/admin/groups?demo` | Create form; four channels with `bit 0`–`bit 3`, source pills, description fields (disabled for the built-in and the single-sign-on one) and Save/Delete. Selecting "Command" tinted the row and opened "Members of Command" with all four accounts and their Write/Read switches. |
| `/admin/devices?demo` | "Enrolled devices" with the filter box and four rows — callsign, uid, owner, platform/version, model, last seen, last IP, certificate — each with Forget. |
| `/admin/credentials?demo` | "Signed in as Avery Quinn", the enrolling-a-device note, the mint form, and "Your credentials" with `Spent` and `Active` pills. |
| Confirm | "Revoke" becomes `Revoke 'X'? Any certificate issued with it is revoked too.` with Cancel / "Revoke it"; "Forget" becomes the device question naming the credential route to a real revocation. |
| 375×812 | Every row collapses to one column with the pill, the button and the switches at their own width; the tab strip wraps; the reveal panel stacks the values above the QR. Two fixes came out of this pass: the responsive `justify-self` rule only matched `.btn-group`, so a bare `ConfirmButton` stretched into a bar (now `> .btn`), and the profile facts list ran straight into the display-name label (now a `profile-panel__facts` margin). A third came from the desktop pass: the inline forms were bottom-aligned, which lines up the *help text* rather than the inputs — they are top-aligned now. |

## Files

**New (21):** `rustak-ui/src/api/{credentials,devices,groups}.rs`;
`rustak-ui/src/components/{qr_code,secret_reveal,confirm,groups_picker}.rs`;
`rustak-ui/src/pages/{devices,groups,group_members,me,user_create,user_detail}.rs`;
`rustak-ui/src/pages/panels/{mod,channels,credentials,devices,mint,profile}.rs`;
`e2e/tests/identity.spec.ts`; this file.

**Changed (14):** `rustak-ui/Cargo.toml` (+`Cargo.lock`: `qrcode 0.14.1`, no transitive
dependencies); `rustak-ui/styles.scss`; `rustak-ui/src/{app,util}.rs`;
`rustak-ui/src/api/{mod,users}.rs`; `rustak-ui/src/components/{mod,admin_shell}.rs`;
`rustak-ui/src/fixtures/{data,store}.rs`; `rustak-ui/src/pages/{mod,stubs,users}.rs`;
`e2e/tests/navigation.spec.ts`.

**Not changed:** `rustak-api/**` (nothing additive was needed), `rustak-server/**`, every other crate.
