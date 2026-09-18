# M2-05 — `interop/node-tak`: CloudTAK-library contract suite (scaffold + first scenarios) — complete

Brief: `.claude/plan/briefs/M2-05-interop-node-tak.md`
Read first: `conventions.md`; `compat/{cloudtak,oauth,enrollment,groups,contacts,missions,files,streaming}.md`;
`research/03-cloudtak-node-tak-contract.md`; status files `M0-11`, `M0-12`, `M2-02`;
`e2e/scripts/start-server.mjs`. node-tak/node-CoT sources read for API reference (MIT), nothing copied.

## What was built

A TypeScript project under `interop/node-tak/` driving `@tak-ps/node-tak@12.30.0` and
`@tak-ps/node-cot@14.52.2` — the libraries CloudTAK pins — against a throwaway rustak the suite
starts itself.

| File | Functional lines | Contents |
|---|---:|---|
| `src/run.ts` | 84 | `npm test`: start the server, wait for it, run the bootstrap child, run the scenarios, tear down on every exit path including `SIGINT` |
| `src/rustak.ts` | 195 | `resolveBinary`, stale-scratch sweep, port reservation, the generated `config.toml`, `startServer`, `waitForServer` |
| `src/prepare.ts` | 26 | the bootstrap child's entry point: bootstrap → probe → write the session |
| `src/bootstrap.ts` | 133 | the `/api/v1` walk: health → setup status → setup token → admin → passkey → wizard → `/me` → client password |
| `src/webauthn.ts` | 185 | `SoftAuthenticator`: RSA key pair, CBOR, `none` attestation, RS256 assertions |
| `src/probe.ts` | 47 | which compatibility surfaces this server serves |
| `src/surfaces.ts` | 58 | the ten surfaces, their probe paths, and the `TODO(<brief>)` each missing one carries |
| `src/session.ts` | 51 | what the runner hands the scenarios; `unless`/`unlessAll` |
| `src/client.ts` | 26 | `tokenClient`, `passwordClient`, `enroll`, `certificateClient` — CloudTAK's three roles |
| `tests/bootstrap.test.ts` | 76 | the harness's own contract (4 scenarios, **all run today**) |
| `tests/{version,login,enrollment,groups,contacts,stream,missions,files}.test.ts` | 46/71/70/51/41/49/58/59 | 21 scenarios; 3 run today, the other 18 skip with a reason naming the brief that will serve them |
| `README.md`, `.gitignore`, `package.json`, `package-lock.json`, `tsconfig.json` | — | how to run it, what it asserts, and the gap it cannot close |

Also: the `interop-node-tak` job in `.github/workflows/rust.yml` (still `if: false`), and one
paragraph in `docs/interop.md`.

**25 scenarios: 7 pass, 18 skip, 0 fail.** Nothing in the server, the workspace manifests or any
other crate was touched.

Seven rather than four because **M2-04 landed while this brief was being written**. `martiVersion`
and `filesConfig` started answering, the runner's probe noticed on the next run, and three scenarios
that had been skipping began running and passing — with no edit to this suite. That is the design
working, and it is the best evidence available that the rest will behave the same way when M2-03,
M2-06, M1-05, M3 and M4 land. It also confirms M2-04 mounted the Marti scope **ahead of** the
single-page shell's catch-all: `GET /Marti/api/version` answers a version string rather than HTML.

## How it runs, and why it is three processes

`npm test` is `tsx src/run.ts`, which:

1. resolves `target/{debug,release}/rustak` (newer wins; `RUSTAK_INTEROP_BINARY` overrides and is an
   error when it does not exist), sweeps scratch directories older than six hours, reserves three
   ports and writes a `config.toml` into a fresh `mkdtemp` directory;
2. starts the server there and waits for **both** the listener to accept and `<data_dir>/pki/ca.crt`
   to exist;
3. runs `src/prepare.ts` in a child process with `NODE_EXTRA_CA_CERTS` pointing at that file;
4. runs the scenarios under `node --test` in child processes with the same trust store and the
   session file's path in the environment;
5. removes the directory, on every exit path.

