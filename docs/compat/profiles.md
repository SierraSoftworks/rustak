# Device profiles — manual compatibility checklist

Everything on this page is a **manual gate**. rustak's automated suites assert the wire contract —
statuses, headers, zip layout, the exact bytes of a `.pref` — but no CI job runs ATAK, WinTAK or
iTAK, so whether a real client *imports* what we send is checked by a person before a release that
touches `rustak-server/src/profiles/**` or `rustak-server/src/marti/profiles.rs`.

Run through this once per release, and record the result (client name, version, date, outcome) in
the release notes.

## 0. What is already covered automatically

Do not re-check these by hand; they fail the build if they break.

| Covered | Where |
|---|---|
| `.pref` bytes: single-quoted declaration, no `encoding`, no whitespace between elements, `class java.lang.*` on every entry, deterministic order, escaping | `profiles::prefs` unit tests |
| Manifest round-trip, nested `MANIFEST/` prefix, `Content` without `zipEntry` dropped, minted `uid` | `files::package` unit tests |
| Zip layout: `fileN/<name>`, `MANIFEST/manifest.xml`, `onReceiveImport`/`onReceiveDelete`, `multiFile` keeping stored directories | `profiles::builder` unit tests |
| `204` / `200` / `304`, `Last-Modified`, `If-Modified-Since`, `relativePath` traversal, `clientUid` required, channel scoping | `rustak-server/tests/profiles_contract.rs` |
| Config-package layout for both variants, and that no credential is minted or embedded | `profiles::config_package` unit tests and the contract suite |

## 1. Enrolment profile — ATAK-CIV

1. Create an enrolment token for a test account (`/api/v1/credentials`).
2. Enrol a fresh ATAK-CIV install against this server (scan or type the token).
3. **Expect:** the enrolment completes, and ATAK logs a `200` for
   `GET /Marti/api/tls/profile/enrollment`.
4. Open **Settings → Show all preferences** and confirm:
   - [ ] `deviceProfileEnableOnConnect` is **on**. If it is not, every connection profile configured
         later will silently never fire — this is the single most important item on the page.
   - [ ] the Channels selector is available (`prefs_enable_channels`).
   - [ ] the server connection widget is shown.
5. [ ] No import error dialog appeared. An error here usually means a `class` attribute went
       missing or the XML declaration was normalised.

## 2. Connection profile — ATAK-CIV

1. With `deviceProfileEnableOnConnect` on, create a profile in the admin UI with
   **apply on connect** set and one preference (e.g. `locationTeam = Green`).
2. Disconnect and reconnect the stream.
3. [ ] ATAK imports the package and the preference takes effect.
4. Reconnect again without changing anything.
5. [ ] The second fetch is answered `204` (nothing changed within `syncSecago`) and ATAK does not
       re-import. Check the server log rather than the client for the status.

## 3. Tool profile and `relativePath`

1. Give a profile a `tool` name and attach a file under a directory (e.g. `maps/source.xml`).
2. [ ] `GET /Marti/api/device/profile/tool/<tool>?clientUid=…&syncSecago=-1` returns the package.
3. [ ] `…/tool/<tool>/file?relativePath=/maps` returns the single file **raw**, with
       `Content-Disposition: attachment; filename=source.xml`.
4. Repeat the request with the `Last-Modified` value echoed back as `If-Modified-Since`.
5. [ ] The answer is `304` with an empty body, and ATAK logs it without treating it as an error.

## 4. Configuration package — ATAK and WinTAK

1. `POST /api/v1/config-packages` with `variant: "wintak_atak"`, `include_client_cert: false`.
2. Transfer `<user>_CONFIG.zip` to the device and import it.
3. [ ] ATAK shows one import prompt and, after accepting, a new server entry appears in
       **Settings → Network Preferences → Network Connections**.
4. [ ] The entry's connect string is `<host>:<stream port>:ssl` and it is enabled.
5. [ ] `truststore.p12` has been re-homed to `<atak root>/cert/truststore.p12` by ATAK's certificate
       sorter — the `.pref` says `cert/truststore.p12` and relies on this.
6. [ ] Connecting prompts for a **username and client password** (the enrolment variant sets
       `enrollForCertificateWithTrust0=true` and `useAuth0=true`), and after entering them the
       device enrols and connects.
7. [ ] Repeat on WinTAK with the same file.

## 5. Configuration package — iTAK

1. `POST /api/v1/config-packages` with `variant: "itak"`.
2. Import `<user>_CONFIG_iTAK.zip`.
3. [ ] iTAK reads the flat archive (`config.pref`, `truststore.p12`) with no manifest and the
       server appears in its list.
4. [ ] The unsuffixed `caLocation` / `caPassword` keys in the app preference group were honoured.

**Unverified:** the iTAK path prefix. rustak writes `cert/truststore.p12` for both variants because
that is what ATAK's sorter produces; we have no source-level confirmation that iTAK re-homes the
same way. If step 4 fails, try a package built with a bare `truststore.p12` and record the result
in `.claude/plan/compat/profiles.md`.

## 6. Things that are expected to do nothing

Confirm these *do not* work, so that nobody later designs a feature around them:

- [ ] `username0` / `password0` in a `cot_streams` group are ignored by ATAK. rustak never emits
      them; if a future change adds them, they will still not be read.
- [ ] `GET /Marti/api/device/profile/enrollment` is **not** a route. Real TAK Server has no such
      mapping either, and the enrolment profile lives only under `/Marti/api/tls/`.

## 7. Known gaps

- **A configuration package cannot carry a client keystore.** rustak never holds a device's private
  key — a certificate is issued against a signing request the device made — so there is nothing to
  assemble a `.p12` from after the fact. `include_client_cert: true` is refused with that
  explanation. The enrolment variant is the supported path.
- **`useStreamingGroup`** (TAK Server's option to take the caller's channels from their streaming
  subscription rather than from the HTTP request) is not implemented; channels come from the
  authenticated principal.
- **Directory-backed profiles** (`/Marti/api/device/profile/directories`) answer `501`. rustak
  stores profile files in the content store rather than on a filesystem path an operator manages.
