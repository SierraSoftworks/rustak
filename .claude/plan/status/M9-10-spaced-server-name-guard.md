# M9-10 — Every suite now runs with a server name a careless implementation breaks on

**Status:** delivered. Every in-process test, every interop suite and the e2e suite start an
installation called **`Rustak Test & Co. (näme)`**. Two interpolations of `[server] name` needed
fixing (the mission-archive connection string, the mission-token `iss` fallback); the rest already
escape or derive. The explicit protobuf+XML relayed-event regression and the CloudTAK-log assertion
are in place, and the new node-tak relay scenario has been proven to reproduce the production
failure through `@tak-ps/node-cot` itself.

Everything except the two container suites (`interop/cloudtak` scenarios, `interop/eud` scenarios)
was run here and passes. §6 says exactly what only the nightly run can prove.

---

## 1. The name

```
Rustak Test & Co. (näme)
```

| character | legal in a display name | illegal or special in |
|---|---|---|
| space | yes | an XML name; an unquoted HTTP header token |
| `&` | yes | XML text and attribute values (`&amp;`); a query-string separator |
| `(` `)` | yes | an XML name; RFC 9110 `separators`, so a bare `Content-Disposition` token |
| `.` | yes | the *start* of an XML name; a DNS label separator |
| `ä` | yes | an HTTP header value, a DNS label, an ASCII filename — but **legal** in an XML name, which is the point: the flow-tag derivation must keep it rather than mangle everything non-ASCII |

It is spelled in exactly three places, and a Rust unit test (`config::tests::
the_name_every_suite_runs_under_is_one_a_careless_derivation_breaks_on`) `include_str!`s the other
two so that a change to one fails the build:

| Where | What it feeds |
|---|---|
| `rustak_server::config::TEST_SERVER_NAME` | `Config::testing` and `AppContext::new_mock`, therefore every unit test, every `tests/*.rs` suite and the `stream_support` fake-EUD harness |
| `interop/shared/src/names.ts` (`HOSTILE_SERVER_NAME` / `hostileServerName(suffix)`) | `interop/node-tak`, `interop/eud` (per scenario), `interop/cloudtak`, and the shared launcher's default when a suite passes no name at all |
| `e2e/scripts/start-server.mjs` (`SERVER_NAME`) | the Playwright suite |

The suffix form (`Rustak Test & Co. (näme: cloudtak)`) keeps the per-suite and per-scenario
distinctness the interop logs had before, inside the parentheses so the punctuation stays put.

## 2. Every place the name is interpolated, and what happens to it

Audited by grepping every use of `config.server.name` and of the resolved `ServerSettings.name`.

| Site | Grammar | Verdict |
|---|---|---|
| `rustak-cot` flow tag (`stream::Router` → `flow_tags::flow_tag_name`) | **XML attribute name** | Already derived (`9c6f6f4`): every character outside XML 1.0 `NameChar` becomes `-`. `ä` survives, the space/`&`/brackets do not. Now exercised by every suite. |
| `missions::archive::archive_host` → manifest `mission_uid` / `mission_server` | **connection string** (`<host>-8443-ssl-<name>`, `<host>:8443:ssl`) | **Fixed.** See §3.1. |
| `auth::mission_token` fallback `iss` (`rustak/<name>`) | **JWT StringOrURI** | **Hardened.** See §3.2. |
| `Passkeys::for_base_url(.., &config.server.name)` → WebAuthn `rp.name` | JSON string, free text by spec | Correct as is. `rp.id` comes from the base URL, never from here. Now asserted verbatim in `web::api::passkey::tests::the_options_a_browser_is_handed_are_the_ones_the_ui_knows_how_to_read`. |
| `GET /api/v1/settings`, `GET /api/v1/setup/status` | JSON | serde escapes. `web::api::settings::tests::an_administrator_is_told_what_the_server_is` now asserts the configured name comes back whole. |
| admin UI (`pages/dashboard.rs`, `pages/settings_security.rs`) | HTML text node | Yew escapes text nodes; the name never reaches an attribute or `document.title` (the title is the static `rustak | Sierra Softworks`). |
| `runtime.rs` start-up log (`name = %config.server.name`) | tracing structured field | Correct; a field value is not a grammar. |
| `main.rs --check` `println!` | terminal text | Correct. |

