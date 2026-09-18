I have everything I need. Here is the full report.

---

# CloudTAK ↔ TAK Server compatibility surface

**Scope note on versions.** Everything below is read from `main` of each repo as of 2026-09-17. CloudTAK has recently been restructured into `api/stateless/` and `api/stateful/` — the `api/routes/` and `api/lib/` paths in your brief no longer exist; routes are at `api/stateless/routes/*.ts`. Where I could not verify something I say so explicitly rather than guessing.

---

## 1. CloudTAK architecture vs TAK Server

### 1.1 The three URLs — this is the whole configuration contract

CloudTAK does **not** use env vars for the TAK Server. It stores a single `server` row in its own Postgres, edited via `PATCH /api/server`. Source: [`api/common/schema.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/common/schema.ts):

```ts
export const Server = pgTable('server', {
    id: serial().primaryKey(),
    created: timestamp({ withTimezone: true, mode: 'string' }).notNull().default(sql`Now()`),
    updated: timestamp({ withTimezone: true, mode: 'string' }).notNull().default(sql`Now()`),
    name: text().notNull().default('Default'),
    url: text().notNull(),
    auth: jsonb().$type<{
        cert?: string;
        key?: string;
    }>().notNull().default({}),
    api: text().notNull().default(''),
    webtak: text().notNull().default(''),
    connection: boolean().notNull().default(true),
});
```

| Field | Meaning | Default / typical |
|---|---|---|
| `url` | **Streaming** CoT endpoint. Must be `ssl://` scheme or `TAK.connect` throws `Unknown TAK Server Protocol`. | `ssl://localhost:8089` |
| `api` | **Marti HTTPS API** base. All `/Marti/...` calls go here, authenticated by client certificate (mTLS). | `https://localhost:8443` |
| `webtak` | **OAuth / cert-enrolment** base. `POST /oauth/token` and `POST /Marti/api/tls/signClient/v2` go here, authenticated by username/password. | `https://host:8446` (CLI default `443`) |
| `auth.{cert,key}` | The **admin / system client certificate** in PEM. Used for the "Admin Connection" (connection `0`) and for `Certificate.validate()` lookups. | — |
| `connection` | Whether the Admin Connection is enabled in the pool. | `true` |

Defaults come from [`api/common/config.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/common/config.ts) (`url: 'ssl://localhost:8089'`, `api: 'https://localhost:8443'`) and the port triple is confirmed by [`node-tak/cli.ts`](https://github.com/dfpc-coe/node-tak/blob/main/cli.ts) — `marti = 8443`, `webtak = 443`, `stream = 8089` — and by `CommandConfig` in [`node-tak/lib/commands.ts`](https://github.com/dfpc-coe/node-tak/blob/main/lib/commands.ts):

```ts
ports: Type.Object({
    marti: Type.Integer(),
    webtak: Type.Integer(),
    stream: Type.Integer()
}),
```

**The three may be the same host with different ports, or entirely different hosts.** Nothing in CloudTAK assumes they are related. For a Rust server you can serve all three from one listener if you want — CloudTAK will happily have `api == webtak`.

CloudTAK's *own* env vars ([`.env.example`](https://github.com/dfpc-coe/CloudTAK/blob/main/.env.example)) are all about CloudTAK itself, not TAK: `POSTGRES`, `SigningSecret`, `ASSET_BUCKET`, `AWS_S3_*`, `API_URL`, `PMTILES_URL`, `CLOUDTAK_Server_Mode`, `CLOUDTAK_Hub_URL`, `StackName`, `WEBHOOKS_URL`. **There is no `TAK_SERVER`, `MartiAPI`, `AuthGroup` or `ConnectionCert` env var in current CloudTAK** — if your brief got those names from somewhere, they are from an older release or a different project; I could not find them anywhere in `main`.

### 1.2 The connectivity smoke test

`PATCH /api/server` validates a supplied admin cert by calling exactly one endpoint ([`api/stateless/routes/server.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/routes/server.ts)):

```ts
const config = await api.Files.config();
if (config.uploadSizeLimit === undefined) {
    throw new Err(400, null, 'Could not connect to TAK Server');
}
```

`Files.config()` is `GET /files/api/config` → `{ "uploadSizeLimit": <integer> }`. **Note the path: `/files/api/config`, not `/Marti/...`.** If you don't implement this, admins cannot save a certificate and the whole setup wizard dead-ends. This is the single highest-priority endpoint to implement.

### 1.3 What lives in CloudTAK's Postgres vs the TAK Server

**CloudTAK-owned (never asked of the TAK Server):** basemaps, overlays, iconsets, ETL Layers and their schedules/tasks, imports, geofences, Core Events/Boards/Forms, chat history mirror, profile settings/display prefs, sessions, passkeys, API tokens, video leases, feature caches.

**Delegated entirely to the TAK Server:** authentication (password check), client certificate issuance, groups/channels and their active state, contacts, client endpoints, subscriptions, missions (Data Syncs) and everything under them — changes, layers, logs, contents, invitations, roles — data packages and the file store, CoT history, KML export, injectors, repeaters, video connections.

