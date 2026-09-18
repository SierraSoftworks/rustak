<p align="center">
  <img src="docs/assets/icon.svg" alt="rustak" width="360" />
</p>

**A single-binary TAK server for ATAK and CloudTAK, secure by default.**

rustak is a [Team Awareness Kit](https://tak.gov) server written in Rust. It speaks the same
wire protocols as the official TAK Server — the Cursor-on-Target stream (XML and TAK Protocol v1),
certificate enrolment, channels, data packages, device profiles and the Marti HTTP API — so
[ATAK-CIV](https://github.com/deptofdefense/AndroidTacticalAssaultKit-CIV) devices and
[CloudTAK](https://github.com/dfpc-coe/CloudTAK) connect to it as they would to TAK Server. It
ships as one binary with an embedded SQLite database and an embedded admin UI, and it is a
clean-room implementation under the MIT licence: no code is shared with TAK Server or
OpenTAKServer.

It is built for the deployments the official server is too large for — a family, a search team,
a HAM radio club, a homelab — without giving up the security model a TAK deployment is supposed to
have.

> **Status: pre-release.** The CoT stream, enrolment, channels, data packages, device profiles and
> the CloudTAK login path are implemented and covered by the interop suites below; Data Sync
> (missions) is in progress and ACME is planned. There is no tagged release yet. Progress is
> tracked in [`.claude/plan/plan.md`](.claude/plan/plan.md) and the per-milestone notes under
> [`.claude/plan/status/`](.claude/plan/status/).

## Installation

Install with [Homebrew](https://brew.sh) (formula published with the first tagged release):

```sh
brew install sierrasoftworks/tap/rustak
```

Or run the container image, published for `linux/amd64` and `linux/arm64` from every build of
`main`:

```sh
docker run -v $(pwd)/data:/data -p 8446:8446 -p 8443:8443 -p 8089:8089 \
  ghcr.io/sierrasoftworks/rustak:latest   # reads /data/config.toml
```

Pre-compiled binaries for Linux (musl, static), macOS and Windows are attached to
[GitHub releases](https://github.com/SierraSoftworks/rustak/releases). The binary has no runtime
dependencies: TLS is [rustls](https://github.com/rustls/rustls), there is no OpenSSL, and the
database is embedded.

## Highlights

- **Encrypted, authenticated transport only.** The CoT stream listens on TLS with a client
  certificate required; there is no plaintext TCP listener and no anonymous access. The Marti API
  listener requires a client certificate as well, and the public HTTPS listener exists for
  enrolment, the admin UI and CloudTAK.
- **Built-in certificate authority and QR enrolment.** rustak issues device certificates from its
  own CA through the same `tls/config` and `signClient` flow ATAK uses. A device enrols with a
  one-time token rendered as a `tak://` QR code in the admin UI; the token expires in fifteen
  minutes and is consumed on first use. Revoking a certificate ends its live sessions.
- **No passwords by default.** Administrators sign in with passkeys (WebAuthn) or an OpenID
  Connect provider; first-run setup is unlocked by a one-time token written to disk. The only
  reusable secret is an opt-in, expiring *client password* minted explicitly for clients that need
  one — CloudTAK's `/oauth/token` password grant — and it is accepted nowhere else.
- **Channels, contacts and data packages as ATAK expects them.** Channel membership with IN/OUT
  direction and per-device active state, `t-x-g-c` change notices, contact and client-endpoint
  listings, mission-package upload and download over `/Marti/sync/*`, and `b-f-t-r` relay between
  devices.
- **Device profiles and config packages.** Profiles deliver `.pref` settings and files on
  enrolment and on every connect; the server builds ATAK/WinTAK and iTAK connection packages
  from its trust store (`POST /api/v1/config-packages`) for devices that cannot use the QR flow.
- **CloudTAK as the map.** rustak implements the exact response shapes CloudTAK's `node-tak`
  client parses — the `Content-Type` it compares byte for byte, the JWT layout it decodes by hand,
  the `ns2:certificateConfig` document, `/files/api/config` — so CloudTAK can be pointed at it
  with `ssl://host:8089`, `https://host:8443` and `https://host:8446`.
- **One process, one directory.** State and metadata live in SQLite (WAL mode); file content is
  stored content-addressed on disk; CoT history is written to append-only, protobuf-framed
  segment files per device, so a busy stream never turns into a busy database. Back up the data
  directory and you have everything.
- **Sidecar plugins in any language.** A plugin is an ordinary authenticated TAK client with its
  own certificate and channel scope. The `rustak-client` crate provides a reconnecting stream,
  the Marti client and a `Sidecar` trait; `rustak-plugin-example` is the template.
- **Compatibility proven in CI, not by hand.** Every push runs a TypeScript suite against the
  real `@tak-ps/node-tak` client CloudTAK uses; nightly, ATAK's own networking core
  (`commoncommo`, built from source in a container) enrols, negotiates TAK Protocol v1, routes
  chat and transfers packages against a live server.
- **Observable.** Structured logs and OpenTelemetry traces via
  [tracing-batteries](https://github.com/SierraSoftworks/tracing-batteries-rs).

## Quick start

rustak takes one TOML file. Every key is documented with its default in
[`config.example.toml`](config.example.toml); a minimal configuration is:

```toml
[server]
name = "rustak"
domains = ["tak.example.com"]
data_dir = "/var/lib/rustak"

[web.public]
listen = [":8446"]      # admin UI, /api/v1, enrolment, /oauth/token, Marti

[web.public.tls]
mode = "internal"       # a certificate from the built-in CA; "files" for your own

[web.marti]
listen = ":8443"        # Marti API, client certificate required

[stream.tls]
listen = ":8089"        # the CoT stream, TLS with client certificates only
```

Validate it, then start the server:

```sh
rustak --config config.toml --check
rustak --config config.toml
```

On first start rustak creates its CA, writes a one-time setup token to
`<data_dir>/setup-token` and logs the path. Open `https://tak.example.com:8446`, enter the token,
register a passkey (or configure OIDC), and the wizard closes itself for good. From the admin UI,
create a user and mint an enrolment token: it is shown once, as a `tak://` QR code. Scanning it
in ATAK's *Quick Connect* enrols the device and switches it to the encrypted stream. CloudTAK is
connected by minting a client password for its account and entering the three URLs above.

Docker, `docker-compose` with CloudTAK, a systemd unit, the TLS modes and backups are covered in
[`docs/deployment.md`](docs/deployment.md).

## How it compares

|  | rustak | TAK Server | OpenTAKServer |
|---|---|---|---|
| Runtime | one static Rust binary | Java services | Python (Flask) plus RabbitMQ and nginx |
| Storage | embedded SQLite + append-only files | PostgreSQL | SQLite or PostgreSQL |
| Stream transport | TLS with client certificates only | TLS; plaintext TCP optional | TLS terminated by nginx; plaintext TCP optional |
| Admin sign-in | passkeys or OIDC, no local passwords | local accounts, LDAP, OAuth | local accounts, LDAP |
| Device enrolment | one-time QR token, built-in CA | password or token, external or built-in CA | QR token, built-in CA |
| Late-joiner state | latest position of every reachable device replayed on connect | replayed | not replayed |
| Licence | MIT | GPL-3.0 | see upstream |

TAK Server is the reference implementation and supports federation, video, plugins on the server
side and much more; rustak deliberately does not. The comparison above reflects what each server
does today, as read from its source; corrections are welcome.

## Not included

Video streaming and recording, voice, server federation, QUIC transport, ExCheck and iTAK-specific
behaviour are out of scope for now. Data Sync (missions) and ACME certificates for the public
listener are under development.

## Documentation

- [`docs/deployment.md`](docs/deployment.md) — configuring, running and backing up a server.
- [`docs/interop.md`](docs/interop.md) — the automated compatibility suites and how to run them.
- [`docs/plugins.md`](docs/plugins.md) — writing a sidecar with `rustak-client`.
- [`docs/ci.md`](docs/ci.md) — the CI/CD pipeline and how to reproduce every check locally.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — workspace layout, conventions and how to propose a change.

## Development

The workspace holds the server (`rustak-server`, the `rustak` binary), the protocol crate
(`rustak-cot`), shared DTOs (`rustak-api`), the foundations crate (`rustak-core`), the sidecar
SDK (`rustak-client`), an example plugin (`rustak-plugin-example`) and the Yew admin UI
(`rustak-ui`, built with Trunk and embedded into the binary).

```sh
cd rustak-ui && trunk build && cd ..     # the UI is embedded at compile time
cargo build                              # builds the rustak binary
cargo test --workspace                   # offline; fake EUDs, a test CA and an in-process IdP
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check-file-length.sh           # files stay under 300 functional lines
```

ATAK-CIV, TAK Server and OpenTAKServer were read for protocol facts only; nothing from them is
copied into this repository, and the ATAK-side test harness builds `commoncommo` at container
build time rather than vendoring it.