**Step 3 is a separate process because `NODE_EXTRA_CA_CERTS` is read once, at process start, and
the authority does not exist until step 1 has finished.** The runner can therefore never trust the
server it just started. The alternatives were to pass an explicit `ca` to an `undici` `Agent`
everywhere, or to set `rejectUnauthorized: false` — both of which would have made the suite pass
against a deployment CloudTAK could not log in to, because CloudTAK's `webtak` calls go through
`undici`'s `fetch` with **full verification and no override available in its own configuration**
(`compat/cloudtak.md` §3, the single most common CloudTAK bring-up failure). Reproducing the trust
path is most of the value of this suite existing, so it is reproduced rather than worked around.

### The scratch server's configuration

`[web.public.tls] mode = "internal"` (a real internal CA), `[server] domains = ["localhost"]` and
`base_url = "https://localhost:<port>"`, all three listeners configured on reserved ports,
`user_acl`/`admin_acl` `'true'`, `client_passwords_enabled = true`. No `[marti]` section is written,
so the suite works both before and after M2-04 adds one.

The listeners **bind `127.0.0.1`** and the suite **reaches them at `localhost`**, for the two
reasons `e2e/scripts/start-server.mjs` gives in opposite directions: WebAuthn identifies a relying
party by domain, so an address cannot register the passkey the bootstrap needs (M0-11 deviation 6);
and binding the *name* would make start-up depend on whether the machine has an IPv6 loopback, where
a bind of a family it does not have is a failure rather than a fallback. Node's Happy Eyeballs
(`autoSelectFamily`, on by default since Node 20) bridges the two.

## Decisions worth recording

### The passkey ceremony is performed rather than bypassed

The brief allowed a `RUSTAK_TEST_ADMIN_TOKEN` the server would accept in `testing` builds if M0-11
exposed no non-browser path to an administrator bearer. It does not: `POST /setup/admin` returns a
**registration token**, and the only thing that token authorises is registering a passkey — the
`finish` of which is what returns a `TokenResponse`.

`src/webauthn.ts` is therefore a genuine software authenticator, the TypeScript twin of
`rustak-server/src/testing/authenticator.rs`: real RSA key pair, real CBOR, `none` attestation, a
real RSASSA-PKCS1-v1_5 signature over `authData || sha256(clientDataJSON)`, with the `UP|UV|BE|BS|AT`
flags the server requires. Preferred to the environment variable because it needs no change to the
server and, more importantly, **adds no code path by which any build could ever accept an
environment variable as an administrator credential**. The cost is 185 lines this suite owns.

It works: the bootstrap registers a passkey and signs in on every run.

### Skips are probed, not hard-coded

Each scenario names one or more surfaces from `src/surfaces.ts`. The runner probes all ten once
against the server it started and writes the result into the session; `unless(session, surface)`
returns `false` (run it) or the surface's `TODO(<brief>)` string (skip with that reason). A skipped
scenario reports as:

```
﹣ exchanges a client password for a token node-tak can parse
    # TODO(M2-03): POST /oauth/token is not served yet — the password grant lands with M2-03 (marti/oauth.rs).
```

So **nothing needs editing when a brief lands** — the probe notices and the scenario starts running.
`unlessAll` reports the *first* missing surface, because a scenario needing two absent things is not
twice as skipped and one of them is the thing to go and read about.

Verified in both directions, twice. Deliberately: pointing `martiVersion.path` at `/api/v1/health`
temporarily (reverted) made the probe report it served, ran the two `martiVersion` scenarios — one
passing, one failing on the real assertion — and left the mTLS scenario skipped against `tlsConfig`,
its first missing dependency. And for real: M2-04 landed mid-brief, and `version.test.ts` and the
`/files/api/config` scenario went from skipped to passing between two runs with nothing edited.

### The probe had to learn about the single-page shell — and so does M2-04

The obvious rule, "`404` means not implemented", is **wrong against rustak**: `web/ui.rs` answers
anything it does not recognise with the admin UI's `index.html` at `200 text/html`, because the UI
routes on the client and a reloaded deep link has to reach the shell. The first run of this suite
duly reported every Marti surface as present and then failed with an HTML page where it expected a
version string.

The probe now treats a `404` **or HTML on a success status** as absent. The corollary is for whoever
mounts the Marti scope: **it has to sit ahead of the shell's catch-all.** If it does not, CloudTAK
gets an HTML page where it expects JSON, node-tak's `isHTML` sniffing turns it into a
`TAKServerError`, and the endpoint it names will be one that is perfectly well implemented. Recorded
here, in `interop/node-tak/README.md` and in `src/probe.ts` because M2-04 is in flight.

