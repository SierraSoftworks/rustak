# Deployment

How to configure, run, and back up a rustak server: as a single binary, as a
Docker container, and as a systemd service.

> **Status.** This document describes the deployment shape rustak is built
> for, which is already real in the configuration schema, the `--check`
> validator, the Dockerfile and the CI pipeline that builds it. It is **not**
> yet a description of a server you can bring up end to end: the process
> start-up that opens the database, brings up the listeners and runs the job
> host (`rustak-server/src/{main.rs,lib.rs,runtime.rs}`) has no status file
> under `.claude/plan/status/` yet, and today `rustak --config config.toml`
> (without `--check`) exits immediately with "this build has no run loop
> yet". Treat the walkthroughs below as the target, not a claim that they
> currently work — the sections most affected are called out inline.
>
> Within the config this document walks through, some sections describe
> features that land in later milestones rather than M0:
> - **ACME (`[acme]`, `[web.public.tls] mode = "acme"`) is planned — M2.**
>   Setting it today is refused with a `Kind::User` error explaining that ACME
>   is not available yet (verified in `.claude/plan/status/M0-11-web-api-auth.md`).
> - **The CoT stream listener (`:8089`) is planned — M1.** `[stream.tls]`
>   parses and validates, but nothing binds it yet.
> - **The Marti API and ATAK/CloudTAK enrollment (`/Marti/**`,
>   `/Marti/api/tls/*`) are planned — M2 through M4.** The listener table
>   below names the port each will use; none of those routes exist yet.
>
> What *is* verified, end to end, by an automated test suite today: the
> configuration schema and `--check` (`.claude/plan/status/M0-06-server-config.md`),
> the SQLite/crypto/content-store/CA/JWT foundations
> (`M0-07`, `M0-08`, `M0-10`), and — reachable only in-process by the test
> suite, not yet by a running binary — the admin web server, `/api/v1`, OIDC
> and passkey sign-in, and the setup wizard API
> (`.claude/plan/status/M0-11-web-api-auth.md`).

## The binary and the config file

rustak is one binary (`rustak`) plus one TOML file. There is no separate
migration step, no external cache, and no second process to keep alive next
to it — everything it needs lives under one `data_dir`.

```sh
rustak --config config.toml
```

Copy [`config.example.toml`](../config.example.toml) as a starting point: it
documents **every** key, with its default, inline — unknown or misplaced keys
are refused (`#[serde(deny_unknown_fields)]` on every config struct, per
[`conventions.md`](../.claude/plan/conventions.md)), so the example file
doubles as a schema you can diff a real config against.

Before you point a real config at a real data directory, validate it:

```sh
rustak --config config.toml --check
```

This loads and validates the file and exits — 0 and a one-line summary when
it would start, 1 with the failure explained when it would not — without
touching the data directory or bringing up telemetry, so it is safe to run
repeatedly in a deployment pipeline against a candidate file, on a machine
that is not the server. It is the one code path in this document that is
fully wired and tested today.

### Walking the config file

The sections below follow `config.example.toml` top to bottom; see that file
for the full comment on every key.

- **`[server]`** — `name` (shown on `/Marti/api/version` and the admin UI),
  `domains` (the canonical first entry becomes the enrollment-QR host, the
  internal server certificate's subject, and the default token issuer —
  changing it invalidates what already-enrolled devices hold), `base_url`
  (only needed when a proxy publishes rustak on a different port; also the
  WebAuthn relying party, so it must be a **name**, not an address — a
  console reached at `https://192.0.2.10` cannot register a passkey at all),
  `trust_proxy` (only turn on behind a proxy that actually sets
  `X-Forwarded-*`; the credential rate limiter keys on the address it
  reports), `data_dir` (default `./data` — everything below lives under it).
- **`[storage]`** — where the database, content store and stream segments sit
  relative to `data_dir`, plus the SQLite reader-pool size, busy timeout and
  WAL checkpoint interval.
- **`[web.public]`** / **`[web.public.tls]`** — the browser- and
  enrollment-facing listener: the admin UI, `/api/v1`, `/oauth/*`,
  `/login/*`, `/Marti/api/tls/*`, and the full Marti API for CloudTAK's
  `webtak` role. `listen` defaults to `[":8446"]`; add `":443"` for browsers
  that will not be told a port number, and for ACME's `tls-alpn-01`
  challenge. `tls.mode` is `internal` (rustak's own CA — see **TLS modes**
  below), `files`, `acme` (**planned — M2**), or `none` (refused unless
  `allow_insecure_http = true`, which is meant for development and the e2e
  suite, not a real deployment — it serves credentials, tokens and
  certificate enrollment in the clear).