**Dual-written (CloudTAK caches a TAK-owned identifier):** `data.mission_guid`, `data.mission_token`, `profile_overlays.mode_id` (mission GUID) and `profile_overlays.token` (mission subscription token), `profile.auth` (the user's issued cert+key), `connections.auth` (a machine cert+key).

### 1.4 The four CloudTAK nouns

| CloudTAK concept | Table | TAK Server mapping |
|---|---|---|
| **Server** | `server` (exactly one row, id 1) | The TAK Server itself: three URLs + admin cert |
| **Connection** | `connections` | A **machine identity holding its own client certificate** (`auth: {cert, key}` PEM). Each enabled Connection opens its own persistent TLS socket to `url` (8089) and its own mTLS Marti client against `api`. Its TAK UID is derived from the cert subject. |
| **Profile** | `profile` (PK = `username`, an email) | A CloudTAK user. `profile.auth` holds the **per-user client certificate** CloudTAK enrolled on their behalf. Every Marti call made "as the user" uses that cert. Each logged-in user also gets their own 8089 socket with UID `ANDROID-CloudTAK-{email}`. |
| **Layer** | `layers` (+ `layers_incoming` / `layers_outgoing`) | Pure CloudTAK ETL. No TAK Server feature. Output is either CoT written to a Connection's socket, or a Data Sync. |
| **Data Sync** | `data` | A TAK Server **Mission**. `data.name` → mission name, `data.mission_guid` → mission GUID, `data.mission_token` → the mission's `token` returned at creation, `data.mission_role` → `defaultRole`, `data.mission_groups` → `group`. |

The `Data` table verbatim ([`api/common/schema.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/common/schema.ts)):

```ts
export const Data = pgTable('data', {
    id: serial().primaryKey(),
    /* ... */
    name: text().notNull(),
    description: text().notNull().default(''),
    mission_sync: boolean().notNull().default(false),
    mission_diff: boolean().notNull().default(false),
    mission_role: text().notNull().default('MISSION_SUBSCRIBER'),
    mission_token: text(),
    mission_guid: text(),
    mission_groups: text().array().notNull().default([]),
    assets: jsonb().$type<Array<string>>().notNull().default(['*']),
    connection: integer().notNull().references(() => Connection.id),
});
```

### 1.5 Identity / UID conventions you must accept

From [`api/common/control/connection.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/common/control/connection.ts):

```ts
static uid(cert: string): string {
    const x509 = new X509Certificate(cert);
    return (x509.subject || '').split('\n').reverse().join(',');
}
```

So a Connection's TAK UID is **the certificate subject DN, components reversed and comma-joined** (Node prints subject as newline-separated `key=value`, least-significant first; reversing yields conventional RFC-4514 order). A user's UID is `ANDROID-CloudTAK-{email}` ([`api/common/connection-config.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/common/connection-config.ts)). Mission invitation lookups use the same string: `api.MissionInvite.list('ANDROID-CloudTAK-' + user.email)`. Data Sync mission operations use `creatorUid: \`connection-${data.connection}-data-${data.id}\``.

---

## 2. Authentication

### 2.1 Login flow, step by step

Entry point is `POST /api/login` in [`api/stateless/routes/login.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/routes/login.ts). Body `{ username, password }`. It delegates to `Provider.login()` in [`api/stateless/lib/provider.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/lib/provider.ts):

```ts
async login(username: string, password: string): Promise<string> {
    const auth = new APIAuthPassword(username, password);
    const api = await TAKAPI.init(new URL(this.config.server.webtak), auth);

    const contents = await api.OAuth.parse(auth.jwt);
    /* ... */
    return contents.sub;
}
```

**Step 1 — `TAKAPI.init` triggers `APIAuthPassword.init()`, which calls `OAuth.login()`.** From [`node-tak/lib/api/oauth.ts`](https://github.com/dfpc-coe/node-tak/blob/main/lib/api/oauth.ts):

```
POST {webtak}/oauth/token
Content-Type: application/x-www-form-urlencoded

grant_type=password&username=<u>&password=<p>
```

No `client_id`, no `client_secret`, no Basic client auth, no `scope`. Expected response handling, verbatim:

```ts
if ([401, 403].includes(authres.status)) {
    throw new Err(400, new Error(text), 'TAK Server reports incorrect Username or Password');
} else if (!authres.ok) {
    throw new Err(400, new Error(`Status: ${authres.status}: ${text}`), 'Non-200 Response from Auth Server - Token');
}

const body: any = JSON.parse(text);

if (body.error === 'invalid_grant' && body.error_description.startsWith('Bad credentials')) {
    throw new Err(400, null, 'Invalid Username or Password');
} else if (body.error || !body.access_token) {
    throw new Err(500, new Error(body.error_description), 'Unknown Login Error');
}

return {
    token: body.access_token,
    contents: this.parse(body.access_token)
};
```

So: **200 with `{"access_token": "<jwt>"}`** on success; **401 or 403** for bad credentials (a 400 with `{"error":"invalid_grant","error_description":"Bad credentials..."}` also works). Note `authres.ok` means 200–299 — a 3xx redirect is a failure here. Note also that `text` is parsed with `JSON.parse` unconditionally after the status checks, so a 200 must be JSON.

**Step 2 — claim extraction.** This is the most fragile part of the whole integration. Verbatim:

```ts
export const TokenContents = Type.Object({
    sub: Type.String(),
    aud: Type.String(),
    nbf: Type.Number(),
    exp: Type.Number(),
    iat: Type.Number()
})

parse(jwt: string): Static<typeof TokenContents>{
    const split = Buffer.from(jwt, 'base64').toString().split('}').map((ext) => { return ext + '}'});
    if (split.length < 2) throw new Err(500, null, 'Unexpected TAK JWT Format');
    const contents: { sub: string; aud: string; nbf: number; exp: number; iat: number; } = JSON.parse(split[1]);

    return contents;
}
```

Consequences you must design for:

- **No signature verification. No JWKS fetch. No `/oauth/token_key` call.** I grepped both repos: `token_key`, `jwks`, `.well-known` appear nowhere in node-tak or CloudTAK's server code. CloudTAK trusts the token because it trusts the TLS channel to `webtak`. Your server does not need to publish a public key for CloudTAK's sake (real TAK clients may still want `/oauth/token_key`, but CloudTAK will not ask).
- **It base64-decodes the *entire* JWT string as one blob**, not the payload segment. `.` is not in the base64 alphabet so Node strips it, concatenating header+payload+signature bytes. The decode only aligns correctly on the payload if the **header's base64url encoding is a multiple of 4 characters**, i.e. the **header JSON must be a whole number of 3 bytes long**. `{"alg":"RS256"}` is 15 bytes → 20 b64 chars → fine. `{"alg":"RS256","typ":"JWT"}` is 27 bytes → fine. A header of, say, 16 or 17 bytes would shift the payload and break login with `Unexpected TAK JWT Format` or a JSON parse error. **Pad your header to a multiple of 3 bytes.**
- **It splits on the first `}`.** `split[1]` is everything between the first and second `}`. Therefore the payload **must not contain any nested object or any `}` before its closing brace**. Flat scalar claims only. A `{"authorities":["ROLE_USER"]}` array is fine (no braces), but `{"realm_access":{"roles":[...]}}` would break it outright.
- **Only `sub` is actually read.** `provider.login()` returns `contents.sub` and `login.ts` uses it as the CloudTAK profile key (`email`). `aud`, `nbf`, `exp`, `iat` are declared in the TypeBox type but the function returns the parsed object by a plain cast — no runtime validation — so they are never checked. `user_name`, `authorities`, `scope`, `jti` are never read by CloudTAK at all.
- **`sub` must equal the username the user typed**, because the CSR common name in step 3 is taken from the typed username while the profile key comes from `sub`. The login route documents the body as *"Case-Sensitive username, if an email, the client MUST lowercase"*.

**Step 3 — certificate enrolment.** `api.Credentials.generate()` from [`node-tak/lib/api/credentials.ts`](https://github.com/dfpc-coe/node-tak/blob/main/lib/api/credentials.ts). Two calls:

**3a. `GET {webtak}/Marti/api/tls/config`** — authenticated with `Authorization: Bearer <access_token>` (the default for `APIAuthPassword`). Response is **XML**, parsed as:

```ts
const config: any = xml2js(await this.config(), { compact: true });
const nameEntries = config['ns2:certificateConfig'].nameEntries;
if (nameEntries && nameEntries.nameEntry) {
    for (const ne of nameEntries.nameEntry) {
        if (ne._attributes && ne._attributes.name === 'O') organization = ne._attributes.value;
        if (ne._attributes && ne._attributes.name === 'OU') organizationUnit = ne._attributes.value;
    }
}
```

The root element name **must literally be `ns2:certificateConfig`**, and `nameEntry` must be an *array* (i.e. emit at least two entries, otherwise `xml-js` compact mode collapses it to an object and the `for...of` throws). Minimal shape:

```xml
<ns2:certificateConfig xmlns:ns2="http://bbn.com/marti/xml/config">
  <nameEntries>
    <nameEntry name="O" value="Your Org"/>
    <nameEntry name="OU" value="Your Unit"/>
  </nameEntries>
</ns2:certificateConfig>
```

**3b. `POST {webtak}/Marti/api/tls/signClient/v2?clientUid=<username>%20(ETL)&version=3`**

```ts
const url = new URL(`/Marti/api/tls/signClient/v2`, this.api.url);
url.searchParams.append('clientUid', username + ' (ETL)');
url.searchParams.append('version', '3');

const res = await this.api.fetch(url, {
    method: 'POST',
    nocookies: true,
    headers,
    body: keys.csr
});
```

with `headers = { Accept: 'application/json', Authorization: 'Basic ' + btoa(username + ":" + password) }`.

Critical details:
- **Method is `POST`.** (The OpenTAKServer docs list this endpoint under PUT — see §8.)
- **Auth is HTTP Basic, not Bearer.** `Credentials.generate()` sets `headers.Authorization` explicitly, and `APIAuthPassword.fetch()` only injects the Bearer token `if (!opts.headers.Authorization && this.jwt)`. So the OAuth token is obtained (it must succeed, or `TAKAPI.init` throws) but then *not used* for the signing call. You must accept **both** Basic and Bearer on this endpoint to be safe, and Basic at minimum.
- **Body is a raw PEM CSR** (`-----BEGIN CERTIFICATE REQUEST-----…`) as the request body, `Content-Type` unset by the caller (node-tak's `fetch` wrapper only sets it for plain objects/arrays/FormData/URLSearchParams, so a string body goes out with no explicit content type).
- CSR subject: `CN = <username>`, plus `O` and `OU` from `tls/config`. Generated by `pem.createCSR`.
- **Response must be JSON** with these exact keys:

```ts
let cert = '-----BEGIN CERTIFICATE-----\n' + res.signedCert;
if (!res.signedCert.endsWith('\n')) cert = cert + '\n';
cert = cert + '-----END CERTIFICATE-----';

const chain = [];
if (res.ca0) chain.push(res.ca0);
if (res.ca1) chain.push(res.ca1);

return { ca: chain, cert, key: keys.clientKey }
```

i.e. `{ "signedCert": "<base64 DER body, NO PEM header/footer>", "ca0": "...", "ca1": "..." }`. **`signedCert` must be the bare base64 without the PEM armour** — node-tak adds the armour itself. `ca0`/`ca1` are optional and are stored raw (they are not re-armoured, so emit them however your trust chain needs; CloudTAK only ever stores them in `ConnectionAuth.ca` and does not use them for the Marti API).

Return type:
```ts
export const CertificateResponse = Type.Object({
    ca: Type.Array(Type.String()),
    cert: Type.String(),
    key: Type.String()
});
```

**Step 4 — certificate validation on every subsequent login/page load.** `Provider.valid()`:

```ts
export const CERT_RENEWAL_WINDOW_MS = 7 * 24 * 60 * 60 * 1000;
```

- If `profile.auth` is missing, or the stored cert's `validTo` is within 7 days, CloudTAK **silently re-enrols** (re-runs step 3) if it has the password in hand; otherwise it throws `401 Certificate is expired`.
- It then constructs an mTLS client against `config.server.api` and calls `Certificate.probe()`:

```ts
async probe(): Promise<Static<typeof CertificateProbe>> {
    const url = new URL('/Marti/api/version', this.api.url);
    try {
        const version = String(await this.api.fetch(url)).trim();
        return { accepted: true, version };
    } catch (err) { /* classify */ }
}
```

**`GET {api}/Marti/api/version` returning a short plain-text version string is therefore required on every login.** Its failure mode is classified by regex over the error text:

```ts
if (/RevokedException|revoked certificate/i.test(haystack)) reason = 'revoked';
else if (/BadCredentialsException|AuthenticationException|TAK Server authentication/i.test(haystack)) reason = 'rejected';
else if (err instanceof Err && err.status === 400 && !(err instanceof TAKServerError)
    && /certificate|handshake|alert|EPROTO|ECONNRESET|SSL|TLS/i.test(haystack)) reason = 'tls';
else throw err;  // NOT treated as an auth failure — propagates
```

Practical guidance: return **2xx with a plain-text version** when the client cert is good. If you reject a cert, reject it at the **TLS layer** (handshake failure) — that lands in the `tls` bucket cleanly. Returning an opaque 401/500 with a body that matches none of those regexes causes `probe()` to **rethrow**, which surfaces as a hard error rather than a "please log in again".

`GET /api/login` (session check) runs `Provider.valid()` without a password, so a healthy session requires `GET /Marti/api/version` to keep succeeding under mTLS.

Optionally, `Certificate.validate()` hits the **cert-admin API** (`/Marti/api/certadmin/cert/{hash}`, falling back to `/Marti/api/certadmin/cert?username=`) using the *admin* cert, purely to add a revocation date to an error message. It is wrapped in try/catch and is **not required**.

### 2.2 OIDC — the honest answer

**CloudTAK does not implement an OIDC client in the open-source `main` branch.** I verified this by downloading all 71 files in `api/stateless/routes/` and grepping; the only hits for `oidc|openid|/authorize|keycloak|saml` are:

- [`api/common/defaults.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/common/defaults.ts) — five settings, defaults shown verbatim:
  ```ts
  'oidc::enabled': false,
  'oidc::enforced': false,
  'oidc::name': '',
  'oidc::discovery': '',
  'oidc::logo': '',
  ```
- [`api/stateless/routes/config.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/routes/config.ts) — exposes those five keys through `GET/PUT /api/config`.
- [`api/stateless/routes/login.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/routes/login.ts) — the *only* runtime behaviour: refuse password login when both flags are on.
  ```ts
  if (oidc['oidc::enabled'] && oidc['oidc::enforced']) {
      throw new Err(403, null, 'Username/Password login is disabled - Please use SSO');
  }
  ```
- [`api/web/src/components/Login.vue`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/web/src/components/Login.vue) — renders a button with `href='/api/login/oidc'`, plus a warning card *"OIDC Misconfigured — The administrator has not configured OIDC correctly"* when `oidc::discovery` is empty.
- [`api/test/login-oidc.srv.test.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/test/login-oidc.srv.test.ts) — tests only the 403 refusal and the non-enforced pass-through. It never exercises an OIDC handshake.

**There is no `GET /api/login/oidc` route handler anywhere in the repository.** The CHANGELOG corroborates that this is unfinished: *"Beta: surface OIDC settings in Config UI"*, *"Rewrite Login component to support display of OIDC options"*, *"Disable login endpoints if OIDC is enforced"* — all UI/config-level, no backend callback. ([CHANGELOG.md](https://github.com/dfpc-coe/CloudTAK/blob/main/CHANGELOG.md))

**How SSO actually works in the reference (COTAK) deployment.** It is not OIDC in CloudTAK at all — it is **LDAP behind the TAK Server**. [`dfpc-coe/auth-infra`](https://github.com/dfpc-coe/auth-infra) is described as *"Infrastructure to support LDAP based auth in TAK via Authentik"*, deploying an Authentik server plus an **Authentik LDAP Outpost**. And [`dfpc-coe/tak-infra/CoreConfig.base.xml`](https://github.com/dfpc-coe/tak-infra/blob/main/CoreConfig.base.xml) sets:

```xml
<auth default="ldap" x509groups="true" x509addAnonymous="false" x509useGroupCache="true" x509useGroupCacheDefaultActive="true" x509checkRevocation="true">
    <oauth oauthUseGroupCache="true"/>
```

So: Authentik is the IdP → LDAP outpost → TAK Server `auth default="ldap"` → TAK Server's `/oauth/token` password grant validates against LDAP → CloudTAK sees plain username/password. **The user's browser never touches the IdP.**

**What this means for your requirement.** "Signing in with OIDC in CloudTAK should transparently work for this server" can be satisfied in exactly one of two ways today:

1. **Resource-Owner-Password-Credentials bridge (what actually works now).** Your Rust server implements `POST /oauth/token` with `grant_type=password` and internally validates the credentials against your IdP — via LDAP, via the IdP's own ROPC grant, or against your own user store federated to the IdP. CloudTAK's existing login works unchanged and the user sees a normal username/password box. This is what OpenTAKServer and the COTAK stack both do. **This is the only path I can confirm works end-to-end.**

2. **Implement the missing CloudTAK route yourself.** If you want the real browser redirect flow, `GET /api/login/oidc` must be written (upstream contribution or fork). Since the frontend only has `oidc::discovery` (a discovery document URL), `oidc::name`, `oidc::logo`, the intended design is clearly **CloudTAK acting as the OIDC RP directly against the IdP**, not proxying through the TAK Server. But note the hard problem: even after an OIDC login, CloudTAK still needs a **client certificate** for the user, and the only enrolment path node-tak implements requires either Basic auth (password) or `APIAuthToken` + an explicit username. `Credentials.generate()` does support token auth:
   ```ts
   } else if (this.api.auth instanceof APIAuthToken) {
       // TAK Server derives the enrollment username from the token claims and requires
       // the CSR CN to match it - the caller must supply that username explicitly
       if (!opts.username) throw new Error('Token Auth requires a username for the Certificate CN');
       username = opts.username;
   }
   ```
   …but **CloudTAK never uses `APIAuthToken`** — I grepped; every `Credentials.generate()` call site in CloudTAK constructs `APIAuthPassword`. So for an OIDC-only CloudTAK you would additionally need to change CloudTAK to pass an `APIAuthToken` and your server to accept `Authorization: Bearer <idp-or-takserver-token>` on `POST /Marti/api/tls/signClient/v2`.

**TAK Server-side OIDC, for reference.** If you want to mirror official TAK Server 5.x semantics anyway, the CoreConfig model is in [`dfpc-coe/tak-infra/src/CoreConfigType.ts`](https://github.com/dfpc-coe/tak-infra/blob/main/src/CoreConfigType.ts):

```ts
oauth: Type.Optional(Type.Object({
    _attributes: Type.Optional(Type.Object({
        oauthAddAnonymous: ..., oauthUseGroupCache: ..., loginWithEmail: ...,
        useTakServerLoginPage: ..., readOnlyGroup: ...,
        readGroupSuffix: Type.Optional(Type.String({ default: "_READ" })),
        writeGroupSuffix: Type.Optional(Type.String({ default: "_WRITE" })),
        groupsClaim: Type.Optional(Type.String({ default: "groups" })),
        usernameClaim: Type.Optional(Type.String()),
        scopeClaim: Type.Optional(Type.String({ default: "scope" })),
        webtakScope: Type.Optional(Type.String()),
        groupprefix: ..., allowUriQueryParameter: ..., allowAccessTokenRetrieval: ...,
    })),
    client: /* clientId, secret, redirectUri, resourceIds, scope, authorizedGrantTypes, authorities, autoapprove, refreshTokenValidity */,
    authServer: Type.Optional(Type.Array(Type.Object({
        _attributes: Type.Object({
            name: Type.String(), issuer: Type.String(), clientId: Type.String(),
            secret: Type.String(), redirectUri: Type.String(),
            scope: Type.Optional(Type.String()),
            authEndpoint: Type.String(), tokenEndpoint: Type.String(),
            accessTokenName: Type.Optional(Type.String({ default: "access_token" })),
            refreshTokenName: Type.Optional(Type.String({ default: "refresh_token" })),
            trustAllCerts: Type.Optional(Type.Boolean({ default: false })),
        }),
        key: Type.Optional(Type.Array(Type.String()))
    }))),
    openIdDiscoveryConfiguraiton: /* name, clientId, secret, redirectUri, configurationUri, ... */
}))
```

Note `groupsClaim` default `"groups"` and the `_READ`/`_WRITE` group suffix convention — that's how TAK Server maps IdP groups onto channels. If you want ATAK/WinTAK OIDC to work against your server too, that's the vocabulary to mirror. **But none of it is needed for CloudTAK.**

### 2.3 Server-to-server authentication

Three distinct credentials, all X.509 client certs over mTLS to `config.server.api`:

1. **Admin cert** (`server.auth`) — used for the Admin Connection (connection `0`) and `Certificate.validate()`. Configured once by an admin pasting PEM into `PATCH /api/server`.
2. **Per-Connection certs** (`connections.auth`) — used for that Connection's streaming socket and for Data Sync mission management (`DataMission.sync` creates missions, subscribes, manages layers using the Connection's cert).
3. **Per-user certs** (`profile.auth`) — used for *every* Marti call made on behalf of a logged-in user. Pattern, repeated verbatim across all `marti-*.ts` routes:
   ```ts
   const user = await Auth.as_user(config, req);
   const profile = await authenticatedProfile(config, user.email);
   const api = await TAKAPI.init(new URL(String(config.server.api)), new APIAuthCertificate(profile.auth.cert, profile.auth.key));
   ```

**There is no P12 anywhere** — CloudTAK stores PEM `cert` + `key` strings. (`api/stateless/lib/certificate.ts` has `generateClientP12`/`generateTrustP12`, but those are for *exporting* a bundle to an ATAK device, not for talking to the TAK Server.)

### 2.4 TLS verification asymmetry — important

- **Marti API (mTLS):** `rejectUnauthorized: false`, hard-coded in `APIAuthCertificate.fetch()` ([`node-tak/lib/auth.ts`](https://github.com/dfpc-coe/node-tak/blob/main/lib/auth.ts)). Self-signed server cert is fine.
- **Streaming 8089:** `rejectUnauthorized: this.auth.rejectUnauthorized ?? false` ([`node-tak/index.ts`](https://github.com/dfpc-coe/node-tak/blob/main/index.ts)), and CloudTAK never sets it. Self-signed is fine.
- **WebTAK / OAuth / signClient:** goes through plain `undici` `fetch` ([`node-tak/lib/fetch.ts`](https://github.com/dfpc-coe/node-tak/blob/main/lib/fetch.ts)) with **full system-CA TLS verification**. A self-signed cert on the `webtak` host **breaks login** and cannot be turned off from CloudTAK config.

This explains open CloudTAK issue [#983 "Ability to provide Self-signed SSL option"](https://github.com/dfpc-coe/CloudTAK/issues/983) and the OpenTAKServer docs' insistence on Let's Encrypt certificates. **Your `webtak` listener needs a publicly trusted certificate** (or operators must set `NODE_EXTRA_CA_CERTS` on the CloudTAK container).

---

## 3. node-tak API surface — the compatibility contract

All files under [`node-tak/lib/api/`](https://github.com/dfpc-coe/node-tak/tree/main/lib/api). Modules marked ⚠️ are **not used by CloudTAK** (verified by grepping every route and lib file) — stub or omit them freely.

### 3.1 `oauth.ts`
| Method | Endpoint | Body | Response |
|---|---|---|---|
| `login()` | `POST {webtak}/oauth/token` | form-urlencoded `grant_type=password&username&password` | `{ access_token }` |
| `parse()` | *(local)* | — | `{ sub, aud, nbf, exp, iat }` |

### 3.2 `credentials.ts`
| Method | Endpoint | Auth | Response |
|---|---|---|---|
| `config()` | `GET /Marti/api/tls/config` | Bearer | XML `ns2:certificateConfig` |
| `generate()` | `POST /Marti/api/tls/signClient/v2?clientUid={user} (ETL)&version=3` | **Basic** | `{ signedCert, ca0?, ca1? }` |

### 3.3 `certificate.ts` — mostly ⚠️ admin-only
| Method | Endpoint | Used by CloudTAK? |
|---|---|---|
| `probe()` | `GET /Marti/api/version` | ✅ **every login + session check** |
| `validate()` | calls `get()` then `list()` | ✅ best-effort, error-message only |
| `get(hash)` | `GET /Marti/api/certadmin/cert/{hash}` | via `validate()` |
| `list(username?)` | `GET /Marti/api/certadmin/cert?username=` | via `validate()` fallback |
| `listActive/Revoked/Replaced/Expired()` | `GET /Marti/api/certadmin/cert/{active,revoked,replaced,expired}` | ⚠️ |
| `download(hash)` | `GET /Marti/api/certadmin/cert/{hash}/download` | ⚠️ |
| `downloadIds(ids)` | `GET /Marti/api/certadmin/cert/download/{csv}` | ⚠️ |
| `revoke(hash)` | `DELETE /Marti/api/certadmin/cert/{hash}` | ⚠️ |
| `revokeIds`/`deleteIds` | `DELETE /Marti/api/certadmin/cert/{revoke,delete}/{id}` | ⚠️ |

```ts
export const Certificate = Type.Object({
    id: Type.Integer(),
    creatorDn: Type.String(),
    subjectDn: Type.String(),
    userDn: Type.String(),
    certificate: Type.String(),
    hash: Type.String(),
    clientUid: Type.String(),
    issuanceDate: Type.String({ format: 'date-time' }),
    expirationDate: Type.String({ format: 'date-time' }),
    effectiveDate: Type.String({ format: 'date-time' }),
    revocationDate: Type.Optional(Type.String({ format: 'date-time' })),
    token: Type.String(),
    serialNumber: Type.String()
});
```

Note the documented quirk: *"The TAK Server reports an unknown hash via sendError(500)"* — node-tak treats a 500 from `get(hash)` as "not found" and falls back to the list. Mirror that or just return a normal 404-ish error and accept the fallback path.

### 3.4 `groups.ts` — **essential**
| Method | Endpoint |
|---|---|
| `list({useCache?, sendLatestSA?})` | `GET /Marti/api/groups/all?useCache=&sendLatestSA=` |
| `update(Group[], query)` | `PUT /Marti/api/groups/active` (body = JSON array of Group) |

```ts
export const Group = Type.Object({
    name: Type.String(),
    direction: Type.String(),
    created: Type.String(),
    type: Type.String(),
    bitpos: Type.Number(),
    active: Type.Boolean(),
    description: Type.Optional(Type.String())
})
export const TAKList_Group = TAKList(Group);
```

`bitpos` and `active` carry real weight: CloudTAK builds its channel filter from `data.filter(g => g.active).map(g => g.bitpos)` ([`api/stateless/lib/tak-channels.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/lib/tak-channels.ts)), and `DataMission.sync` **force-activates every group** before creating a mission:

```ts
// All groups should be active for data-sync api to work properly
const groups = await api.Group.list({ useCache: true });
if (groups.data.some(g => !g.active)) {
    await api.Group.update(groups.data.map((group) => { group.active = true; return group; }), {});
}
```

`useCache=true` means "return the user's *saved* channel selection rather than a fresh pull from the auth backend" — the comment in `tak-channels.ts` explains CloudTAK deliberately defers caching to the server because it runs horizontally scaled replicas.

### 3.5 `contacts.ts` — **essential**
`list()` → `GET /Marti/api/contacts/all`, returning a **bare JSON array** (not a `TAKList` envelope):

```ts
export const Contact = Type.Object({
    filterGroups: Type.Any(), // I'm not familiar with this one
    notes: Type.String(),
    callsign: Type.String(),
    team: Type.String(),
    role: Type.String(),
    takv: Type.String(),
    uid: Type.String()
});
```
`notes` is dereferenced unguarded in the CLI formatter (`contact.notes.trim()`) so emit `""` rather than null.

### 3.6 `client.ts`
`list()` → `GET /Marti/api/clientEndPoints?secAgo=&showCurrentlyConnectedClients=&showMostRecentOnly=&group=…` (`group` repeatable).

```ts
export const ClientEndpoint = Type.Object({
    callsign: Type.String(), uid: Type.String(), username: Type.String(),
    team: Type.String(), role: Type.String(), lastStatus: Type.String(),
});
export const ClientEndpointList = TAKList(ClientEndpoint);
```

### 3.7 `subscriptions.ts`
`list({sortBy, direction, page, limit})` → `GET /Marti/api/subscriptions/all`. Defaults `sortBy=CALLSIGN`, `direction=ASCENDING`, `page=-1`, `limit=-1`. The `Subscription` type is large (37 fields incl. `dn`, `clientUid`, `groups: Group[]`, `protocol`, `battery*`, `heap*`, `storage*`, `incognito`, `handlerType`, `lastReportDiffMilliseconds`) and **all fields are required in the TypeBox schema** — but note CloudTAK's routes pass results straight through without `TypeCompiler` validation, so partial responses will not hard-fail. This is an admin/diagnostics page only.

### 3.8 `mission.ts` — the big one

Envelope helpers ([`lib/api/types.ts`](https://github.com/dfpc-coe/node-tak/blob/main/lib/api/types.ts)):
```ts
export const TAKItem = <T extends TSchema>(T: T) => {
    return Type.Object({
        version: Type.String(),
        type: Type.String(),
        data: T,
        messages: Type.Optional(Type.Array(Type.String())),
        nodeId: Type.Optional(Type.String())
    })
};
export const TAKList = <T extends TSchema>(T: T) => { return TAKItem(Type.Array(T)); }
```

`Mission`, verbatim:
```ts
export const Mission = Type.Object({
    name: Type.String(),
    description: Type.String(),
    chatRoom: Type.Optional(Type.String()),
    baseLayer: Type.Optional(Type.String()),
    bbox: Type.Optional(Type.String()),
    path: Type.Optional(Type.String()),
    classification: Type.Optional(Type.String()),
    tool: Type.String(),
    keywords: Type.Array(Type.String()),
    creatorUid: Type.Optional(Type.String()),
    createTime: Type.String(),
    externalData: Type.Array(Type.Unknown()),
    feeds: Type.Array(Type.Unknown()),
    mapLayers: Type.Array(Type.Unknown()),
    ownerRole: Type.Optional(Type.Object({
        permissions: Type.Array(Type.String()),
        type: Type.Enum(MissionSubscriberRole)
    })),
    inviteOnly: Type.Boolean(),
    expiration: Type.Number(),
    guid: Type.String(),
    uids: Type.Array(Type.Unknown()),
    logs: Type.Optional(Type.Array(MissionLog)),                // Only present if ?logs=true
    contents: Type.Array(Type.Object({
        timestamp: Type.String(),
        creatorUid: Type.Optional(Type.String()),
        data: MissionContent
    })),
    passwordProtected: Type.Boolean(),
    token: Type.Optional(Type.String()),                        // Only present when mission created
    groups: Type.Optional(Type.Union([Type.String(), Type.Array(Type.String())])),           // Only present on Mission.get()
    missionChanges: Type.Optional(Type.Array(MissionChange))   // Only present on Mission.get()
});
```

```ts
export const MissionContent = Type.Object({
    keywords: Type.Optional(Type.Array(Type.String())),
    name: Type.Optional(Type.String()),
    hash: Type.String(),
    submissionTime: Type.Optional(Type.String()),
    uid: Type.Optional(Type.String()),
    size: Type.Optional(Type.Integer()),
    creatorUid: Type.Optional(Type.String()),
    mimeType: Type.Optional(Type.String()),
    submitter: Type.Optional(Type.String()),
    expiration: Type.Optional(Type.Integer())
});

export const MissionChange = Type.Object({
    isFederatedChange: Type.Boolean(),
    type: Type.String(),
    missionName: Type.String(),
    missionGuid: Type.Optional(Type.String()),
    timestamp: Type.String(),
    serverTime: Type.String(),
    creatorUid: Type.Optional(Type.String()),
    contentUid: Type.Optional(Type.String()),
    details: Type.Optional(Type.Object({
        type: Type.String(),
        callsign: Type.Optional(Type.String()),
        title: Type.Optional(Type.String()),
        iconsetPath: Type.Optional(Type.String()),
        color: Type.Optional(Type.String()),
        attachments: Type.Optional(Type.Array(Type.String())),
        name: Type.Optional(Type.String()),
        category: Type.Optional(Type.String()),
        location: Type.Optional(Type.Object({ lat: Type.Number(), lon: Type.Number() }))
    })),
    contentResource: Type.Optional(MissionContent)
});

export const MissionRole = Type.Object({
    permissions: Type.Array(Type.String()),
    hibernateLazyInitializer: Type.Optional(Type.Any()),
    type: Type.Enum(MissionSubscriberRole)
})

export const MissionSubscriber = Type.Object({
    token: Type.Optional(Type.String()),
    clientUid: Type.String(),
    username: Type.String(),
    createTime: Type.String(),
    role: MissionRole
})

export enum MissionSubscriberRole {
    MISSION_OWNER = 'MISSION_OWNER',
    MISSION_SUBSCRIBER = 'MISSION_SUBSCRIBER',
    MISSION_READONLY_SUBSCRIBER = 'MISSION_READONLY_SUBSCRIBER'
}
```

Full endpoint table (`{n}` = name or GUID; see §5.4 for addressing):

| Method | Endpoint | Notes |
|---|---|---|
| `list` | `GET /Marti/api/missions?passwordProtected=&defaultRole=&tool=` | → `TAKList(Mission)` |
| `list({paged:true})` | `GET /Marti/api/pagedmissions?page=&pagesize=&sort=&ascending=&nameFilter=&uidFilter=&keywordFilter=&groupFilter=` | ⚠️ CloudTAK uses unpaged. `keywordFilter`/`groupFilter` repeatable |
| `get` | `GET /Marti/api/missions/{n}?password=&changes=&logs=&secago=&start=&end=` | takes `data[0]`; empty `data` → 404 |
| `getGuid` | `GET /Marti/api/missions/guid/{guid}?…` | same |
| `create` | `POST /Marti/api/missions/{name}?<all body fields as query params>` | see §5.1 |
| `update` | `POST /Marti/api/missions/{name}?…&allowGroupChange=&allowDupe=false` | read-modify-write |
| `delete` | `DELETE /Marti/api/missions/{name}?creatorUid=&deepDelete=` or `DELETE /Marti/api/missions?guid={guid}&…` | **GUID form uses a query param, not a path segment** |
| `setKeywords` | `PUT /Marti/api/missions/{name}/keywords?creatorUid=` body = `string[]` | name-only |
| `deleteKeyword` | `DELETE /Marti/api/missions/{name}/keywords/{kw}?creatorUid=` | name-only |
| `changes` | `GET /Marti/api/missions/{n}/changes?secago=&start=&end=&squashed=` | → `TAKList(MissionChange)` |
| `latestCots` | `GET /Marti/api/missions/{n}/cot` | **XML CoT document** |
| `contacts` | `GET /Marti/api/missions/{n}/contacts` | |
| `children` | `GET /Marti/api/missions/{n}/children` | ⚠️ |
| `setParent` | `PUT /Marti/api/missions/guid/{child}/parent/guid/{parent}` or `PUT /Marti/api/missions/{child}/parent/{parent}` | ⚠️ |
| `attachContents` | `PUT /Marti/api/missions/{n}/contents` body `{hashes?, uids?}` | |
| `detachContents` | `DELETE /Marti/api/missions/{n}/contents?hash=&uid=` | |
| `upload` | `PUT /Marti/api/missions/{name}/contents/missionpackage?creatorUid=` body = stream | resolves GUID→name first |
| `getArchive` | `GET /Marti/api/missions/{name}/archive` | ZIP stream |
| `subscriptions` | `GET /Marti/api/missions/{n}/subscriptions` | → `TAKItem(MissionSubscriber)` |
| `subscriptionRoles` | `GET /Marti/api/missions/{n}/subscriptions/roles` | → `TAKList(MissionSubscriber)` |
| `subscription` | `GET /Marti/api/missions/{n}/subscription?uid=` | returns `res.data` |
| `subscribe` | `PUT /Marti/api/missions/{n}/subscription?uid=&password=&secago=&start=&end=` | → `TAKItem(MissionSubscriber)`; **`data.token` is captured** |
| `unsubscribe` | `DELETE /Marti/api/missions/{n}/subscription?uid=&disconnectOnly=` | |
| `role` | `GET /Marti/api/missions/{n}/role` | returns `res.data` |
| `setRole` | `PUT /Marti/api/missions/{n}/role?clientUid=&username=&role=` | |
| `access` | `GET /Marti/api/missions/{n}` | boolean wrapper, swallows errors |

### 3.9 `mission-invite.ts`
```ts
export enum MissionInviteType {
    CLIENT_UID = 'clientUid', CALLSIGN = 'callsign',
    USERNAME = 'userName', GROUP = 'group', TEAM = 'team'
}
export const MissionInvite = Type.Object({
    missionName: Type.Optional(Type.String()),
    invitee: Type.Optional(Type.String()),
    type: Type.Optional(Type.String()),
    creatorUid: Type.Optional(Type.String()),
    createTime: Type.Optional(Type.String()),
    token: Type.Optional(Type.String()),
    role: Type.Optional(MissionRole),
    missionGuid: Type.Optional(Type.String())
});
```
| Method | Endpoint |
|---|---|
| `list(clientUid)` | `GET /Marti/api/missions/invitations?clientUid={uid}` |
| `get(mission)` | `GET /Marti/api/missions/{n}/invitations` |
| `invite` | `PUT /Marti/api/missions/{n}/invite/{type}/{invitee}?…` |
| `uninvite` | `DELETE /Marti/api/missions/{n}/invite/{type}/{invitee}?…` |

**Note: `/Marti/api/missions/all/invitations` from your brief is NOT used.** CloudTAK calls `list()` with `clientUid = 'ANDROID-CloudTAK-' + user.email` on the mission list page.

### 3.10 `mission-layer.ts`
```ts
export enum MissionLayerType {
    GROUP = 'GROUP', UID = 'UID', CONTENTS = 'CONTENTS', MAPLAYER = 'MAPLAYER', ITEM = 'ITEM'
}
export const MissionLayer = Type.Object({
    // ITEM Layers represent filed Mission content and are returned without a name
    name: Type.Optional(Type.String({ minLength: 1 })),
    type: Type.Enum(MissionLayerType),
    parentUid: Type.Optional(Type.String()),
    uid: Type.String(),
    mission_layers: Type.Optional(Type.Array(Type.Any())),
    uids: Type.Optional(Type.Array(Type.Object({
        data: Type.String({ description: 'The UID of the COT' }),
        timestamp: Type.String(),
        creatorUid: Type.String(),
        keywords: Type.Optional(Type.Array(Type.String())),
        details: Type.Optional(Type.Object({
            type: Type.String(), callsign: Type.String(), color: Type.Optional(Type.String()),
            location: Type.Object({ lat: Type.Number(), lon: Type.Number() })
        }))
    }))),
    contents: Type.Optional(Type.Array(Type.Any())),
    maplayers: Type.Optional(Type.Array(Type.Any()))
});
```
Note the snake_case `mission_layers` alongside camelCase siblings — that's TAK Server's actual wire format, reproduce it exactly.

| Method | Endpoint |
|---|---|
| `list` | `GET /Marti/api/missions/{n}/layers` |
| `get` | `GET /Marti/api/missions/{n}/layers/{layerUid}` |
| `create` | `PUT /Marti/api/missions/{n}/layers?name=&type=&uid=&parentUid=&afterUid=&creatorUid=` |
| `delete` | `DELETE /Marti/api/missions/{n}/layers?uid=…&creatorUid=` |
| `rename` | `PUT /Marti/api/missions/{n}/layers/{layer}/name?name=&creatorUid=` |
| `setParent` | `PUT /Marti/api/missions/{n}/layers/parent?layerUid=…&parentUid=&afterUid=&creatorUid=` |
| `attachUids` | `PUT /Marti/api/missions/{n}/contents?creatorUid=` (body `{uids}`) |

⚠️ **Bug to be aware of:** in `create`, `delete` and `rename`, the GUID branch builds `/Marti/api/missions/guid/${this.#encodeName(name)}/layers` using `#encodeName` (which trims + `encodeURIComponent`) rather than the plain `encodeURIComponent` used elsewhere. Functionally equivalent for GUIDs, but worth knowing if you're diffing paths.

### 3.11 `mission-log.ts`
```ts
export const MissionLog = Type.Object({
    id: Type.String(),
    content: Type.String(),
    creatorUid: Type.String(),
    missionNames: Type.Array(Type.String()),
    servertime: Type.String(),
    dtg: Type.Optional(Type.String()),
    entryUid: Type.Optional(Type.String({ /* ... */ })),
    created: Type.String(),
    contentHashes: Type.Array(Type.String()),
    keywords: Type.Array(Type.String())
});
export const CreateMissionLog = Type.Object({
    dtg: Type.String({ /* ... */ }),
    entryUid: Type.Optional(Type.String({ /* ... */ })),
    content: Type.String(),
    creatorUid: Type.String(),
    contentHashes: Type.Optional(Type.Array(Type.String())),
    keywords: Type.Optional(Type.Array(Type.String()))
});
export const UpdateMissionLog = Type.Composite([ CreateMissionLog, Type.Object({ id: Type.String() })]);
```
| Method | Endpoint |
|---|---|
| `create` | `POST /Marti/api/missions/logs/entries` |
| `update` | `PUT /Marti/api/missions/logs/entries` |
| `get(id)` | `GET /Marti/api/missions/logs/entries/{id}` |
| `delete(id)` | `DELETE /Marti/api/missions/logs/entries/{id}` |

Note `servertime` is **lowercase 't'** while `createTime`/`submissionTime` elsewhere are camelCase. Reading logs is done via `Mission.get(..., { logs: true })`, not a dedicated GET.

### 3.12 `files.ts` — **essential** (data packages + attachments)
```ts
export const Content = Type.Object({
  UID: Type.String(), SubmissionDateTime: Type.String(), Keywords: Type.Array(Type.String()),
  MIMEType: Type.String(), SubmissionUser: Type.String(), PrimaryKey: Type.String(),
  Hash: Type.String(), CreatorUid: Type.String(), Name: Type.String()
});
export const TAKList_Content = TAKList(Type.Object({
    filename: Type.String(), keywords: Type.Array(Type.String()), mimeType: Type.String(),
    name: Type.String(), submissionTime: Type.String(), submitter: Type.String(),
    uid: Type.String(), size: Type.Integer(),
}));
export const Config = Type.Object({ uploadSizeLimit: Type.Integer() })
```
| Method | Endpoint | Notes |
|---|---|---|
| `config()` | `GET /files/api/config` | **the connectivity test** |
| `upload(opts, body)` | `POST /Marti/sync/upload?name=&keywords=&creatorUid=&uid=&latitude=&longitude=&altitude=` | raw body + `Content-Type`/`Content-Length`. Returns a `Content` object (JSON or JSON-as-text — node-tak handles both) |
| `uploadPackage(opts, body)` | `POST /Marti/sync/missionupload?filename=&creatorUid=&hash=&mimetype=&keyword=missionpackage&keyword=…&Groups=…` | **multipart `assetfile`**; `Groups` is capitalised *"intentionally case sensitive due to an apparent bug in TAK server"*; `keyword=missionpackage` is always injected so the package appears in the public list |
| `download(hash)` | `GET /Marti/sync/content?hash=` | stream |
| `meta(hash)` | `GET /Marti/sync/{hash}/metadata` | ⚠️ |
| `delete(hash)` | `DELETE /Marti/sync/delete?hash=` | |
| `adminDelete(hash)` | `DELETE /Marti/api/files/{hash}` | |
| `list()` | `GET /Marti/api/sync/search` | ⚠️ not used |
| `count()` | `GET /Marti/api/files/metadata/count` | ⚠️ |
| `update(hash, {keywords})` | `PUT /Marti/api/sync/metadata/{hash}/keywords` body `string[]` | |
| `update(hash, {expiration})` | `PUT /Marti/api/sync/metadata/{hash}/expiration?expiration=` | |

CloudTAK additionally calls one endpoint **directly**, bypassing node-tak ([`marti-package.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/routes/marti-package.ts)):
`GET /Marti/api/files/metadata?missionPackage=true&name={pkgName}` → `{ data: Array<Record<string,string>> }`, matched on `entry.Hash`, reading `entry.Groups` (comma-separated) and `entry.Time`. This is how the package "channels" column is populated.

### 3.13 `package.ts` — **essential**
`list({name?, tool?, uid?})` → `GET /Marti/sync/search?name=&tool=&uid=` returning `{ resultCount: number, results: Package[] }`. **Note the defensive parse** with the telling comment:
```ts
if (typeof res === 'string') {
    // The TAK Server API doesn't return application/json
    return JSON.parse(res) as { resultCount: number; results: ... }
```
```ts
export const Package = Type.Object({
    EXPIRATION: Type.String(), UID: Type.String(), SubmissionDateTime: Type.String(),
    Size: Type.String(), PrimaryKey: Type.String(), Hash: Type.String(),
    CreatorUid: Type.Optional(Type.Union([Type.Null(), Type.String()])),
    Name: Type.String(), MIMEType: Type.Optional(Type.String()),
    SubmissionUser: Type.Optional(Type.String()),
    Keywords: Type.Optional(Type.Array(Type.String())), Tool: Type.Optional(Type.String())
});
```
**SCREAMING_CASE keys and stringly-typed numbers** (`Size`, `EXPIRATION` are `String`). CloudTAK coerces with `Number(latest.Size)`.

### 3.14 `query.ts`
| Method | Endpoint |
|---|---|
| `single(uid)` | `GET /Marti/api/cot/xml/{uid}` |
| `history(uid, {start,end,secago})` | `GET /Marti/api/cot/xml/{uid}/all?…` |

Both return **CoT XML**, parsed to GeoJSON by node-cot.

### 3.15 `export.ts`
`export(query)` → `POST /Marti/ExportMissionKML` with a **form-urlencoded** body: `startTime`, `endTime`, `groups[]`, `format` (`kml`|`kmz`), `interval?`, `multiTrackThreshold?`, `extendedData?`, `optimizeExport?`. Returns a stream.

### 3.16 `injectors.ts`
`GET|POST|DELETE /Marti/api/injectors/cot/uid`, `GET /Marti/api/injectors/cot/uid/{uid}`.
```ts
export const Injector = Type.Object({ /* uid, toInject, ... */ });
```
Admin-only page; used by `server-injector.ts`.

### 3.17 `repeater.ts`
`GET /Marti/api/repeater/list`, `GET|POST /Marti/api/repeater/period`, `GET /Marti/api/repeater/remove/{uid}` (**delete is a GET** — reproduce that). Admin-only.

### 3.18 `video.ts` — out of scope, stub carefully
`GET|POST /Marti/api/video`, `GET|PUT|DELETE /Marti/api/video/{uuid}`. Returns `{ videoConnections: VideoConnection[] }` where `VideoConnection = { uuid, active, alias, thumbnail, classification, feeds: Feed[] }` and `Feed` has 22 fields, **most of them `Type.Union([Type.String(), Type.Null()])` — i.e. stringly-typed numbers that may be null** (`latitude`, `longitude`, `fov`, `heading`, `range`, `roverPort`, `networkTimeout`, `bufferTime`, `rtspReliable`, `ignoreEmbeddedKLV`) plus genuinely integer `order`, `width`, `height`, `bitrate`. To stub sensibly: return `{"videoConnections": []}` from `GET /Marti/api/video` and `404`/`501` from the rest. See §8 for the malformed-`port: -1` bug this shape causes.

### 3.19 `security.ts` — ⚠️ not used by CloudTAK
`GET /Marti/api/security/config`, `GET /Marti/api/authentication/config`, `GET /Marti/api/security/verifyConfig`, `GET /Marti/api/security/isSecure`. `SecurityConfig` exposes keystore paths/passwords, `x509Groups`, `enableEnrollment`, `caType`, `validityDays`, `msca*`. **Note your brief's `/Marti/api/security/config` is here but unreferenced by CloudTAK.**

### 3.20 `user-management.ts` — ⚠️ not used by CloudTAK
`/Marti/api/user-management/api/{new-user,new-users,list-users,get-groups-for-user/{u},change-user-password,update-groups,update-group-users,delete-user/{u},list-groupnames,users-in-group/{g}}`.

### 3.21 `profile.ts` — ⚠️ not used by CloudTAK
`connection({syncSecago, clientUid})` → `GET /Marti/api/device/profile/connection?syncSecago=&clientUid=`. Returns a device-profile package. ATAK uses this; CloudTAK does not.

### 3.22 `iconsets.ts` — ⚠️ `GET /Marti/api/iconset/all/uid`. `locate.ts` — ⚠️ `POST /locate/api?latitude=&longitude=&name=&remarks=`.

---

## 4. Streaming

### 4.1 Transport
[`node-tak/index.ts`](https://github.com/dfpc-coe/node-tak/blob/main/index.ts):

```ts
static async connect(url: URL, auth: Static<typeof TAKAuth>, opts: TAKOptions = {}): Promise<TAK> {
    const tak = new TAK(url, auth, opts);
    if (url.protocol === 'ssl:') {
        if (!tak.auth.cert) throw new Error('auth.cert required');
        if (!tak.auth.key) throw new Error('auth.key required');
        return await tak.connect_ssl();
    } else {
        throw new Error('Unknown TAK Server Protocol');
    }
}
```
Plain `tls.connect({ host, port, rejectUnauthorized: false, cert, key, passphrase, ca })` plus `setNoDelay()`. **`ssl://` only — no TCP, no WebSocket, no QUIC.**

### 4.2 Protocol: **CoT XML only, no TAK Protocol v1 negotiation**

This is unambiguous and important. The read path is:

```ts
// Match <event .../> or <event> but not <events>
export const REGEX_EVENT = /(<event[ >][\s\S]*?<\/event>)([\s\S]*)/;
export const REGEX_CONTROL = /[\u000B-\u001F\u007F-\u009F]/g;

static findCoT(str: string): null | PartialCoT {
    str = str.replace(REGEX_CONTROL, '');
    const match = str.match(REGEX_EVENT); // find first CoT
    if (!match) return null;
    return { event: match[1], remainder: match[2] };
}
```

with an inline `// Eventually Parse ProtoBuf` TODO in the `data` handler. The write path joins pre-serialised XML strings with `\n`:
```ts
const ok = this.client.write(parts.join('\n') + '\n');
```

Implications for your server:
- **Never send the TAK Protocol v1 magic byte (`0xbf`) framing or a `t-x-takp-q` protocol negotiation request** to a CloudTAK connection. It will not respond, and binary bytes would be mangled by `REGEX_CONTROL` stripping.
- `REGEX_EVENT` requires `<event ` or `<event>` and a matching `</event>` — **self-closing `<event .../>` will never match** despite the comment claiming otherwise. Always emit a full open/close pair.
- Control characters U+000B–U+001F and U+007F–U+009F are stripped from the stream before parsing. Don't rely on them as delimiters.
- Anything before the first `<event` is silently discarded; anything after the last `</event>` is kept as the buffer remainder. An XML declaration or whitespace between events is harmless.
- node-cot *does* have protobuf support (`CoTParser.to_proto`/`from_proto`, `atakmap.commoncommo.protobuf.v1.TakMessage` in [`node-CoT/lib/parser.ts`](https://github.com/dfpc-coe/node-CoT/blob/main/lib/parser.ts)) but node-tak's stream never invokes it.

### 4.3 Pings and control messages

```ts
this.pingInterval = setInterval(() => { this.ping(); }, 5000);
async ping(): Promise<void> { this.write([CoT.ping()]); }
```
`CoT.ping()` ([`node-CoT/lib/cot.ts`](https://github.com/dfpc-coe/node-CoT/blob/main/lib/cot.ts)) builds type **`t-x-c-t`**, how `h-g-i-g-o`, empty detail.

Three control types are intercepted before the `cot` event fires:

| Incoming type | Effect |
|---|---|
| `t-x-c-t-r` | sets `this.open = true`, emits `'ping'`. **This is the only thing that marks the connection live.** |
| `t-x-takp-v` with `detail.TakControl.TakServerVersionInfo._attributes.serverVersion` | stores `this.version` |
| anything else | emitted as a `'cot'` event |

So your server **must reply to `t-x-c-t` with `t-x-c-t-r`**. Send `t-x-takp-v` on connect if you want the version displayed. Two more types matter at the CloudTAK layer:
- **`t-x-g-c`** → `connection-pool.ts` calls `refreshChannels()` (re-fetches `/Marti/api/groups/all`). Emit this when a user's channel membership changes.
- **`t-x-m-c`** / **`t-x-m-c-l`** → mission change / mission log change (see §4.5).
- **`t-x-d-d`** → delete, with `properties.links[].uid` and optional `forcedelete`.

### 4.4 Reconnect semantics
`'end'` and `'close'` both trigger `destroy()` and emit; the pool's `retry(connClient)` handles backoff. `connect_ssl` guards against stale-socket races by capturing the socket in a local and checking `if (client !== this.client) return;` in every handler. `awaitSecure(timeoutMs = 15000)` waits on `'secureConnect'` — so **the TLS handshake must complete within 15s**. Note `secureConnect` fires on TLS completion regardless of `authorized`, since `rejectUnauthorized` is false; CloudTAK logs `client.authorized`/`authorizationError` but does not gate on them.

Backpressure: a bounded ring buffer (`writeQueueSize`, default `10_000`), drained in batches (`socketBatchSize`, default `64`) on `'drain'`.

### 4.5 How mission changes reach the UI — **stream, not polling**

From [`api/web/src/workers/atlas-connection.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/web/src/workers/atlas-connection.ts):

```ts
if (task.properties.type.startsWith('t-x-m-c')) {
    if (task.properties.type === 't-x-m-c-l' && task.properties.mission) {
        /* Log Entry Added or Modified notification */
    } else if (
        task.properties.type === 't-x-m-c'
        && task.properties.mission?.missionChanges?.length === 1
        && task.properties.mission?.missionChanges?.[0].contentResource?.name
    ) {
        if (task.properties.mission.missionChanges[0].type === 'ADD_CONTENT') { /* … */ }
        else if (task.properties.mission.missionChanges[0].type === 'REMOVE_CONTENT') { /* … */ }
    }
    // Mission Change Tasking
    await this.atlas.db.subChange(task);
}
```

So a `t-x-m-c` CoT carries a `<detail><mission>` block. The wire shape, from [`node-CoT/lib/types/types.ts`](https://github.com/dfpc-coe/node-CoT/blob/main/lib/types/types.ts):

```ts
export const MissionAttributes = Type.Object({
    type: Type.Optional(Type.String()),
    tool: Type.Optional(Type.String()),
    name: Type.Optional(Type.String()),
    guid: Type.Optional(Type.String()),
    authorUid: Type.Optional(Type.String()),
});
export const MissionLayer = Type.Object({
    name: ..., parentUid: ..., type: ..., uid: ...
})
export const MissionChangeDetails = Type.Object({
    _attributes: Type.Object({
        type: Type.String(), callsign: Type.Optional(Type.String()), color: Type.Optional(Type.String())
    }),
    location: Type.Optional(Type.Object({ _attributes: Type.Object({ lat: Type.String(), lon: Type.String() }) }))
})
export const MissionChangeContentResource = Type.Object({
    expiration: GenericText, filename: Type.Optional(GenericText), hash: GenericText,
    name: GenericText, size: GenericTextInteger, submissionTime: GenericText,
    submitter: GenericText, tool: Type.Optional(GenericText), uid: GenericText
});
export const MissionChange = Type.Object({
    contentUid: ..., creatorUid: ..., isFederatedChange: ..., missionName: ...,
    missionGuid: ..., timestamp: ..., type: ...,
    contentResource: Type.Optional(MissionChangeContentResource),
    details: Type.Optional(MissionChangeDetails)
})
export const MissionChanges = Type.Object({
    MissionChange: Type.Union([MissionChange, Type.Array(MissionChange)])
})
export const Mission = Type.Object({
    _attributes: Type.Optional(MissionAttributes),
    missionLayer: Type.Optional(MissionLayer),
    MissionChanges: Type.Optional(MissionChanges),
})
```

Note the `GenericText` wrapper types — these are **child elements with text content** (`<hash>abc</hash>`), not attributes, whereas `MissionChange.details` uses `_attributes`. Mixed convention; reproduce exactly.

`/Marti/api/missions/{n}/changes` **is** implemented and exposed (`GET /api/marti/missions/:guid/changes`) but that's a user-initiated history view, not the live path.

### 4.6 `<marti><dest>` routing — **both forms are used**

```ts
export const MartiDestAttributes = Type.Object({
    uid: Type.Optional(Type.String()),
    callsign: Type.Optional(Type.String()),
    group: Type.Optional(Type.String()),
    mission: Type.Optional(Type.String()),
    'mission-guid': Type.Optional(Type.String()),
    after: Type.Optional(Type.String()),
    path: Type.Optional(Type.String())
})
export const Marti = Type.Object({
    _attributes: Type.Optional(Type.Object({
        archive: Type.Optional(Type.Boolean({ description: 'Instructs the TAK Server to archive this message' })),
    })),
    dest: Type.Optional(Type.Union([MartiDest, Type.Array(MartiDest)]))
});
```

- **Channel scoping** — `api/common/connection-config.ts` builds `dest: [{ group: '<channel name>' }, …]` for Core Event broadcasts, with an explicit guard: *"An Event with no resolvable Channels would otherwise be broadcast to every Channel the Admin cert can see"*.
- **Data Sync writes** — `api/stateless/routes/connection-layer-cot.ts`:
  ```ts
  cot.addDest({ mission: data.name, path: pathMapEntryLast.uid, after: '' });
  // …and for the non-layered case:
  cot.addDest({ mission: data.name });
  ```
  **Note it addresses the mission by `name`, not GUID, in `dest`** even though every REST call prefers the GUID. Your server must accept `<dest mission="Name" path="<layerUid>" after=""/>` over the 8089 socket and file the CoT into that mission (and that mission layer). `mission-guid` exists in the type but I found no CloudTAK code that emits it.

Also relevant: `<marti archive="true">` for persistence, and `stripFlow` in `WriteOptions` which resets the flow tag to `NodeCoT-*` — *"Useful when re-submitting CoTs back to TAK Server over 8089"*, i.e. loop prevention depends on the server honouring flow tags.

---

## 5. Data Sync usage in detail

### 5.1 Mission creation

`MissionCreateInput`, verbatim:
```ts
export const MissionCreateInput = Type.Object({
    name: Type.String(),
    group: Type.Optional(Type.Union([Type.Array(Type.String()), Type.String()])),
    keywords: Type.Optional(Type.Array(Type.String())),
    creatorUid: Type.String(),
    description: Type.Optional(Type.String({ default: '' })),
    chatRoom: Type.Optional(Type.String()),
    baseLayer: Type.Optional(Type.String()),
    bbox: Type.Optional(Type.String()),
    boundingPolygon: Type.Optional(Type.Array(Type.String())),
    path: Type.Optional(Type.String()),
    classification: Type.Optional(Type.String()),
    tool: Type.Optional(Type.String({ default: 'public' })),
    password: Type.Optional(Type.String()),
    defaultRole: Type.Optional(Type.String()),
    expiration: Type.Optional(Type.Integer()),
    inviteOnly: Type.Optional(Type.Boolean({ default: false })),
    allowDupe: Type.Optional(Type.Boolean({ default: false })),
});
```

The wire call is `POST /Marti/api/missions/{name}` with **every field except `name` and `keywords` as query parameters** (`group` array is `.join(',')`-ed into a single comma-separated param — unlike `update()`, which repeats it). `keywords` are applied afterwards via a separate `PUT .../keywords` call using the token just returned.

Client-side name validation you should mirror server-side:
```ts
// I want to keep this 1:1 with the TAK Server Source Code
if (!body.name.match(/^[\p{L}\p{N}\w\d\s\.\(\)!=@#$&^*_\-\+\[\]\{\}:,\.\/\|\\]*$/u)) throw …'invalid Character';
else if (body.name.length === 0) throw …'must have a length > 0';
else if (body.name.length > 1024) throw …'cannot exceed 1024 characters';
else if (body.name.includes('/')) throw …'cannot contain forward slashes';
```

**The create response MUST include `data[0].token`** — that mission token is the only way CloudTAK can subsequently manage the mission. From [`api/stateless/lib/data-mission.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/lib/data-mission.ts):

```ts
const mission = await api.Mission.create({
    name: data.name,
    creatorUid: `connection-${data.connection}-data-${data.id}`,
    description: data.description,
    defaultRole: data.mission_role,
    group: data.mission_groups,
});

await config.models.Data.commit(data.id, {
    mission_token: mission.token || undefined,
    mission_guid: mission.guid,
});
```
`data[0].guid` must also be present — it's stored and used for all later addressing.

Also note the preceding force-activate-all-groups step (§3.4) and its comment, plus the TODO: `// TODO Update Groups: Not supported by TAK Server at this time`.

### 5.2 Mission tokens — header name is **`MissionAuthorization`**, not `Authorization`

```ts
#headers(opts?: Static<typeof MissionOptions>): object {
    if (opts && opts.token) {
        return { MissionAuthorization: `Bearer ${opts.token}` }
    } else {
        return {};
    }
}
```
(identical private helper in both `mission.ts` and `mission-layer.ts`). The scheme prefix is still `Bearer`. Every mission-scoped method accepts `opts` and passes these headers.

Token sources and storage:
- **Data Sync** → `Mission.create()` response `token` → `data.mission_token`.
- **User subscription** → `Mission.subscribe()` response `data.token` → `profile_overlays.token`. From [`api/stateless/routes/profile-overlays.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/routes/profile-overlays.ts):
  ```ts
  const sub = await api.Mission.subscribe(req.body.mode_id, {
      uid: `ANDROID-CloudTAK-${user.email}`,
      /* ... */
  }, { token: req.body.token });
  /* ... */
  token: sub.data.token,
  ```
- **Client override** — every `marti-mission*.ts` route lets the browser supply one directly:
  ```ts
  const opts: Static<typeof MissionOptions> = req.headers['missionauthorization']
      ? { token: String(req.headers['missionauthorization']) }
      : await profileControl.subscription(user.email, req.params.guid);
  ```
  Note it forwards the header value **verbatim**, so the client sends the full `Bearer …` string.

**The token is not a CloudTAK-verified JWT** — it is opaque to CloudTAK, stored and replayed. Your server can make it any opaque string.

### 5.3 Subscribe / unsubscribe

- User: `PUT /Marti/api/missions/{guid}/subscription?uid=ANDROID-CloudTAK-{email}` → `TAKItem(MissionSubscriber)`, `data.token` captured.
- Connection: on every `secureConnect`, the pool re-subscribes all of that Connection's Data Syncs:
  ```ts
  for (const sub of await connConfig.subscriptions()) {
      await api.Mission.subscribe(sub.guid || sub.name, { uid: connConfig.uid() }, { token: sub.token || undefined });
  }
  ```
  with a retry loop that **only retries on `ECONNREFUSED`** — *"We don't retry for unknown issues as it could be the Sync has been remotely deleted and will retry forever"*. So a transient 5xx here silently drops the subscription until the next reconnect.
- Unsubscribe: `DELETE /Marti/api/missions/{guid}/subscription?uid=ANDROID-CloudTAK-{email}`.

### 5.4 GUID vs name addressing

```ts
export const GUIDMatch = new RegExp(/^[{]?[0-9a-fA-F]{8}-([0-9a-fA-F]{4}-){3}[0-9a-fA-F]{12}[}]?$/);
#isGUID(id: string): boolean { return GUIDMatch.test(id) }
#encodeName(name: string): string { return encodeURIComponent(name.trim()) }
```

The identifier is **sniffed**: if it matches the GUID regex (optionally brace-wrapped) it routes to `/Marti/api/missions/guid/{id}/…`; otherwise `/Marti/api/missions/{encodedName}/…`.

**Hard consequence: a mission whose *name* is a bare UUID is unaddressable by name.** If your server lets users name a mission `550e8400-e29b-41d4-a716-446655440000`, CloudTAK will look it up by GUID and 404.

Three methods have **no GUID variant** and always use the name path — `setKeywords`, `deleteKeyword`, `getArchive` — and two resolve GUID→name first (`upload`, and `update` via `get`). `delete` is the odd one out: the GUID form is `DELETE /Marti/api/missions?guid={guid}`, a **query parameter on the collection**, not a path segment.

CloudTAK consistently prefers GUIDs (`data.mission_guid || data.name`, `profile_overlays.mode_id`), so **implement the `/guid/` family first**; the name family is the fallback and is required for keywords/archive/create.

### 5.5 Content upload flow

Two-step, as you suspected ([`marti-mission.ts`](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/routes/marti-mission.ts) `POST/PUT /marti/missions/:guid/upload`):

1. `api.Files.upload({ name, contentLength, contentType, keywords, creatorUid, uid?, lat/lon/alt? }, body)` → `POST /Marti/sync/upload?…` → returns a `Content` with a `Hash`.
2. `api.Mission.attachContents(guid, { hashes: [hash] }, opts)` → `PUT /Marti/api/missions/{guid}/contents`.

Removal: `api.Mission.detachContents(guid, { hash })` → `DELETE /Marti/api/missions/{guid}/contents?hash=`.

Note the `uid` option on upload: *"TAK clients treat a Mission file whose UID matches a map item as an attachment of that item"* — that's how CloudTAK implements per-marker attachments.

Whole-package upload is the separate `PUT /Marti/api/missions/{name}/contents/missionpackage?creatorUid=` (raw stream).

### 5.6 Mission layers

`DataMission.sync` maintains one `UID`-type mission layer per ETL Layer, keyed `uid: \`layer-${l.id}\``, capped at `MAX_LAYERS_IN_DATA_SYNC = 5`. It lists existing layers, creates missing ones, and **deletes-then-recreates any layer whose `type` is not `UID`**. `connection-layer-cot.ts` additionally builds a path→layer map (`listAsPathMap`), auto-creating nested layers and garbage-collecting empty ones via `MissionLayer.isEmpty()`.

---

## 6. Other Marti endpoints — essential vs optional

Verified by enumerating every `api.<Module>.<method>(` call across all 71 route files and all lib files.

### Tier 1 — CloudTAK is unusable without these

| Endpoint | Why |
|---|---|
| `POST {webtak}/oauth/token` | login |
| `GET {webtak}/Marti/api/tls/config` | enrolment prerequisite (XML) |
| `POST {webtak}/Marti/api/tls/signClient/v2` | per-user cert issuance |
| `GET /files/api/config` | **server setup blocks on this** |
| `GET /Marti/api/version` | probed on **every** login and session check |
| `GET /Marti/api/groups/all` | channels; called on every connect, every package page, every group-scoped op |
| `PUT /Marti/api/groups/active` | channel toggling; **force-called before every Data Sync create** |
| TLS 8089 accepting client certs, `t-x-c-t` → `t-x-c-t-r` | connection shows `dead` otherwise |

### Tier 2 — a core page breaks without these

| Endpoint | Page |
|---|---|
| `GET /Marti/api/contacts/all` | contacts list |
| `GET /Marti/api/missions` | Data Sync / mission list |
| `GET /Marti/api/missions/invitations?clientUid=` | called **in parallel with** the mission list on the same page — a failure here fails the whole page |
| `GET/POST/DELETE /Marti/api/missions/{guid\|name}` + `/guid/{guid}` | mission CRUD |
| `PUT/DELETE /Marti/api/missions/{n}/subscription` | subscribing to a Data Sync |
| `GET /Marti/api/missions/{n}/cot` | mission feature rendering (XML) |
| `GET /Marti/sync/search` | data packages list |
| `POST /Marti/sync/upload`, `POST /Marti/sync/missionupload`, `GET /Marti/sync/content` | package/attachment I/O |
| `GET /Marti/api/files/metadata?missionPackage=true&name=` | package channel column |
| `PUT /Marti/api/missions/{n}/contents`, `DELETE …?hash=` | attachments |
| `GET /Marti/api/clientEndPoints` | live client list |

### Tier 3 — feature pages, degrade gracefully

`/Marti/api/missions/{n}/{changes,layers,layers/*,role,subscriptions,subscriptions/roles,contacts,archive,invite/*,keywords}`, `/Marti/api/missions/logs/entries*`, `/Marti/api/cot/xml/{uid}[/all]`, `/Marti/ExportMissionKML`, `/Marti/api/sync/metadata/{hash}/{keywords,expiration}`, `/Marti/sync/delete`, `/Marti/api/files/{hash}`.

### Tier 4 — admin-only

`/Marti/api/subscriptions/all`, `/Marti/api/injectors/cot/uid[/{uid}]`, `/Marti/api/repeater/{list,period,remove/{uid}}`, `/Marti/api/certadmin/cert*`.

### Tier 5 — video, stub as empty

`/Marti/api/video[/{uuid}]`. Return `{"videoConnections": []}` for the list; the UI renders an empty video panel. Known-broken upstream anyway (§8).

### Explicitly **NOT** used — do not bother implementing for CloudTAK

From your candidate list, I verified these have **zero references** in CloudTAK or node-tak's CloudTAK-exercised paths:

- `/Marti/api/version/config` — **not called.** Only bare `/Marti/api/version`.
- `/Marti/api/util/user/roles` — not called.
- `/Marti/api/security/config`, `/Marti/api/security/isSecure`, `/Marti/api/security/verifyConfig`, `/Marti/api/authentication/config` — exist in node-tak (`security.ts`), unused by CloudTAK.
- `/Marti/api/qos/...` — **does not exist in node-tak at all.** (`qos` appears only in node-CoT's protobuf and tak-infra's CoreConfig.)
- `/Marti/api/device/profile/...` — exists in node-tak (`profile.ts`), unused by CloudTAK.
- `/Marti/api/missions/all/invitations` — **not used**; CloudTAK uses `/Marti/api/missions/invitations?clientUid=`.
- `/Marti/api/inject` — the real path is `/Marti/api/injectors/cot/uid`.
- `/Marti/api/repeaters` — the real paths are `/Marti/api/repeater/list` etc. (singular).
- `/Marti/api/sync/search` — defined in `files.ts#list()` but never called; CloudTAK uses `/Marti/sync/search`.
- `/Marti/api/pagedmissions` — supported by node-tak, not used by CloudTAK.
- `/Marti/api/iconset/all/uid`, `/locate/api`, `/Marti/api/user-management/*` — unused.

---

## 7. OpenTAKServer feature comparison

Source: [docs.opentakserver.io/feature_comparison.html](https://docs.opentakserver.io/feature_comparison.html) (OTS 1.7.0). Columns are **OpenTAKServer / FreeTAKServer / TAKServer**:

| Feature | OTS | FTS | TAK Server |
|---|---|---|---|
| TCP | Yes | Yes | Yes |
| SSL | Yes | Yes | Yes |
| Automatic Certificate Authority Generation | Yes | No | No |
| Certificate Enrollment | Yes | No | Yes |
| Federation | No (Coming in 1.7.0) | Yes | Yes |
| Data Packages | Yes | Yes | Yes |
| DataSync | Yes | Yes | Yes |
| ExCheck | Coming Soon | Yes | Yes |
| Video Streaming | Yes | Yes | Installed Separately |
| Video Stream Recording/Playback | Yes | No | No |
| Mumble Server Authentication | Yes | No | No |
| Web UI | Yes | Yes | Yes |
| EUD Authentication | Yes | No | Yes |
| ADS-B Data From Airplanes.live | Yes | No | No |
| AIS Data From AISHub.net | Yes | No | No |
| Database | SqlAlchemy (PostGIS default) | SqlAlchemy (SQLite default) | PostgreSQL |
| Runs on Raspberry Pi | Yes | Yes | Yes |
| Meshtastic support | Yes | No | No |
| Update Server | Yes | No | No |
| Device Profiles | Yes | No | Yes |
| Groups/Channels | Yes | No | Yes |
| Actively Developed | Yes | No | Yes |

No footnotes on the page.

**Mapping onto your CloudTAK requirement:** the rows that actually gate CloudTAK are **SSL**, **Certificate Enrollment**, **EUD Authentication**, **Groups/Channels**, **DataSync**, **Data Packages**. **Federation**, **ExCheck**, **Device Profiles**, **Mumble**, **ADS-B/AIS**, **Meshtastic**, **Update Server** are all irrelevant to CloudTAK. **Video Streaming** is nominally required but broken in practice everywhere (§8).

The canonical **vocabulary** worth adopting: *EUD* (End User Device), *Channel* = group = `bitpos`-addressed, *Data Sync* = Mission, *Data Package* = zip in the file store, *Certificate Enrollment* = the `signClient/v2` flow, *Device Profile* = the enrolment-time settings bundle, *Federation* = server-to-server CoT bridging.

---

## 8. Known gotchas

### 8.1 Content-Type must be **exactly** `application/json`

The single highest-risk incompatibility. [`node-tak/lib/api.ts`](https://github.com/dfpc-coe/node-tak/blob/main/lib/api.ts):

```ts
if (res.headers.get('content-type') === 'application/json') {
    return await res.json();
} else {
    return await res.text();
}
```

**Strict string equality.** `application/json;charset=UTF-8` or `application/json; charset=utf-8` falls into the `else` branch and returns a **string**. Callers that then do `missions.data[0]` get `TypeError: Cannot read properties of undefined`. Real TAK Server evidently trips this, which is why two call sites carry defensive parses:

```ts
// package.ts
if (typeof res === 'string') {
    // The TAK Server API doesn't return application/json
    return JSON.parse(res) as { resultCount: number; results: ... }
// files.ts upload()
return typeof res === 'string' ? JSON.parse(res) : res;
```

but **`Mission.*`, `Group.*`, `MissionLayer.*`, `Contacts.*`, `Client.*` have no such fallback**. Emit bare `Content-Type: application/json` with no parameters from every Marti JSON endpoint.

### 8.2 The JWT parser (see §2.1)
Header must be a multiple of 3 bytes; payload must contain no nested braces; only `sub` is read; no signature check.

### 8.3 Status-code handling
```ts
if ((res.status < 200 || res.status >= 400)) { /* throw */ }
```
**3xx is treated as success** and the redirect body is parsed as the payload. Never redirect on a Marti API call. Also, non-2xx bodies are sniffed for HTML (`isHTML`) and wrapped in `TAKServerError` with a parsed summary ([`lib/utils/html-error.ts`](https://github.com/dfpc-coe/node-tak/blob/main/lib/utils/html-error.ts)) — that path exists specifically because TAK Server returns Tomcat HTML error pages. A plain-text or JSON error body works fine too; it's just used as the message.

### 8.4 `Certificate.probe()` regex classification
Covered in §2.1 — an error that matches none of `RevokedException|revoked certificate`, `BadCredentialsException|AuthenticationException|TAK Server authentication`, or the TLS-ish set is **rethrown**, surfacing as a hard error instead of a re-login prompt.

### 8.5 TLS asymmetry
`webtak` requires a **publicly trusted** certificate (undici fetch, full verification); `api` and the 8089 stream do not (`rejectUnauthorized: false`). Tracked as [CloudTAK #983 — "Ability to provide Self-signed SSL option"](https://github.com/dfpc-coe/CloudTAK/issues/983) (open). The [OpenTAKServer CloudTAK guide](https://docs.opentakserver.io/cloudtak.html) accordingly mandates Let's Encrypt certs on **four** domains: `ots.example.com`, `cloudtak.example.com`, `tiles.cloudtak.example.com`, `video.example.com`.

### 8.6 The `Groups` capitalisation bug
From `files.ts#uploadPackage`:
```ts
// This is intentionally case sensitive due to an apparent bug in TAK server
url.searchParams.append('Groups', group);
```
Accept **both** `Groups` and `groups` on `/Marti/sync/missionupload`.

### 8.7 `allowGroupChange` 403
From `mission.ts#update`:
```ts
// Group changes by non-admin Mission Owners require `allowGroupChange=true`
// on the request, or TAK Server will respond with 403.
// See MissionApi.java in takserver-core for the server-side check.
if (body.group !== undefined && body.allowGroupChange === undefined) {
    body.allowGroupChange = true;
}
```
Accept and ignore `allowGroupChange=true`, or replicate the check.

### 8.8 Unknown-hash reported as 500
```ts
// The TAK Server reports an unknown hash via sendError(500), which is
// indistinguishable from a genuine failure
```
on `GET /Marti/api/certadmin/cert/{hash}`. Admin-only path; low impact.

### 8.9 Video is broken against every server
- [CloudTAK #1347 — "Remote Video Connections Write Malformed Entries to TAKServer"](https://github.com/dfpc-coe/CloudTAK/issues/1347): CloudTAK writes video feeds via the Marti API with malformed values (notably `port: -1`), causing **iTAK to fail downloading the feed list with "unknown error"**. Described as a fundamental design mismatch.
- [OpenTAKServer CloudTAK docs](https://docs.opentakserver.io/cloudtak.html): *"Video streaming currently doesn't work"* and *"Uploading files and data packages doesn't work"*.
- [CloudTAK #585 "OTS: No Video playback"](https://github.com/dfpc-coe/CloudTAK/issues/585) — closed/completed.

**Recommendation: return `{"videoConnections": []}` and don't accept writes.** You avoid the malformed-entry bug entirely.

### 8.10 Other observed integration bugs
- [CloudTAK #584 — "OTS: No persistance on CloudTAK breadcrumbs"](https://github.com/dfpc-coe/CloudTAK/issues/584) (closed) — CoT history persistence differences; relates to `/Marti/api/cot/xml/{uid}/all`.
- [CloudTAK #1160 — "ETL with Data Sync sends CoT to TAK Server instead of only to Mission"](https://github.com/dfpc-coe/CloudTAK/issues/1160) — when an ETL layer targets a Data Sync with mission sync enabled, the `<dest mission="...">` routing is ignored and the CoT is broadcast. A CloudTAK-side bug, but it means **your server will receive broadcast CoT that was meant for a mission** — don't assume `dest` is always present.
- OTS's own docs list `/Marti/api/tls/signClient/v2` under **PUT**, while node-tak issues **POST**. Accept both verbs.
- OTS notes: *"on most Marti API endpoints, OpenTAKServer will look for the client certificate in an HTTP header to identify the username via the certificate's common name"* — a reverse-proxy-termination pattern. If you terminate TLS behind a proxy, you need the same. ([OTS Marti API docs](https://docs.opentakserver.io/marti_api.html))
- Support routing: the OTS docs ask users of CloudTAK-on-OTS to use the **OTS Discord**, not COTAK — i.e. upstream does not support non-official servers.

### 8.11 Things that will bite that nobody has filed yet
From reading the code rather than the issue tracker:

1. **Mission named as a UUID is unaddressable** (§5.4).
2. **`nameEntry` must be an array** in `tls/config` XML, or `xml-js` compact mode yields an object and the `for...of` throws (§2.1 step 3a).
3. **`signedCert` must be bare base64**, no PEM armour — node-tak concatenates the armour itself.
4. **Self-closing `<event/>` never parses** — `REGEX_EVENT` requires `</event>`.
5. **15-second TLS handshake budget** on 8089 (`awaitSecure`).
6. **Subscription retry only on `ECONNREFUSED`** — a 5xx on `PUT .../subscription` silently drops a Data Sync until the next reconnect.
7. **`Contacts.list()` returns a bare array**, not a `TAKList` envelope — unlike almost everything else.
8. **`Mission.create` comma-joins `group`; `Mission.update` repeats it.** Accept both `?group=a,b` and `?group=a&group=b`.
9. **`Mission.delete` by GUID uses `?guid=` on the collection**, not `/guid/{id}`.
10. **`/Marti/api/repeater/remove/{uid}` is a GET**, not a DELETE.
11. **`Mission` requires `externalData`, `feeds`, `mapLayers`, `uids`, `contents` as arrays** (non-optional in the type) — emit `[]` rather than omitting them.

---

## Suggested implementation order

1. `GET /files/api/config` → `{"uploadSizeLimit": N}` — unblocks setup.
2. `POST /oauth/token` (password grant) + a 3-byte-aligned, flat-payload JWT with `sub`.
3. `GET /Marti/api/tls/config` (XML, ≥2 `nameEntry`) + `POST /Marti/api/tls/signClient/v2` (Basic auth, bare-base64 `signedCert`).
4. `GET /Marti/api/version` under mTLS, plain text.
5. `GET /Marti/api/groups/all` + `PUT /Marti/api/groups/active`.
6. TLS 8089 accepting client certs, XML CoT framing, `t-x-c-t` → `t-x-c-t-r`.
7. `GET /Marti/api/contacts/all`, `GET /Marti/api/clientEndPoints`.
8. Missions: list, get (name + `/guid/`), create (with `token` + `guid`), subscribe/unsubscribe, `/cot`, `/changes`, plus `t-x-m-c` push.
9. File store: `/Marti/sync/{upload,missionupload,content,search}`, `/Marti/api/files/metadata`, mission `contents` attach/detach.
10. Mission layers, logs, invitations, roles.
11. Stub video as `{"videoConnections": []}`.

**Sources:**
- [dfpc-coe/CloudTAK](https://github.com/dfpc-coe/CloudTAK) — [login.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/routes/login.ts), [provider.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/lib/provider.ts), [server.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/routes/server.ts), [schema.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/common/schema.ts), [connection-config.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/common/connection-config.ts), [connection-pool.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateful/lib/connection-pool.ts), [data-mission.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/lib/data-mission.ts), [tak-channels.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/lib/tak-channels.ts), [marti-mission.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/routes/marti-mission.ts), [marti-package.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/stateless/routes/marti-package.ts), [defaults.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/common/defaults.ts), [Login.vue](https://github.com/dfpc-coe/CloudTAK/blob/main/api/web/src/components/Login.vue), [atlas-connection.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/web/src/workers/atlas-connection.ts), [login-oidc.srv.test.ts](https://github.com/dfpc-coe/CloudTAK/blob/main/api/test/login-oidc.srv.test.ts)
- [dfpc-coe/node-tak](https://github.com/dfpc-coe/node-tak) — [index.ts](https://github.com/dfpc-coe/node-tak/blob/main/index.ts), [lib/api.ts](https://github.com/dfpc-coe/node-tak/blob/main/lib/api.ts), [lib/auth.ts](https://github.com/dfpc-coe/node-tak/blob/main/lib/auth.ts), [lib/api/](https://github.com/dfpc-coe/node-tak/tree/main/lib/api)
- [dfpc-coe/node-CoT](https://github.com/dfpc-coe/node-CoT) — [lib/cot.ts](https://github.com/dfpc-coe/node-CoT/blob/main/lib/cot.ts), [lib/types/types.ts](https://github.com/dfpc-coe/node-CoT/blob/main/lib/types/types.ts), [lib/parser.ts](https://github.com/dfpc-coe/node-CoT/blob/main/lib/parser.ts)
- [dfpc-coe/tak-infra](https://github.com/dfpc-coe/tak-infra) — [CoreConfig.base.xml](https://github.com/dfpc-coe/tak-infra/blob/main/CoreConfig.base.xml), [src/CoreConfigType.ts](https://github.com/dfpc-coe/tak-infra/blob/main/src/CoreConfigType.ts)
- [dfpc-coe/auth-infra](https://github.com/dfpc-coe/auth-infra)
- [OpenTAKServer feature comparison](https://docs.opentakserver.io/feature_comparison.html), [CloudTAK guide](https://docs.opentakserver.io/cloudtak.html), [Marti API](https://docs.opentakserver.io/marti_api.html)
- CloudTAK issues [#983](https://github.com/dfpc-coe/CloudTAK/issues/983), [#1347](https://github.com/dfpc-coe/CloudTAK/issues/1347), [#1160](https://github.com/dfpc-coe/CloudTAK/issues/1160), [#585](https://github.com/dfpc-coe/CloudTAK/issues/585), [#584](https://github.com/dfpc-coe/CloudTAK/issues/584)

---

**Two caveats on completeness.** I could not verify (a) that a `GET /api/login/oidc` backend handler exists anywhere — it is referenced by the frontend but absent from `main`, so the OIDC browser flow appears unimplemented upstream; and (b) the exact HTTP status codes real TAK Server returns for several endpoints, since I worked from the client side only — node-tak's error handling is permissive (anything outside 200–399 throws) so this mostly doesn't matter, with the exception of `/oauth/token`, where the 401/403 vs 400-with-`invalid_grant` distinction changes the user-facing message. The GitHub code-search API rate-limited partway through, so my "not used" claims are based on exhaustively downloading and grepping all 71 route files plus all lib/control files rather than on search results.