Places that turn out **not** to use `[server] name`, checked so the next reader does not have to:

- `GET /Marti/api/version` — the product string is `TAK Server rustak-<version>`; the display name
  is not in it.
- Certificate subjects and SANs — the CA common name is its own field, `[pki] ca_common_name`
  (default `rustak CA`, which already contains a space), and leaf subjects are built from
  `[pki] name_entries` plus the username. **No `--check` rejection is needed**: there is no rule
  that has to refuse a spaced `[server] name`, because nothing derives a CN or a DNS label from it.
- `Content-Disposition` filenames — the mission archive's is `<mission>_<guid>.zip` (percent-encoded
  by `archive::encode_filename`), the configuration package's is `<username>_CONFIG.zip`.
- The `.pref` description and the enrolment `prefs_enable_channels_host-<host>` key — both built
  from `[marti] public_host` / the canonical domain, and `profiles::prefs::render` escapes keys and
  values regardless.
- The `tak://` enrolment QR — `host`, `username`, `token` only.
- The mission-package manifest generally — `files::package::write_manifest` escapes every attribute
  value, which is why §3.1 is a *semantic* bug and not a well-formedness one.

## 3. The two fixes

### 3.1 `missions::archive::archive_host` — `rustak-server/src/missions/archive.rs`

`mission_uid` and `mission_server` are a connection string ATAK dials. The fallback was
`[marti] public_host` → **`[server] name`**, so an installation that had not set `public_host`
published `Rustak Test & Co. (näme):8443:ssl`. Because the manifest writer escapes attribute values
properly, the package stayed perfectly well-formed while being perfectly unusable — the kind of
failure nobody reports.

The ladder is now `[marti] public_host` → canonical `[server] domains` entry → `localhost`, which is
the same ladder `web::api::config_packages::host` already used. `store_archive` was duplicating the
old ladder inline and now calls `archive_host()`, so there is one of it.

Test: `missions::archive::tests::the_connection_string_is_built_from_a_host_and_never_from_the_display_name`.

### 3.2 `auth::mission_token::default_issuer` — `rustak-server/src/auth/mission_token.rs`

`iss` is a **StringOrURI** (RFC 7519 §2): an arbitrary string is fine, but one containing `:` must
be a URI. `rustak/<name>` with `[server] name = "SierraSoftworks: TAK"` is neither. The derivation
now replaces `:` with `-` and carries everything else — spaces, `&`, non-ASCII — verbatim, because
those are ordinary JSON string characters. Nothing verifies this claim, so no token compatibility
changes; `[auth] issuer` still overrides it outright.

Test: `auth::mission_token::tests::the_default_issuer_stays_a_plain_string_whatever_the_installation_is_called`.

## 4. The regressions

### `rustak-server/tests/hostile_server_name.rs` (new, 4 tests)

The strict reader is `quick-xml` with end-name checking and its checked attribute iterator, plus
`rustak_cot::xml::is_name` on every element and attribute name. quick-xml refuses the production
document with *"attribute key must be directly followed by `=` or space"* — the same class of
refusal as sax's "Attribute without value".

| Test | What it drives |
|---|---|
| `the_reader_these_tests_trust_refuses_what_production_sent` | The reader's own self-test, against the literal bytes the outage put on the wire. Without it the other three could pass vacuously. |
| `a_protobuf_peer_and_an_xml_peer_both_read_a_relay_from_a_hostilely_named_server` | Real TLS listener, three enrolled fake EUDs. BRAVO negotiates (`Mode::Proto`), CHARLIE refuses (`Mode::Xml`), both receive ALPHA's SA; the flow tag is present, its derived name is an XML name and carries no space/`&`/bracket, and what CHARLIE received survives the strict reader. |
| `the_uid_history_document_is_one_a_strict_reader_takes_whole` | `GET /Marti/api/cot/xml/{uid}/all`. The row is appended through the real `HistoryWriter` as **protobuf**, so the document is built by decoding it back — a different path from the mission document's. |
| `a_missions_cot_document_is_one_a_strict_reader_takes_whole` | `GET /Marti/api/missions/guid/{guid}/cot` — the document production answered a bare `500` for. |

