# `interop/cloudtak` — the CloudTAK full-stack suite

This directory runs **CloudTAK itself** — its published container, its Postgres,
its browser UI — against a rustak built from this checkout, and asserts on what
CloudTAK says came back. It is the only suite in which rustak is not driven by a
library or a test client but by the whole application: CloudTAK's own
certificate handling, its own mission-package building, its own database and its
own opinions about what a TAK server owes it.

It complements [`interop/node-tak`](../node-tak/README.md) rather than repeating
it. node-tak proves the wire shapes are right for the libraries CloudTAK uses,
on every PR, in seconds. This proves CloudTAK *works*, nightly, in minutes — and
the failures it catches are the ones that only appear when something above the
library layer has an opinion.

```
interop/cloudtak/
  docker-compose.yml  Postgres + CloudTAK + rustak on one network
  src/
    run.ts            `npm run test:suite`: the stack, the steps, the report
    settings.ts       paths, ports, image pins and names — one source of truth
    compose.ts        is there a runtime, up, down, logs
    pki.ts            the test CA, the server certificate, a client CSR
    rustak.ts         the configuration the container is given
    session.ts        the cold start: administrator, operator, channel, password
    enroll.ts         tls/config + signClient/v2 — CloudTAK's admin certificate
    client.ts         talking to CloudTAK, with its error sentences kept
    api.ts            configuration, login and channels: builders and parsers
    missions.ts       Data Sync, contents, changes and packages: the same
    steps.ts          the order a run performs them in
    step-kit.ts       what a step is, and the helpers all of them use
    steps-setup.ts    configure the server, sign in, toggle a channel
    steps-datasync.ts the mission, the marker, the file, the changes, the package
    smoke.ts          the Playwright smoke of the UI
    surfaces.ts       the rustak surfaces a step needs, and the brief each waits on
  tests/              the builders and parsers, against fixtures, with no Docker
  fixtures/           hand-written CloudTAK answers — ours, not captures
```

## Running it

The Dockerfile packages a binary rather than compiling one, and the UI is
embedded into that binary at compile time, so both come before the image:

```bash
cd rustak-ui && trunk build --release
cd .. && cargo build --release -p rustak-server
mkdir -p dist && cp target/release/rustak dist/rustak
docker build -f rustak-server/Dockerfile -t rustak-interop-cloudtak:local .

cd interop/cloudtak
npm ci
npx playwright install --with-deps chromium   # only for the UI smoke
npm test
```

`npm test` is the unit tests and then the stack. The stack half brings the
compose project up, generates a test CA, bootstraps rustak through `/api/v1`,
configures CloudTAK against it and runs every step, then takes the stack down
again — including on failure, where it keeps the container logs and the
screenshots under `artifacts/` first.

| Docker | `RUSTAK_INTEROP_REQUIRE_DOCKER` | What happens |
|---|---|---|
| yes | anything | the stack is started and driven for real |
| no | unset or `1` | **the run fails, loudly**, naming what it needed |
| no | `0` | every step reports as a skip and the run passes |

The default is the opposite of [`interop/eud`](../eud/README.md)'s on purpose:
there is no probe-only mode worth having here, because every assertion is made
*through* CloudTAK. What a developer without Docker still gets is
`npm run test:unit` — 40 assertions on the request builders and the response
parsers, against the fixtures in `fixtures/`.

| Variable | Default | What it moves |
|---|---|---|
| `RUSTAK_INTEROP_REQUIRE_DOCKER` | `1` | whether a missing runtime fails the run |
| `RUSTAK_INTEROP_RUSTAK_IMAGE` | `rustak-interop-cloudtak:local` | the rustak image the stack runs |
| `RUSTAK_INTEROP_CLOUDTAK_TAG` | `v13.89.0` | the published CloudTAK image |
| `RUSTAK_INTEROP_WEBTAK_PORT` / `_MARTI_PORT` / `_STREAM_PORT` | `8446` / `8443` / `8089` | rustak's published ports |
| `RUSTAK_INTEROP_CLOUDTAK_PORT` | `5000` | CloudTAK's published port |
| `RUSTAK_INTEROP_ARTIFACTS` | `artifacts/` | where screenshots and logs are kept |
| `RUSTAK_INTEROP_DOCKER` | `docker` | the container runtime to shell out to |

## The three URLs, and the trust asymmetry that is the point

CloudTAK stores one server as three independent base URLs, and authenticates
differently against each (`compat/cloudtak.md` §1, §3). The stack configures
them as the *containers* see them:

| CloudTAK field | Value here | Credential | Verifies the chain? |
|---|---|---|---|
| `url` | `ssl://rustak:8089` | the admin client certificate | no |
| `api` | `https://rustak:8443` | the same certificate (mTLS) | no |
| `webtak` | `https://rustak:8446` | username + client password | **yes, with no override** |

