# `interop/node-tak` — the CloudTAK-library contract suite

This directory drives rustak with **`@tak-ps/node-tak`** and
**`@tak-ps/node-cot`** — the libraries [CloudTAK][cloudtak] itself depends on,
at the versions CloudTAK pins. It is not a CloudTAK-*like* client written
against the compatibility notes: it is the code CloudTAK runs, so a shape rustak
gets wrong fails here the same way it would fail in a deployment.

It owns the HTTP/JSON half of the compatibility contract — `/oauth/token`,
`/Marti/api/tls/*`, version, channels, contacts, missions, files — plus the
`ssl://` stream handshake as node-tak performs it. The transport and CoT half
belongs to [`interop/eud`](../eud/README.md), which drives ATAK's own
`commoncommo`; neither suite covers what the other does.

```
interop/node-tak/
  src/
    run.ts            `npm test`: start a server, bootstrap it, run the scenarios, clean up
    rustak.ts         the throwaway server: scratch config, three listeners, readiness
    prepare.ts        the bootstrap child, run with the server's CA already trusted
    bootstrap.ts      the /api/v1 walk: setup token → admin → passkey → client password
    webauthn.ts       a software WebAuthn authenticator, because that walk needs one
    probe.ts          which compatibility surfaces this server actually serves
    surfaces.ts       the surface list, and the milestone each missing one waits on
    session.ts        what the runner hands the scenarios
    client.ts         the three ways CloudTAK talks to a server, built out of node-tak
  tests/
    bootstrap.test.ts the harness's own contract — always runs
    version.test.ts   GET /Marti/api/version, by bearer and by client certificate
    login.test.ts     POST /oauth/token, and the JWT shape CloudTAK's parser demands
    enrollment.test.ts  tls/config + signClient/v2, via Credentials.generate()
    groups.test.ts    channels: Group.list() / Group.update()
    contacts.test.ts  Contacts.list() and clientEndPoints
    stream.test.ts    ssl:// connect, ping → t-x-c-t-r
    relay.test.ts     one client's message reaching another, flow tag and all
    missions.test.ts  Data Sync CRUD and subscriptions
    files.test.ts     /files/api/config and Enterprise Sync
```

## Running it

The UI is embedded into the server binary by `include_dir!` at compile time, so
both have to be built first — in this order, or the server serves an empty
shell:

```bash
cd rustak-ui && trunk build
cd .. && cargo build -p rustak-server

cd interop/node-tak
npm ci
npm test
```

`npm test` starts one throwaway rustak in a scratch directory under the system
temporary directory, bootstraps it, runs every scenario against it and removes
the directory on the way out — including on `SIGINT`. Nothing is written inside
this repository and no state survives a run, which matters because almost
everything the bootstrap does is one-shot: the setup wizard closes itself for
good and the setup token is deleted when it does.

Useful invocations:

```bash
npm test -- tests/login.test.ts            # one scenario file
npm test -- --test-name-pattern=version    # one scenario
npm run typecheck                          # tsc --noEmit
```

| Environment variable | Effect |
|---|---|
| `RUSTAK_INTEROP_BINARY` | The server to run. An error when it does not exist, rather than a silent fall back to a different build. |
| `NODE_EXTRA_CA_CERTS` | Set **by the runner** for its children. Overriding it by hand breaks the verified half of the suite. |
| `RUSTAK_INTEROP_SESSION` | Set **by the runner**; a scenario file run directly without it says so. |

## How CloudTAK's contract is exercised

### The three URLs, with their three trust postures

CloudTAK stores one server as three independent base URLs and does **not**
assume they share a host or port (`compat/cloudtak.md` §1). The suite configures
all three and uses each one the way CloudTAK does:

| CloudTAK field | Here | Credential | Verifies the chain? |
|---|---|---|---|
| `webtak` | the public listener | username + client password → JWT | **yes**, with no override |
| `api` | the mutually authenticated Marti listener | the enrolled client certificate | no (`rejectUnauthorized: false`) |
| `url` | the CoT stream | the same certificate | no |

That asymmetry is the single most common CloudTAK bring-up failure, so the suite
reproduces it rather than trusting everything everywhere: `[web.public.tls] mode
= "internal"` is a genuine internal CA, and the `webtak` calls only work because
the runner puts `<data_dir>/pki/ca.crt` into `NODE_EXTRA_CA_CERTS` — exactly
what an operator has to do to the CloudTAK container. Turning verification off
would make the suite pass against a deployment CloudTAK could not log in to.