**Proven to be a guard, not a decoration.** With `flow_tag_name`'s sanitisation temporarily removed,
3 of the 4 fail (the self-test is the one that should not). Restored afterwards; `rustak-cot` is
untouched in the final diff.

### `interop/node-tak/tests/relay.test.ts` (new, runs on every PR)

Two enrolled `TAK.connect` clients on `ssl://`; one sends an `<event>` as bytes, the other receives
it and **`@tak-ps/node-cot` parses it** — the library CloudTAK itself uses. The assertion is that
`<_flow-tags_>` carries exactly one attribute, that its name matches the XML 1.0 `Name` production
(written out in the scenario rather than borrowed from rustak), and that none of the display name's
punctuation survived into it.

Also proven as a guard: with the sanitisation removed and the binary rebuilt, node-cot logs
`Error parsing Error: Invalid attribute name`, the message never arrives, and the test fails with
`no relay of INTEROP-RELAY-SENDER within 20000ms`. That is the production incident, reproduced by a
suite that runs on every pull request.

### `interop/cloudtak` — the log assertion (brief item 3)

It did **not** already have one. Added:

- `src/compose.ts`: `CLOUDTAK_SERVICE`, `serviceLog(service)` (`docker compose logs --no-log-prefix`)
  and `parseFailures(log)` against a substring list — `Failed to parse CoT XML`,
  `Attribute without value`, plus the neighbouring sax refusals (`Invalid attribute name`,
  `Unquoted attribute value`, `Invalid character in tag name`). Substrings rather than expressions
  because the wording is somebody else's and a regex tuned to one release silently stops matching.
- `src/run.ts`: a final `cloudtak-parse-log` step, reported and counted like every other, that fails
  the run (and therefore keeps the container logs as artefacts) when anything matches.
- `tests/stack.test.ts`: four new unit tests — the compose file defines the service the run reads,
  the configuration and the wizard agree on the hostile name and it carries each character,
  the name renders as one TOML string that survives a JSON round trip, and `parseFailures` matches
  the two lines production actually logged (case-insensitively) and nothing else.

## 5. Docs

- `docs/deployment.md` → `[server]`: `name` is a free-text **display** name, safe to contain spaces,
  punctuation and non-ASCII letters; it is not a host name and is never parsed; the two derivations
  that exist are named.
- `docs/interop.md` → a paragraph under the suite table on the shared hostile default and why, and a
  bullet in the CloudTAK section on `cloudtak-parse-log`.
- `interop/node-tak/README.md` → the new scenario file in the tree listing.

## 6. What only the nightly run can prove

Docker is not available here, so **no container in `interop/cloudtak` or `interop/eud` was started**.
Their configuration and assertions are correct as far as static checks and unit tests can say, and
everything above the process boundary is shared with suites that did run. What is unproven:

1. **`cloudtak-parse-log` against a real CloudTAK.** The matcher is unit-tested against the exact
   lines production logged, and the compose file is asserted to define the service it reads — but
   whether `docker compose logs --no-log-prefix cloudtak` returns what this expects on the pinned
   image has never been executed. If it returns nothing, the step passes vacuously; the first
   nightly run should be read for the step's own reason line, which names the server the stack ran.
2. **Whether CloudTAK's sax build accepts the `ä`.** `ä` (U+00E4) is inside sax-js's `nameBody`
   character class (`À-˿`), so it should parse — but the only proof is a run. If the
   nightly `cloudtak-parse-log` trips on the *fixed* build, that is the thing to look at first, and
   the answer is to narrow `flow_tag_name` to ASCII rather than to weaken this suite.
3. **The EUD scenarios under the new per-scenario names.** `commotest` never sees the server name
   (it is not in anything ATAK parses on the enrolment or stream path), so the risk is low, but the
   scenarios have not been run.
4. **Everything else in the two nightly suites** that was already gated on Docker before this brief.