### Node 24, not e2e's 22

CloudTAK's own `api/package.json` declares `"node": ">= 24"`. The point of this suite is to run its
libraries the way it runs them, so the job pins 24 and `package.json` declares the same engine.

### One scenario file the brief did not list

`tests/bootstrap.test.ts` exists because **a suite where every scenario skips looks exactly like a
suite that is quietly broken.** It asserts what is true today: Node verifies the server's chain
against the internal authority (explicit `ca:`, not the ambient trust store, so it tests the chain
rather than the environment), the certificate carries `DNS:localhost`, the bootstrap left a working
administrator session, the credential minted is a `client_password` with an expiry, and the probe
reached a conclusion about every surface with a well-formed `TODO(<brief>)` on each missing one.

## The gap this suite cannot close

**`/api/v1` has no `POST /users`.** M0-11 ships `GET /users` and `PATCH /users/{username}`; M2-02
added channels and credentials but no provisioning. Accounts arrive through the setup wizard or
through OIDC, and a scratch server has no identity provider — so the suite cannot create a second
account, and the client password is minted **for the administrator** rather than for an ordinary
user.

The consequence: this suite does not yet prove that an ordinary account enrolls and authenticates
the same way an administrator does, and an authorisation bug that only bit non-administrators would
not be caught here. When a provisioning endpoint exists, `src/bootstrap.ts` should create an
ordinary account and mint the client password against it — it is about ten lines, in one function
(`mintClientPassword`), and nothing else in the suite assumes the two identities are the same beyond
one assertion in `tests/bootstrap.test.ts`.

## What passes today, and what flips each skip

| Scenario file | Surface(s) | State | Flipped by |
|---|---|---|---|
| `bootstrap.test.ts` (4) | — | **4 pass** | — |
| `version.test.ts` (3) | `martiVersion` (+ `tlsConfig`, `oauthToken` for the mTLS one) | **2 pass**, 1 skip | landed with **M2-04**; the mTLS scenario still needs M2-03 |
| `files.test.ts` (3) | `filesConfig`, `files` | **1 pass**, 2 skip | `/files/api/config` landed with **M2-04**; `/Marti/sync/*` is **M3** |
| `login.test.ts` (4) | `oauthToken` | skip | **M2-03** `POST /oauth/token` |
| `enrollment.test.ts` (3) | `tlsConfig` (+ `oauthToken`) | skip | **M2-03** `/Marti/api/tls/{config,signClient/v2}` |
| `groups.test.ts` (2) | `groups` | skip | **M2-06** `/Marti/api/groups/{all,active}` |
| `contacts.test.ts` (2) | `contacts`, `clientEndPoints` | skip | **M2-06** `/Marti/api/contacts/all`, `/Marti/api/clientEndPoints` |
| `stream.test.ts` (1) | `stream` (+ `tlsConfig`, `oauthToken`) | skip | **M1-05** the mTLS CoT listener |
| `missions.test.ts` (3) | `missions` | skip | **M4** the mission API |

What the three newly-live scenarios prove about M2-04, beyond "it answers": `GET /Marti/api/version`
returns a string matching `^TAK Server `, which is the form ATAK's `ServerVersion` parser expects;
neither `/Marti/api/version` nor `/Marti/api/version/` emits a `3xx`, which node-tak would treat as
a parseable success; and `GET /files/api/config` returns an integer `uploadSizeLimit`, without which
CloudTAK's setup wizard cannot save a server connection at all.

### The rules each scenario asserts where it is produced

So that a failure names the rule rather than the symptom: exact `Content-Type: application/json`
with no parameters (node-tak compares by string equality — `compat/cloudtak.md` §4); never a `3xx`
on a Marti route (node-tak treats anything under 400 as success — §5); the JWT header a multiple of
four base64url characters and the claims flat (CloudTAK decodes the whole token as one blob and
splits on the first `}` — `compat/oauth.md` §2); `nameEntry` a real array of at least two (`xml-js`
collapses a one-element array and CloudTAK iterates a non-iterable — `compat/enrollment.md` §1);
`signedCert` and each `caN` bare base64 with no PEM armour (§3); repeated `signClient/v2` for one
identity accepted, because CloudTAK re-enrolls within seven days of expiry (§7); contacts a bare
array while `clientEndPoints` is the envelope (`compat/contacts.md`); `bitpos` a non-negative number
and `created` `yyyy-MM-dd` (`compat/groups.md`, both things OpenTAKServer gets wrong); the envelope
`type` strings per mission route family (`compat/missions.md` §2); `t-x-c-t` → `t-x-c-t-r` with
`t-x-takp-v` optional (`compat/streaming.md` §4–6).