That last row is why `[web.public.tls] mode = "files"` here rather than the
`internal` the other suites use. CloudTAK's OAuth and enrolment calls go through
Node's `undici` with full system-CA verification and no way to turn it off, so
an internal certificate on `webtak` breaks login outright unless the operator
mounts the authority into the container and names it in `NODE_EXTRA_CA_CERTS` —
CloudTAK issue #983, and the most common CloudTAK bring-up failure there is.
`src/pki.ts` builds that CA, the compose file mounts it, and a run that gets as
far as signing in has proved the documented workaround actually works.

rustak's *internal* CA still exists inside the same container and still issues
every client certificate; only the public listener's server certificate comes
from the generated one. Both have to work at once, which is why the generated
pair lives in `/data/tls/` and not in `/data/pki/`.

## What is and is not in the stack

CloudTAK's own `docker-compose.yml` runs seven services. This one runs three,
and the difference is deliberate:

- **`postgis`** — CloudTAK's database, on tmpfs so every run starts empty. An
  empty database is what makes the unconfigured-server bootstrap repeatable:
  `PATCH /api/server` only accepts a username and password, and only makes the
  first caller a system administrator, while no server is configured.
- **`cloudtak`** — the published `ghcr.io/dfpc-coe/cloudtak-api` image, in
  `CLOUDTAK_Server_Mode=both` so one process serves the API and the hub.
- **`rustak`** — the image built above.

Left out: **MinIO** (`ASSET_BUCKET` is only required when `StackName` is set,
and nothing this suite exercises reads CloudTAK's object store — the file step
streams bytes straight through to rustak, and the package step builds the
package in CloudTAK's own temporary directory), **pmtiles** (basemap tiles; the
map renders without them), **events** and **retention** (scheduled workers), and
**media** (video, which `compat/cloudtak.md` §8 says to stub rather than
implement). Adding any of them would make a nightly run slower and would not add
an assertion.

The image is published for `linux/amd64` only, which is what the nightly runner
is. On an arm64 machine Docker will emulate it; expect a slow first start.

## What a run asserts

| Step | Drives | Proves about rustak |
|---|---|---|
| `configure-server` | `PATCH /api/server` | `GET /files/api/config` answers an integer `uploadSizeLimit` over mTLS, `/oauth/token` grants a password login, and `/Marti/api/tls/signClient/v2` issues CloudTAK's own user certificate — the three calls CloudTAK makes before it will save a connection at all |
| `login` | `POST /api/login`, `GET /api/login` | the password grant returns a JWT whose `sub` CloudTAK can take for the account, and the certificate it enrolled is accepted by the Marti listener |
| `channels` | `GET`/`PUT /api/marti/group` | `/Marti/api/groups/all` and the active-state write round-trip through node-tak's whole-row update |
| `data-sync` | `POST /api/marti/mission`, `GET …/{guid}` | a mission is created and reads back **by guid**, which is the form CloudTAK routes on |
| `marker` | `PUT /api/marti/missions/{guid}/cot` | a mission data package uploads *and* the mission read-back confirms every feature — CloudTAK checks, so a `200` here is a real round-trip |
| `file` | `POST …/{guid}/upload`, `GET …/{guid}` | Enterprise Sync stores the bytes and `PUT …/contents` attaches the hash, which then appears in the mission |
| `changes` | `GET …/{guid}/changes` | the change log records `ADD_CONTENT` for the marker's uid and the file's hash |
| `package` | `PUT /api/marti/package`, `GET /api/marti/package` | `/Marti/sync/missionupload` accepts a package and `/Marti/sync/search` lists it |
| `ui-smoke` | Playwright | the login page renders, the map canvas initialises, and the Data Sync menu lists the mission the API created |

A step whose rustak surface is not served yet **skips with the brief that will
serve it** (`src/surfaces.ts`), the same convention the other two suites use, so
the job keeps saying what is missing instead of hiding it.

## Traps worth knowing

- **The bootstrap reads a file out of the container.** rustak writes a one-time
  setup token into its data directory and that is the only route to a first
  administrator, so `/data` is a bind mount and the container runs as the
  invoking user. A run that leaves root-owned files behind is a run whose
  `user:` mapping did not take.
- **`[server] base_url` names `localhost`, not `rustak`.** WebAuthn binds a
  passkey to a domain and the bootstrap registers one from the *host*;
  `localhost` is also the one relying party rustak accepts on any port, so the
  published port need not match the bound one. CloudTAK is unaffected: it
  reaches the same listener at `https://rustak:8446`.
- **CloudTAK reports rustak's failures as its own.** A mission call rustak
  refuses comes back as a CloudTAK `400` whose `message` is the sentence worth
  reading, which is why `src/client.ts` unwraps it instead of reporting a status
  code.
- **A JSON *string* where an object belongs is the `Content-Type` bug.** If
  rustak ever emits `application/json; charset=UTF-8`, node-tak's strict header
  comparison misses, CloudTAK serialises the raw text back out, and the route
  looks like it worked. `expectObject` in `src/api.ts` names it; `tests/api.test.ts`
  asserts that it does.