## 7. Exit checks

All run on this machine. Final lines verbatim.

```
$ cargo fmt --check
(no output)
exit=0
```

```
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.02s
exit=0
```

```
$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.55s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 8 other files
exit=0
```

```
$ ./scripts/check-file-length.sh
(no output)
exit=0
```

```
$ cargo test --workspace
     Running tests/hostile_server_name.rs (target/debug/deps/hostile_server_name-3f83c9f9bbd46706)

running 4 tests
test the_reader_these_tests_trust_refuses_what_production_sent ... ok
test the_uid_history_document_is_one_a_strict_reader_takes_whole ... ok
test a_missions_cot_document_is_one_a_strict_reader_takes_whole ... ok
test a_protobuf_peer_and_an_xml_peer_both_read_a_relay_from_a_hostilely_named_server ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.34s

...

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

all doctests ran in 1.78s; merged doctests compilation took 1.34s
exit=0
```

3361 tests passed across 56 test binaries; 0 failed.

```
$ cd interop/node-tak && npm run typecheck
> tsc --noEmit
exit=0

$ cd interop/node-tak && npm test
ℹ tests 35
ℹ pass 35
ℹ fail 0
exit=0
```

```
$ cd interop/cloudtak && npm run typecheck
> tsc --noEmit
exit=0

$ cd interop/cloudtak && npm run test:unit
ℹ tests 51
ℹ pass 51
ℹ fail 0
exit=0
```

```
$ cd e2e && npm run typecheck
> tsc --noEmit
exit=0

$ cd e2e && npx playwright test
  ✓  41 [chromium] › tests/smoke.spec.ts:42:1 › the application boots and renders (172ms)

  41 passed (19.5s)
exit=0
```

Not named in the brief, but run because their configuration changed:

```
$ cd interop/eud && npm run typecheck && npm run test:unit
ℹ tests 43
ℹ pass 43
ℹ fail 0
exit=0
```

### Two notes on the runs

- **`cargo test --workspace` failed twice mid-session in `rustak-plugin-adsb` only**
  (`sources/{aggregator,opensky,state}.rs`, `E0308`), which is M9-08's in-flight work; `rustak-server`
  dev-depends on both plugin crates for `feed_sidecars`, so nothing in this brief could be tested
  while it was mid-edit. Retried rather than worked around; the run quoted above is a clean whole
  workspace with M9-08's files compiling. Nothing in the two plugin crates was touched.
- **The e2e suite needed `npx playwright install chromium`** (the browser was missing from the
  cache, not something this brief changed) and a **UI rebuild**: `rustak-ui/dist` predated
  `80ff443 fix(ui): Remount the situation drawer when a different row is selected`, so
  `live.spec.ts:75` failed before the rebuild — reproduced with the *old* server name too, so not a
  regression from this work. After `trunk build` and a server rebuild, 41/41 pass.

## 8. Files

**New**

- `rustak-server/tests/hostile_server_name.rs`
- `interop/shared/src/names.ts`
- `interop/node-tak/tests/relay.test.ts`

**Source fixes**

- `rustak-server/src/missions/archive.rs` — `archive_host` ladder, `store_archive` deduplicated, test
- `rustak-server/src/auth/mission_token.rs` — `default_issuer`, test

**Test/interop configuration**

- `rustak-server/src/config/mod.rs` — `TEST_SERVER_NAME`, `Config::testing`, cross-file consistency test
- `rustak-server/src/services/mock.rs` — `new_mock` sets it before the caller's closure
- `rustak-server/src/web/api/passkey.rs`, `rustak-server/src/web/api/settings.rs` — name expectations
- `interop/shared/src/launch.ts` — the default `[server] name` is the hostile one
- `interop/node-tak/src/{rustak,bootstrap,run}.ts`, `interop/node-tak/README.md`
- `interop/eud/src/{session,run}.ts`
- `interop/cloudtak/src/{rustak,compose,run}.ts`, `interop/cloudtak/tests/stack.test.ts`
- `e2e/scripts/start-server.mjs`

**Docs**

- `docs/deployment.md`, `docs/interop.md`