## CI

The `interop-node-tak` placeholder is replaced by a real job: `needs: [deduplicate, ui]`, the
**release** UI bundle (`ui-dist` — nothing here drives the admin UI, so a debug bundle's `?demo`
fixtures are dead weight), `cargo build -p rustak-server`, Node 24 with `npm ci` cached on
`interop/node-tak/package-lock.json`, `npm run typecheck`, then `npm test`.

It stays **`if: false`**, with a comment saying exactly what flips it: today it would pass with
seven scenarios green and eighteen skipped, none of them login or enrollment — a green job that
proves almost nothing about the parts of Marti that decide whether CloudTAK can connect. M2-03
landing `/oauth/token` and `/Marti/api/tls/*` makes login and enrollment real gates; at that point
replace `if: false` with the condition the other jobs carry and fold `INTEROP_NODE_TAK_RESULT` back
into the `ci` job's loop, per the comment already there. The `ci` job itself was **not** edited.

`actionlint` reports the same findings on this file as on the committed one, line for line — the
`if: false` diagnostic and the pre-existing shellcheck notes are unchanged, and this brief
introduces none.

## Exit checks

```
$ cd interop/node-tak && npm ci
added 84 packages, and audited 85 packages in 1s
found 0 vulnerabilities

$ npx tsc --noEmit
(no output, exit 0)

$ npm test
[interop] workspace:     /var/folders/.../rustak-interop-node-tak-njfs5i
[interop] webtak:        https://localhost:55603
[interop] marti (mTLS):  https://localhost:55604
[interop] stream:        ssl://localhost:55605
[interop] bootstrapped interop-admin and minted a client password.
[interop] surfaces served:  martiVersion, filesConfig
[interop] surfaces missing: oauthToken, tlsConfig, groups, contacts, clientEndPoints,
                            missions, files, stream
✔ Node verifies the server's chain against the internal authority (128.091916ms)
✔ the bootstrap left a working administrator session (34.723208ms)
✔ the credential CloudTAK will use is a client password (12.202583ms)
✔ every surface was probed, and every missing one names what will flip it (0.27725ms)
✔ reports an upload limit CloudTAK's setup wizard can save (95.22875ms)
✔ answers a bearer token with a plain-text version string (94.689875ms)
✔ never answers a Marti route with a redirect (14.069458ms)
﹣ the certificate config is the ns2 document CloudTAK's parser requires
    # TODO(M2-03): GET /Marti/api/tls/config is not served yet — enrollment lands with M2-03 (marti/tls.rs).
﹣ signs a CSR into a client certificate for the authenticated account
    # TODO(M2-03): POST /oauth/token is not served yet — the password grant lands with M2-03 (marti/oauth.rs).
﹣ accepts a second enrollment for the same identity
    # TODO(M2-03): POST /oauth/token is not served yet — the password grant lands with M2-03 (marti/oauth.rs).
﹣ exchanges a client password for a token node-tak can parse
    # TODO(M2-03): POST /oauth/token is not served yet — the password grant lands with M2-03 (marti/oauth.rs).
﹣ issues exactly the body CloudTAK's client expects
    # TODO(M2-03): POST /oauth/token is not served yet — the password grant lands with M2-03 (marti/oauth.rs).
﹣ issues a JWT whose header and claims survive CloudTAK's parser
    # TODO(M2-03): POST /oauth/token is not served yet — the password grant lands with M2-03 (marti/oauth.rs).
﹣ refuses a password that is not the one minted
    # TODO(M2-03): POST /oauth/token is not served yet — the password grant lands with M2-03 (marti/oauth.rs).
﹣ answers an enrolled client certificate on the mutually authenticated listener
    # TODO(M2-03): GET /Marti/api/tls/config is not served yet — enrollment lands with M2-03 (marti/tls.rs).
﹣ lists channels in the envelope CloudTAK types against
    # TODO(M2-06): GET /Marti/api/groups/all is not served yet — channels land with M2-06 (marti/groups.rs).
﹣ takes a channel out of a device's active set and puts it back
    # TODO(M2-06): GET /Marti/api/groups/all is not served yet — channels land with M2-06 (marti/groups.rs).
﹣ lists contacts as a bare array
    # TODO(M2-06): GET /Marti/api/contacts/all is not served yet — contacts land with M2-06 (marti/contacts.rs).
﹣ lists client endpoints in the envelope, with a status node-tak understands
    # TODO(M2-06): GET /Marti/api/clientEndPoints is not served yet — it lands with M2-06 (marti/subscriptions.rs).
﹣ answers a ping with t-x-c-t-r over a mutually authenticated connection
    # TODO(M1-05): nothing is listening on the CoT stream port — the mutually authenticated
      listener lands with M1-05 (stream/listener_tls.rs).
﹣ lists stored content in the Resource envelope
    # TODO(M3): /Marti/sync/* and the files metadata API are not served yet — data packages land in M3.
﹣ stores a file and hands the same bytes back
    # TODO(M3): /Marti/sync/* and the files metadata API are not served yet — data packages land in M3.
﹣ lists missions in the Mission envelope
    # TODO(M4): the mission API is not served yet — Data Sync lands in M4 (plan.md milestone table).
﹣ creates, reads back and deletes a mission
    # TODO(M4): the mission API is not served yet — Data Sync lands in M4 (plan.md milestone table).
﹣ subscribes to a mission and reports the subscription
    # TODO(M4): the mission API is not served yet — Data Sync lands in M4 (plan.md milestone table).
ℹ tests 25
ℹ suites 0
ℹ pass 7
ℹ fail 0
ℹ cancelled 0
ℹ skipped 18
ℹ todo 0
ℹ duration_ms 978.171292
  exit 0

$ ~/go/bin/actionlint .github/workflows/rust.yml
.github/workflows/rust.yml:321:9: constant expression "false" in condition. remove the if: section [if-cond]
  (plus 9 pre-existing shellcheck notes on jobs this brief did not touch; the findings are
   identical, line for line, to `actionlint` on the committed version of this file — verified
   by diffing both outputs with the line numbers stripped)

$ ls /tmp/rustak-interop-node-tak-*
(nothing: the scratch directory, its database, its encryption key and its CA are removed on exit)
```