- **`[web.marti]`** — the mutually authenticated Marti listener, `:8443` by
  default. `client_cert = "required"` is the only accepted value: unlike TAK
  Server, rustak does not accept a bearer token here instead of a
  certificate, because a device without one has not enrolled.
- **`[stream.tls]`** — the CoT streaming listener, `:8089` by default, always
  client-cert-required. There is deliberately no plaintext `[stream.tcp]`
  section and no anonymous access — see `plan.md` → Decisions → "Secure by
  default".
- **`[auth]`** — token lifetimes, `user_acl`/`admin_acl` (filt-rs
  expressions over claims; **both default to denying everybody** — an
  installation that configures an identity provider and forgets these admits
  nobody, not the provider's whole directory; the first administrator is
  instead authorised through the setup wizard, so a fresh installation is
  always administrable), `enrollment_token_ttl`, and the opt-in
  `client_passwords_enabled`/`client_password_ttl` compatibility credential
  CloudTAK needs until it speaks OIDC.
- **`[auth.oidc]`** — federating sign-in to an identity provider; entirely
  optional. Group-claim mapping (`_READ`/`_WRITE` suffixes) is documented
  inline.
- **`[pki]`** — the internal certificate authority: subject shape
  (`name_entries` / `organization`), key type, validities, and
  `require_known_cert` (on by default, so deleting a certificate's record
  stops it working immediately, not just at its next renewal).
- **`[acme]`** — **planned — M2.** The config keys exist and validate today
  (directory, contact, challenge type, `accept_tos`), but setting
  `[web.public.tls] mode = "acme"` is refused with an explanatory error until
  M2 lands.
- **`[retention]`** — how long CoT history, audit log entries and archived
  missions are kept, as an age, a row/entry cap, or both.

### Validating and reloading

- `rustak --config config.toml --check` — validate without starting (see
  above).
- Every value may be written as `"${{ env.NAME }}"`, substituted from the
  process environment (or from the file passed to `--env`) before the TOML is
  parsed, so secrets never need to live in a file you commit or attach to a
  support ticket. An expression whose variable is unset is refused **by
  name** at start-up — never silently sent as literal `${{ env.… }}` text or
  turned into an empty credential.
- There is no SIGHUP reload. Changing the file means restarting the process.

## TLS modes

