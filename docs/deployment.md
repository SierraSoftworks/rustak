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
  reports), `data_dir` (default `./data` — everything below lives under it),
  `shutdown_timeout` (default `"8s"`, at most `"60s"` — how long open
  connections are given to close on the way out; see **Stopping cleanly**
  below, and raise your orchestrator's grace period with it).
- **`[storage]`** — where the database, content store and stream segments sit
  relative to `data_dir`, plus the SQLite reader-pool size, busy timeout and
  WAL checkpoint interval.
- **`[web.public]`** / **`[web.public.tls]`** — the browser- and
  enrollment-facing listener: the admin UI, `/api/v1`, `/oauth/*`,
  `/login/*`, `/Marti/api/tls/*`, and the full Marti API for CloudTAK's
  `webtak` role. `listen` defaults to `[":8446"]`; add `":443"` for browsers
  that will not be told a port number, and for ACME's `tls-alpn-01`
  challenge. `tls.mode` is `internal` (rustak's own CA — see **TLS modes**
  below), `files`, `acme` (see **ACME certificates** below), or `none` (refused unless
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
- **`[acme]`** — where a publicly trusted certificate for `[web.public]` is
  ordered from, how control of the names is proved, and how long before expiry
  it is replaced. Used only when `[web.public.tls] mode = "acme"`; the two
  switches must agree, and `--check` refuses a file where they do not. See
  **ACME certificates** below.
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
| `acme` | Ordered from an ACME authority (Let's Encrypt by default), renewed automatically and swapped in without a restart | Public DNS for every name, and either `:443` bound (`tls-alpn-01`) or port 80 reaching the public listener (`http-01`). |
| `none` | Nothing — plaintext HTTP | `allow_insecure_http = true` as a second, deliberate statement. Development and the e2e suite only. |

## ACME certificates

`[web.public.tls] mode = "acme"` plus `[acme] enabled = true` makes rustak
obtain the public listener's certificate from a certificate authority and keep
it current. Nothing else changes: the Marti and stream listeners still present
certificates from rustak's own CA, because their clients are devices rustak
enrolled and handed a truststore to.

```toml
[server]
domains = ["tak.example.com"]        # what the certificate is ordered for

[web.public]
listen = ["0.0.0.0:443", "0.0.0.0:8446"]

[web.public.tls]
mode = "acme"

[acme]
enabled = true
directory = "letsencrypt"            # or "letsencrypt-staging", or a URL
contact = "ops@example.com"          # where expiry warnings go
accept_tos = true                    # rustak will not agree on your behalf
challenge = "tls-alpn-01"
renew_before = "30d"
```

### What happens, and when

1. **First start.** The listener binds with a certificate from rustak's own CA
   so that it is answering at all — the first order cannot be validated until
   it is. A browser will warn for as long as that takes. The log says so.
2. **The order.** The renewal job runs immediately at start-up and hourly
   afterwards. It registers an account the first time (the account key is
   sealed in the database, and is backed up with it — see **Backups**), places
   an order, answers the challenge, and downloads the chain.
3. **The swap.** The issued key and chain are sealed into `acme_certificates`
   and installed into the listener's certificate resolver. Existing
   connections keep the certificate they negotiated with; every new handshake
   gets the new one. **There is no restart.**
4. **Renewal.** Every hour the job compares the expiry against `renew_before`
   and orders again when it is inside it. A failed order is retried after an
   hour, then four, then daily, so a name whose DNS is not ready yet cannot
   spend the account's rate-limit allowance.

### Which port has to be open

| `challenge` | Answered on | What must be true |
|---|---|---|
| `tls-alpn-01` (default) | port **443**, inside the TLS handshake | `":443"` is in `[web.public] listen`. Needs no plaintext port. |
| `http-01` | port **80**, over plaintext HTTP, at `/.well-known/acme-challenge/{token}` | Port 80 reaches the public listener. rustak serves that path on every `[web.public]` binding; it does **not** yet bind `[web.public] plain_bind` itself, so an `http-01` deployment needs a proxy forwarding `:80` to a `[web.public] listen` address, or `":80"` in that list. |

`--check` refuses a combination that could never complete — `tls-alpn-01` with
nothing on 443, `http-01` with no plaintext port — and refuses a name no public
authority could issue for, such as `tak.lan`, `localhost` or an IP address.
Binding a port below 1024 needs `CAP_NET_BIND_SERVICE` (the systemd unit below
grants it) or a proxy in front.

### Watching it, and asking for a renewal

- `GET /api/v1/settings/tls` (administrator) — where the certificate came
  from, what it covers, when it expires, when it will be renewed, and, if the
  last order failed, how many attempts have failed and what the authority
  said.
- `POST /api/v1/settings/tls/renew` (administrator) — queue an order now,
  ignoring the schedule and the back-off. Answers `202` with the status as it
  stands; poll the `GET` to watch it change. Repeated calls collapse onto one
  queued order, because every one of them spends real rate limit.
- The audit log carries `acme.issued` and `acme.renew.failed` under the `pki`
  category.

### Manual check against staging before production

Let's Encrypt's production rate limits are unforgiving (five failed
validations per account per hour, and a weekly cap per registered domain), and
they are not something a test suite may exercise. The automated suite drives
the real `instant-acme` client against a mock directory
(`rustak-server/tests/acme_directory.rs`); the following is the checklist for
the one thing it cannot prove — that a real authority can reach this server.

Run it on the machine that will be production, against a **staging** directory,
before the first production order:

1. Point the public DNS records for every name in `[acme] domains` at the host.
2. Set `directory = "letsencrypt-staging"`, `accept_tos = true`, and a real
   `contact`. Run `rustak --config config.toml --check`; it must pass.
3. Start rustak. The log should say the listener bound with an internally
   issued certificate, then, within a minute, that a certificate was issued.
4. `curl -s https://tak.example.com/api/v1/health` from **outside** the
   network. It fails on trust — staging's roots are not in any trust store —
   and `openssl s_client -connect tak.example.com:443 -servername
   tak.example.com </dev/null 2>/dev/null | openssl x509 -noout -issuer
   -subject -dates` must show a `(STAGING)` issuer and your names.
5. Check `GET /api/v1/settings/tls`: `state` is `valid`, `domains` is what you
   asked for, `renews_at` is about 60 days out for a 90-day certificate.
6. `POST /api/v1/settings/tls/renew`, wait a minute, and confirm `not_after`
   moved. This proves the swap works without a restart.
7. Only then change `directory` to `"letsencrypt"`, delete the staging row —
   `DELETE FROM acme_certificates;` with the server stopped — and restart. The
   account rows are per directory, so the staging account is left alone.

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
    # `[server] shutdown_timeout` (8s) plus the two-second WAL checkpoint after
    # it. 10s is also the default, so this line only matters if you raise the
    # one in config.toml — see **Stopping cleanly** below.
    stop_grace_period: 10s
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

# `[server] shutdown_timeout` (8s) plus the two-second WAL checkpoint that runs
# after it, with a little room. systemd's own default is 90s, so this only
# tightens things; raise both together if you raise the one in config.toml.
# See **Stopping cleanly** below.
TimeoutStopSec=15

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

## Stopping cleanly

`SIGTERM` (or Ctrl-C) stops rustak in two bounded steps, and an orchestrator
has to allow for both of them:

| Step | How long | Configurable |
|---|---|---|
| Drain — the public, Marti and CoT stream listeners stop accepting and let open connections close | `[server] shutdown_timeout`, **8s** by default, at most 60s | yes |
| Checkpoint — `PRAGMA optimize` and a `wal_checkpoint(TRUNCATE)`, so the data directory is left with a database and an empty log | **2s**, fixed | no |

So a stop takes up to `shutdown_timeout + 2s`, and **whatever kills the process
after a grace period has to allow at least that much**. `docker stop` allows ten
seconds, which is exactly what the defaults add up to; that is where the 8 comes
from. Raise one and you have to raise the other:

```sh
# config.toml: [server] shutdown_timeout = "25s"
docker stop -t 27 rustak
```

...or `stop_grace_period: 27s` in compose, or `TimeoutStopSec=27` in the unit
file. Get it wrong and the part that gets killed is the checkpoint, which is the
one step that has anything to do with your data: the next start then has a
write-ahead log to fold back in, and a backup taken from the volume in the
meantime is `rustak.sqlite` **plus** `rustak.sqlite-wal` rather than a single
complete file (see **Backup**).

Two other things worth knowing:

- **A second signal ends the drain early.** Connections that are still open are
  cut off, but the checkpoint still runs — pressing Ctrl-C twice is a way to
  skip the waiting, not a way to skip the durability. A *third* exits at once
  and is the only thing short of `SIGKILL` that skips the checkpoint.
- **Exit status.** A stop that drained exits **0**; one that was cut short by a
  second or third signal exits **130**, after the checkpoint. If your unit file
  uses `Restart=on-failure`, list `130` in `SuccessExitStatus=` so that an
  operator who stopped the server impatiently does not have it restarted.
- **An idle browser tab can hold the drain open for the whole budget.** HTTP/2
  keep-alive connections are not requests in flight, but they are connections,
  and the listener waits for them. That is what the budget is for; there is
  nothing to fix.

The drain is logged (`Draining the public listener.`, `The CoT stream listener
has stopped.`), and so is a budget that ran out — if you see *"Connections were
still open when the shutdown budget ran out"* on every stop, raise
`shutdown_timeout` and the orchestrator's grace period together.

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

  The supported signature algorithms are **ES256** (`-7`), **RS256** (`-257`)
  and **EdDSA** (`-8`); a credential offering anything else is refused at
  registration rather than stored and found wanting later. A signature counter
  that fails to move forward is refused — but only once it has moved at all,
  because an authenticator that does not implement the counter reports zero
  for ever and refusing those would lock out every such device.

  > **Upgrading from a build before the pure-Rust WebAuthn change**: passkeys
  > registered by an earlier version cannot be read by this one. The table and
  > its columns are unchanged, but the stored public key is now in the new
  > verifier's own encoding rather than the old library's. An affected sign-in
  > is refused the way every other unusable passkey is, and the log says the
  > credential "may have been written by a different version of rustak". The
  > cure is to sign in by another route — an identity provider, or another
  > administrator — and register the passkey again; an installation whose only
  > administrator has only such a passkey has to delete the `passkeys` rows by
  > hand and run the first-run wizard again.
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

## Browser single sign-on (`/login/*` and `/oauth/authorize`)

Beside the admin UI's own popup sign-in, rustak serves the browser flow TAK
clients expect, in TAK Server's own shapes. Two audiences use it:

- **A WebTAK-style page** sends somebody to `GET /login/auth`, rustak sends
  them to the identity provider configured under `[auth.oidc]`, and they come
  back with a session in TAK's chunked `access_token_0`, `access_token_1`, …
  cookies. `GET /token/access` then hands the page that token to present
  elsewhere (turn it off with `[auth] allow_access_token_retrieval = false`).
- **An OAuth2 client of its own** — anything that wants rustak to *be* its
  identity provider — uses `GET /oauth/authorize` and
  `POST /oauth/token` with `grant_type=authorization_code`.

### Registering a client

Nothing may use the authorization-code flow until it is registered:

```toml
[auth.oauth]
clients = [
  { id = "webtak", redirect_uris = ["https://tak.example.com/login/redirect.html"] },
]
```

`redirect_uris` are compared **byte for byte**, never as a prefix, so each one
has to be exactly what the client sends — including its query string, if it has
one. A URI that is not absolute, carries a fragment, or would carry a code over
plain `http://` to anything but a loopback host is refused when the
configuration loads, not when somebody tries to sign in.

Every client **must** send `code_challenge` with `code_challenge_method=S256`;
a request without one is refused, and `plain` is never accepted. The code is
single-use, expires in ten minutes, and is bound to the client, the redirect URI
and that proof key — so a code lifted from an address bar, a `Referer` or a
proxy log buys nothing.

`public = true` (the default) says the client keeps no secret, which is every
browser and mobile client. It is recorded but not yet acted on: rustak accepts
no client secret, so proof key for code exchange is required whatever it says,
and `public = false` changes nothing today. It exists so that adding client
authentication later is not a change to the shape of your config file.

There is no consent screen. Every client here was registered in this server's
own configuration file by an operator, which makes them all first-party.

### The endpoints

| Path | What it does |
|---|---|
| `GET /oauth/authorize` | `response_type=code` only. Issues a code to a signed-in browser, or sends one that is not signed in through `[auth.oidc]` first. |
| `POST /oauth/token` | `grant_type=authorization_code`, beside the existing `password` and `refresh_token` grants. Needs `code`, `redirect_uri`, `client_id` and `code_verifier`. |
| `GET /login/auth` | Starts a sign-in with no OAuth2 client behind it. `?returnTo=/some/path` — a path on this site only. |
| `GET /login/redirect` | Where the identity provider returns the browser. |
| `GET /login/authserver` | The sign-in button's name (`[auth.oidc] display_name`), or `404` when no provider is configured. |
| `GET /login/.well-known/openid-configuration` | The **upstream** provider's `authorization_endpoint` and `token_endpoint`, in TAK Server's bare shape. Not rustak's own discovery document. |
| `GET /token/access` | The caller's own access token. |
| `GET\|POST /logout` | Revokes the session and clears its cookies. `204`. |

All of them are served on `[web.public]` only — never on the mutually
authenticated `[web.marti]` listener, where a device already holds a stronger
credential than any cookie and a browser has no business.

### Where a cookie counts as a credential

`access_token_N` cookies authenticate `/login/*`, `/logout`, `/token/access`,
`/oauth/authorize` and the TAK surface (`/Marti/**`, `/files/api/**`). They are
**never** accepted on `/api/v1`, which reads the `Authorization` header and
nothing else — that is what keeps the admin API free of any cross-site request
forgery surface. Every cookie is `HttpOnly`, `Secure`, `SameSite=Lax` and
`Path=/`; the short-lived `state` cookie is scoped to `/login`.

`Secure` is set unconditionally, so this flow needs TLS. That is not a
restriction in practice — `[web.public]` refuses to serve plaintext unless an
operator says so twice — but a development instance running with
`allow_insecure_http` will find that browsers drop the cookies.

### CloudTAK today

CloudTAK 13.90 has **no OIDC back end**: its login form always posts a username
and password, which its server turns into rustak's `/oauth/token` password
grant. So none of this section is on CloudTAK's path yet. It is implemented to
TAK Server's contract so that when CloudTAK does ship an OIDC client, it works
against rustak unchanged — no rustak-specific branch, no configuration beyond
registering the client above. See `.claude/plan/compat/oauth.md` §4.

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