`NODE_EXTRA_CA_CERTS` is read once, when a Node process starts, and the
authority does not exist until the server has started. That is why `npm test` is
three processes: the runner starts the server and waits for `pki/ca.crt`, then
runs the bootstrap and the scenarios as children with that file already in the
trust store.

### The wire details that actually break clients

Each of these is asserted where it is produced, so a failure names the rule
rather than the symptom:

- **`Content-Type: application/json`, exactly.** node-tak compares it by string
  equality; a `; charset=UTF-8` suffix makes it return a raw string, and most
  call sites then index into it and throw (`compat/cloudtak.md` §4).
- **Never a 3xx on a Marti route.** node-tak treats anything under 400 as
  success and parses the body it did not get (§5).
- **The JWT header must be a multiple of four base64url characters and the
  claims must be flat.** CloudTAK base64-decodes the whole token as one blob and
  splits the payload on the first `}` (`compat/oauth.md` §2).
- **`nameEntry` must be an array of at least two.** `xml-js` collapses a
  one-element array to a bare object and CloudTAK then iterates a non-iterable
  (`compat/enrollment.md` §1).
- **`signedCert` and each `caN` are bare base64**, with no PEM armour — node-tak
  adds it itself (§3).
- **Contacts is a bare array; `clientEndPoints` is the envelope.** Neither has a
  fallback (`compat/contacts.md`).

### What runs today, and what is skipped

rustak is being built one milestone at a time, so most of the surface does not
exist yet. Rather than a hard-coded skip list that somebody has to remember to
delete, the runner **probes** each surface once against the server it just
started and hands the result to the scenarios. A scenario whose surface is
missing is skipped with a reason naming the brief that will serve it:

```
﹣ exchanges a client password for a token node-tak can parse
    # TODO(M2-03): POST /oauth/token is not served yet — the password grant
      lands with M2-03 (marti/oauth.rs).
```

Nothing needs editing when that brief lands — the probe notices and the scenario
starts running. `src/surfaces.ts` is the one place the list and its reasons
live.

The probe treats two answers as "not served": a `404`, and **HTML on a success
status**. rustak serves the admin UI from the same listener and answers anything
it does not recognise with the single-page shell rather than a `404`, because
the UI routes on the client (`rustak-server/src/web/ui.rs`). That has a
corollary for whoever mounts the Marti scope: it has to sit **ahead of** the
shell's catch-all, or CloudTAK gets an HTML page where it expects JSON and
node-tak's `isHTML` sniffing reports a `TAKServerError` about a route that is
perfectly well implemented.

## The bootstrap gap

rustak has no local passwords and no "create an administrator" flag. A fresh
installation writes a one-time setup token to a file with mode 0600, and the
first administrator created with it **cannot sign in** until they have registered
a passkey — the ceremony is what returns a bearer token. There is no
non-browser path to one today
(`.claude/plan/status/M0-11-web-api-auth.md`).

So `src/webauthn.ts` is a software WebAuthn authenticator: a real RSA key pair,
real CBOR, a real RSASSA-PKCS1-v1_5 signature over
`authData || sha256(clientDataJSON)`. It is the TypeScript twin of
`rustak-server/src/testing/authenticator.rs`, which does the same job for the
Rust tests, and it is a genuine authenticator — the server cannot tell it is not
a phone.

That was preferred to the alternative the brief allowed (a
`RUSTAK_TEST_ADMIN_TOKEN` the server would accept in `testing` builds) for two
reasons: it needs no change to the server, and above all it adds no code path by
which a build could ever accept an environment variable as an administrator
credential. The cost is about 250 lines of ceremony that this suite owns.

**The remaining gap is real and not worked around**: `/api/v1` has no
`POST /users`, so the suite cannot create a *second* account. Accounts arrive
through the setup wizard or through OIDC provisioning, and neither is available
to a scratch server with no identity provider. The client password is therefore
minted for the administrator rather than for an ordinary user, which means the
suite cannot yet prove that an ordinary account enrolls and authenticates the
same way an administrator does. When a user-provisioning endpoint exists,
`src/bootstrap.ts` should create one and mint the client password against it.

## Licence

`@tak-ps/node-tak` and `@tak-ps/node-cot` are **MIT**, like rustak, and are
consumed as ordinary npm dependencies. Nothing is vendored here and no GPL
source is involved — unlike [`interop/eud`](../eud/README.md), which has a
licence posture to keep to.

[cloudtak]: https://github.com/dfpc-coe/CloudTAK