| Mode | Certificate comes from | Needs |
|---|---|---|
| `internal` (default) | rustak's own CA, reissued automatically when the covered names, the CA, or the renewal window change | Nothing external. Browsers will not trust it until the CA (`<data_dir>/pki/ca.crt`) is installed, and devices get it automatically through enrollment. This is the LAN-only story. |
| `files` | `cert_file` / `key_file`, PEM, reloaded when they change | A certificate from somewhere else (a public CA, your own PKI). |
| `acme` | Ordered from an ACME authority (Let's Encrypt by default) | **Planned — M2.** Public DNS for the domain, and either `:443` bound (`tls-alpn-01`) or a plaintext `plain_bind` on `:80` (`http-01`). |
| `none` | Nothing — plaintext HTTP | `allow_insecure_http = true` as a second, deliberate statement. Development and the e2e suite only. |

## Running it

### As a native process

```sh
rustak --config /etc/rustak/config.toml
```

The `data_dir` (default `./data`, set it to somewhere durable in production —
see **Backup** below) is created on first start if it does not exist. On an
installation with no administrator yet, start-up writes a one-time setup
token to `<data_dir>/setup-token` (mode `0600`) and the log names the path;
open `https://<host>:8446/setup` and enter it to create the first
administrator and register their passkey. See **First-run setup and sign-in**
below — the HTTP side of this flow is tested end to end
(`.claude/plan/status/M0-11-web-api-auth.md`); reaching it through a running
`rustak` process needs the bootstrap wiring called out at the top of this
document.

### As a container

The published image is `ghcr.io/sierrasoftworks/rustak` — a multi-arch
(`linux/amd64` + `linux/arm64`) build from `rustak-server/Dockerfile`
(`ubuntu:24.04` + `ca-certificates`, nothing else). The workflow that builds
and publishes it (`docker-build` / `docker-publish` in
`.github/workflows/rust.yml`, on every push to `main` and on every release —
see [`docs/ci.md`](ci.md)) exists and has been checked structurally, but a
green, published run has not yet been confirmed in this session — see the
status note at the top of this document.

```sh
docker run -d \
  --name rustak \
  -v "$(pwd)/data:/data" \
  -p 8446:8446 \
  -p 443:443 \
  -p 8443:8443 \
  -p 8089:8089 \
  ghcr.io/sierrasoftworks/rustak:latest
```

`config.toml` is read from `/data/config.toml` (the image's `ENTRYPOINT` is
`rustak --config /data/config.toml`), so put your copy of
`config.example.toml` at `./data/config.toml` on the host before the first
start. The image also `EXPOSE`s `8087`, matching the port table in
`plan.md` → Architecture; nothing binds it (rustak has no plaintext stream
listener by design), so it does not need publishing.

A minimal `docker-compose.yml`:

```yaml
services:
  rustak:
    image: ghcr.io/sierrasoftworks/rustak:latest
    restart: unless-stopped
    volumes:
      - ./data:/data
    ports:
      - "8446:8446"   # admin UI, /api/v1, oauth/login, enrollment, Marti (CloudTAK webtak)
      - "443:443"     # optional: for browsers that assume 443, and ACME tls-alpn-01
      - "8443:8443"   # Marti mTLS
      - "8089:8089"   # CoT stream (planned — M1)
```

#### With CloudTAK

CloudTAK stores one `server` record with three independent base URLs rather
than assuming they share a host — matching rustak's three listeners exactly
(`.claude/plan/compat/cloudtak.md` § "The three-URL model"):

| CloudTAK field | Points at | rustak listener |
|---|---|---|
| `url` | `ssl://rustak:8089` | CoT stream |
| `api` | `https://rustak:8443` | Marti, mTLS |
| `webtak` | `https://rustak:8446` | public listener: OAuth + enrollment |

The important trap: CloudTAK's `api` and stream connections tolerate an
untrusted certificate, but its `webtak` calls go through Node's `undici`
fetch with **full system-CA verification and no per-connection override**.
An `internal`-mode rustak (the default) is self-signed from its own CA, so
the CloudTAK container needs that CA added via `NODE_EXTRA_CA_CERTS`, or
`webtak` breaks outright — this is documented as the single most common
CloudTAK bring-up failure (`compat/cloudtak.md` § "TLS trust is asymmetric").

```yaml
services:
  rustak:
    image: ghcr.io/sierrasoftworks/rustak:latest
    restart: unless-stopped
    volumes:
      - ./data:/data
    ports:
      - "8446:8446"
      - "8443:8443"
      - "8089:8089"

  cloudtak-api:
    # See CloudTAK's own deployment documentation for the image, the
    # Postgres service it needs, and how it is configured; only the trust
    # relationship with rustak is rustak's concern here.
    environment:
      NODE_EXTRA_CA_CERTS: /certs/rustak-ca.crt
    volumes:
      - ./data/pki/ca.crt:/certs/rustak-ca.crt:ro
    depends_on:
      - rustak
```

After both containers are up, register the server in CloudTAK's own setup
wizard (or its `PATCH /api/server`) with the three URLs from the table above.
The authoritative version of this compose stack — Postgres, CloudTAK's API,
and the assertions that it actually works — lands as `interop/cloudtak/` in
M4 (`docs/interop.md`); this snippet is the deployment-facing subset of the
same shape.

### As a systemd service

```ini
# /etc/systemd/system/rustak.service
[Unit]
Description=rustak TAK server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=rustak
Group=rustak
ExecStart=/usr/local/bin/rustak --config /etc/rustak/config.toml
Restart=on-failure
RestartSec=5s

# Binding :443 (optional, for browsers/ACME) needs this; :8446/:8443/:8089
# do not.
AmbientCapabilities=CAP_NET_BIND_SERVICE

# Hardening. /var/lib/rustak is [server] data_dir; nothing else needs to be
# writable.
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/var/lib/rustak

[Install]
WantedBy=multi-user.target
```

```sh
useradd --system --home /var/lib/rustak --shell /usr/sbin/nologin rustak
mkdir -p /var/lib/rustak /etc/rustak
cp config.example.toml /etc/rustak/config.toml   # then edit it
chown -R rustak:rustak /var/lib/rustak
systemctl daemon-reload
systemctl enable --now rustak
```

## First-run setup and sign-in

There are no local user passwords anywhere in rustak. The first administrator
is created by a one-time **setup token** (written to
`<data_dir>/setup-token`, mode `0600`, at start-up on an installation with no
administrator yet), which is spent to register that administrator's
**passkey** (WebAuthn) or to configure an OIDC provider — never both are
required. Every `/api/v1/setup/*` route answers `410 Gone` once the wizard
has completed, and completion is tracked independently of whether an
administrator account still exists, so emptying the users table can never
reopen the wizard as a backdoor.

Day to day, signing in to the admin UI is either:

- **Passkey (WebAuthn)** — the default for a standalone installation. A
  passkey is bound to the exact host name the browser used
  (`[server] base_url`, falling back to the first `[server] domains` entry),
  which is why that value has to be a name a browser will actually use, not
  an IP address.

  rustak registers passkeys as **discoverable** credentials
  (`residentKey: "required"`, `userVerification: "required"`), because the
  sign-in prompt never asks for a username — the authenticator says which
  account it holds, so the prompt cannot be used to find out which accounts
  exist. Practically that means the authenticator has to be able to *store*
  the credential and to verify the user: a platform authenticator (Touch ID,
  Windows Hello, Android, iCloud Keychain) always can, and a security key
  needs a PIN set and a free credential slot. A key with no slots left refuses
  the registration outright rather than producing a passkey that could not
  sign in afterwards.

  The sign-in page also offers **"Sign in with a username instead"**, which
  runs the ceremony against one named account's credentials. It is there for a
  passkey the browser cannot offer on its own — one registered against another
  server, one from an authenticator that ignored `residentKey`, or one
  registered before this behaviour changed. A username with no passkey and a
  username that does not exist are refused identically.
- **OIDC** — federated sign-in to an external identity provider, configured
  under `[auth.oidc]`. Group membership comes from the provider's claims;
  users mint their own per-device enrollment tokens and (opt-in) client
  passwords from the admin UI afterwards.

ATAK devices never see either of these: they enrol with a **one-time
enrollment token**, scanned as a QR code or typed in, which is consumed the
moment it issues a certificate — see [`docs/plugins.md`](plugins.md) and
`.claude/plan/compat/enrollment.md` for the wire detail (**planned — M2**).

This whole flow is exercised end to end by `rustak-server`'s test suite —
metadata, the OIDC redirect round-trip, passkey registration and login
including a cloned-authenticator counter check, and every setup-wizard gating
rule — see `.claude/plan/status/M0-11-web-api-auth.md`. What is not yet
proven is reaching it through an actually-running `rustak` process (the
bootstrap gap called out at the top of this document).

## Backup

Everything rustak needs lives under `[server] data_dir` (default `./data`).
Stop the process (or use SQLite's own online-backup mechanism if you cannot)
and copy these four things together — they are not independently
restorable, because certificates, sealed secrets and content are
cross-referenced by row IDs and hashes that only make sense as a set:

| Path (relative to `data_dir`) | What it is |
|---|---|
| `rustak.sqlite` (+ `-wal`/`-shm` if present) | State and metadata: users, devices, certificates, credentials, passkeys, groups, missions, settings, audit log, and the index rows for the two stores below. `[storage] database` overrides the name. |
| `rustak.sqlite.key` | The AES-256 key that seals every secret stored in the database (device passwords, OIDC client secret, ACME account key, …). **Back this up with the database, not separately** — a database without its key is unrecoverable, and a key without its database is useless. |
| `pki/` | The exported CA certificate (`ca.crt`) and any files written for manual enrollment. The CA's private key is sealed *in* the database, not stored here in the clear — this directory alone is not enough to reissue certificates as your CA. |
| `content/` | The content-addressed blob store (data packages, attachments, profile files), named by sha256. `[storage] content_dir` overrides the location. |
| `streams/` | Append-only CoT history segment files (**planned — M1**, once the stream listener writes them). `[storage] streams_dir` overrides the location. |

A config file itself (`config.toml`) is worth keeping in version control or
your configuration-management tool rather than in this backup set — it holds
no secrets when `${{ env.X }}` is used correctly, and restoring it from the
database backup would undo any host-specific settings (listen addresses,
`trust_proxy`) you have layered on top of it.

## See also

- [`config.example.toml`](../config.example.toml) — every configuration key,
  documented with its default.
- [`docs/ci.md`](ci.md) — how the binary and the container image are built,
  tested and published.
- [`docs/interop.md`](interop.md) — the automated suites that prove
  compatibility with ATAK and CloudTAK, including the full CloudTAK
  docker-compose stack this document's CloudTAK section previews.
- [`docs/plugins.md`](plugins.md) — running a sidecar alongside rustak.
- [`.claude/plan/plan.md`](../.claude/plan/plan.md) → Architecture →
  Listeners, Decisions — the design this document describes.