Prerequisites built first, in this order, as the suite's own error message says to:

```
$ cd rustak-ui && trunk build
$ cd .. && cargo build -p rustak-server
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```

## Notes for the orchestrator and the briefs that follow

- **The Marti scope must stay mounted ahead of `web/ui.rs`'s catch-all.** See "The probe had to
  learn about the single-page shell" above. M2-04 got this right — `GET /Marti/api/version` answers
  a version string rather than the shell, and this suite now asserts it on every run — but the trap
  is silent and re-openable, so M2-03, M2-06, M3 and M4 each need to keep their routes on the right
  side of it.
- **M2-03**: flipping `if: false` is the last step of that brief, not a follow-up. The job body is
  written and linted; the comment in `rust.yml` says precisely what to change, and the `ci` job's
  own comment says to fold the result back into its loop at the same time.
- **M2-06 and M4**: `groups.test.ts` asserts that `PUT /Marti/api/groups/active` changes the
  *device's* active state and that a re-list reflects it, which needs the caller to resolve to a
  device. Under a bearer token on the public listener there may not be one. If that turns out to be
  the wrong shape, the fix is to switch `groups.test.ts` and `contacts.test.ts` from `tokenClient`
  to `certificateClient` — one line each, and `src/client.ts` already has it.
- **Whoever adds `POST /users`**: see "The gap this suite cannot close".
- The suite adds three npm dependencies (`@tak-ps/node-tak`, `@tak-ps/node-cot`, `@tak-ps/xml-js`,
  all MIT) and three dev dependencies (`tsx`, `typescript`, `@types/node`). `@tak-ps/xml-js` is
  direct rather than transitive on purpose: `enrollment.test.ts` asserts the `tls/config` document
  against **CloudTAK's own parser**, so depending on it by name is the assertion.
- `npm ci` warns that `esbuild`'s and `fsevents`' install scripts were not run (npm 11's
  `allowScripts` gate). Harmless: `tsx` resolves esbuild's platform binary through
  `optionalDependencies`, and the suite runs from a clean `npm ci` — verified.
