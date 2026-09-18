# OAuth2 / login (`/oauth/token`, `/login/*`) — wire contract

**Purpose.** The password-grant token endpoint CloudTAK's login and enrollment both depend on, and
the JWT shape every bearer-authenticated Marti call must accept. Served on the public HTTPS listener
(`:8446`). Contract for `rustak-server::auth::oauth_server::{token,jwt}`.

Baseline: `plan.md` Appendix A.2 and A.4 "OAuth". This file expands both. Recall the architecture
decision (`plan.md` "Decisions" table): **rustak is the identity authority** — a full OAuth2 server
(password grant + authorize-code flow federating to an OIDC IdP), not a thin proxy in front of an
LDAP bind like TAK Server typically is. The wire shapes below are what CloudTAK/ATAK require; how
rustak validates the password behind them (OIDC-backed client password, or the OIDC authorize-code
flow for the admin UI) is rustak's own design, covered in `conventions.md` and the identity/auth
module tree in `plan.md`.

## 1. `POST /oauth/token` — password grant

```
POST /oauth/token
Content-Type: application/x-www-form-urlencoded

grant_type=password&username=<user>&password=<client-password-or-enrollment-token>
```
- **`grant_type=password` is the only grant TAK Server serves here**, and the only one either
  verified client uses. No `client_id`/`client_secret`/`scope` are required or meaningfully
  consulted by either of them (CloudTAK never sends them; TAK Server itself ignores them if
  present) — accept them as no-ops if a client sends them, never require them.
  - **Correction (M5-01).** rustak also serves `grant_type=refresh_token` and
    `grant_type=authorization_code` at this same path, because it is a real authorization server
    rather than a proxy in front of an LDAP bind. Both are **additive**: the password grant's
    request and response are byte-for-byte what is described below, and neither of the other two is
    reachable without a credential a password-grant client does not have. `client_id` **is**
    required for `authorization_code` — it is one of the three bindings a code carries — and stays
    a no-op for the other two.
- `password` here is rustak's opt-in, expiring **client password** credential (`conventions.md`
  security defaults) — the only reusable secret rustak issues, scoped to exactly this endpoint and
  `POST /Marti/api/tls/signClient/v2` (`enrollment.md`).

**Success — `200`:**
```json
{ "access_token": "<jwt>", "token_type": "Bearer", "expires_in": 1800 }
```
No `refresh_token`, no `scope` field in the body (verified 06 §4.2 — TAK Server's own
implementation never issues either from this grant). rustak's own token design does issue rotating
refresh tokens elsewhere (admin UI login, `plan.md` "Admin UI auth" decision) — but **not** from
this password-grant response, to stay byte-compatible with what CloudTAK's parser expects.

**Failure:**
```json
{ "error": "invalid_grant" }
```
`400` for `invalid_grant`/`invalid_request`/`unsupported_grant_type`, `401` for `invalid_client`.
CloudTAK's own client additionally special-cases a response body containing
`"error_description":"Bad credentials..."` into a friendlier message — emitting a plain
`invalid_grant` with any `error_description` is sufficient; the exact wording isn't parsed for
logic, only sniffed for that one substring. CloudTAK treats **any** non-2xx status (including 3xx)
as failure and requires the body to parse as JSON — never redirect this endpoint. Verified 06 §4.2,
§4.4; 03 §2.1 step 1.

## 2. JWT shape — the part that actually matters

This is the single most fragile compatibility surface in the project. CloudTAK's JWT "parser" does
**no signature verification and no JWKS fetch** — it does not call `/oauth/token_key`, does not know
about JWKS, and trusts the token purely because it arrived over the TLS channel to `webtak`. Its
parsing logic instead does this (verbatim behaviour, not copied code — 03 §2.1 step 2):

1. Base64-decodes the **entire JWT string as one blob** (not just the payload segment) — Node's
   base64 decoder silently drops the `.` separators, concatenating header + payload + signature
   bytes together.
2. That only lands on a clean payload boundary if the **header's base64url encoding is a multiple
   of 4 characters**, which requires the header JSON to be an exact multiple of 3 bytes.
3. It then finds the payload by **splitting on the first `}`** — so the payload segment must contain
   **no nested object and no `}` before its own closing brace**. Flat scalar (and flat-array) claims
   only.
4. Only the **`sub`** claim is actually read and used (as the CloudTAK profile key / username).
   `aud`, `nbf`, `exp`, `iat` are declared in CloudTAK's type but never runtime-checked.

**rustak's JWT header is fixed at exactly 27 bytes** (`{"alg":"RS256","typ":"JWT"}` —
`conventions.md`, `plan.md` "Our JWT" decision), which base64url-encodes to a clean multiple of 4
characters — this satisfies constraint (2) by construction; do not add or remove header fields
without re-verifying the byte count. **Claims must be flat** (`conventions.md` "flat claims only")
— satisfies constraint (3). `sub` must be the **username** CloudTAK/ATAK will treat as the account
identity (constraint 4) — for the password-grant token this is the authenticated username, matching
the account the client password belongs to.

## 3. Bearer resolution for Marti calls

CloudTAK sends `Authorization: Bearer <access_token>` when calling `GET /Marti/api/tls/config`
during enrollment (`enrollment.md` §1), and separately expects `GET /Marti/api/version` to succeed
under **client-cert** auth once enrolled — the OAuth bearer is not used for ongoing Marti API calls
after enrollment; the client cert is. rustak's bearer-vs-cert-vs-mission-token resolution order
(`auth::resolve`, per-listener policy) should treat a `Bearer` token as: try RS256 verification
first (our own issued JWT), and only if that fails, consider it a mission-token candidate (see
`missions.md` §10 for the header-precedence rule there — `MissionAuthorization` first, then
`Authorization`). TAK Server itself only honours OAuth bearer tokens on its two non-mTLS ports
(`:8446`/`:8447`), never on the mTLS `:8443` port, specifically so a mission token in `Authorization`
on `:8443` is never misread as an OAuth token — rustak's own port layout (`plan.md` "Listeners")
achieves the same separation structurally, since Marti is fully served on both `:8443` (cert-only)
and `:8446` (bearer-or-cert): apply the same rule — a bearer token is only meaningful for a request
that also permits non-cert auth, and mission tokens always take the `MissionAuthorization`-first
path regardless of port. Verified 06 §4.7.

## 4. `/login/*` — background on TAK Server's shape, and what CloudTAK actually needs

CloudTAK's open-source `main` branch has **no working OIDC client today** — its login form always
posts `POST /api/login {username, password}` to itself, which in turn calls `/oauth/token` as in §1.
The `oidc::enabled`/`oidc::enforced` config only gates whether that password form is *shown*
(enforced-but-no-backend ⇒ users are stuck) — there is no `GET /api/login/oidc` handler in
CloudTAK to redirect anywhere. **Practical consequence**: CloudTAK's browser session never talks
OIDC to rustak directly; it always uses the resource-owner-password flow in §1, with rustak doing
the real OIDC validation behind the scenes against the configured IdP. Verified 03 §2.2.

rustak still implements the fuller `/login/*` surface (`plan.md` "OIDC / CloudTAK" decision, and the
`auth/oidc/` + `oauth_server/authorize.rs`/`login.rs` module tree) for **ATAK/WinTAK-style external
OIDC federation** and for the admin UI's own SSO path — that is TAK Server's `/login/auth` →
IdP → `/login/redirect?code&state` → cookie flow, reproduced structurally (state cookie,
`sha256(state)` comparison, `authorization_code` exchange) but is not something CloudTAK's
verified traces exercise. Treat this half of the surface as **ATAK/admin-UI scope**, not a
CloudTAK interop gate; see `research/06-takserver-http-api-verified.md` §4.6 for the full endpoint
shapes if implementing byte-for-byte TAK Server parity here.

### What M5-01 built, and where it deliberately differs from TAK Server

Implemented in `auth/oauth_server/{authorize,login,session,state,codes,cookies}.rs`, mounted by
`marti/login.rs`, and exercised by `rustak-server/tests/oauth_flows.rs`. The shapes a client reads
are TAK Server's; the security properties are stricter, and every difference below is deliberate.

| | TAK Server | rustak |
|---|---|---|
| `state` | cookie, `sha256(cookie) == state` at the callback | same rule, **plus** a one-shot server-side record (ten minutes) keyed by that digest |
| Provider's code | redeemed with `client_id` + `client_secret` | the same, **plus** our own PKCE `S256` verifier |
| ID token | verified against `<key>` elements or an `issuer` read as a **file path** | verified against the provider's published JWKS, RS256 only, with a **`nonce`** we issued |
| `access_token_N` cookies | `HttpOnly`, `Path=/`, `Secure` when the request was, `SameSite=Strict` | `HttpOnly`, `Path=/`, **always `Secure`**, `SameSite=Lax` |
| `state` cookie | `Max-Age=-1`, not secure-forced, unscoped | `HttpOnly`, `Secure`, `SameSite=Lax`, **`Path=/login`** |
| Refresh token | kept in the servlet **HTTP session** | not stored in a cookie at all; renewal is `POST /oauth/token` `grant_type=refresh_token` |
| `/logout` | `301` to `/webtak/index.html`, expires `access_token*` | `204`, expires `access_token*` **and** revokes the `jti` and the refresh family |
| `/login/redirect` failure | forwards to `/Marti/login/*.html` | one generic `400` for every cause, with the `state` cookie cleared |
| Bearer/cookie scope | port-gated (`:8446`/`:8447` only, per `AccessTokenResolver`) | **path**-gated: `/login/*`, `/logout`, `/token/access`, `/oauth/authorize`, `/Marti/**`, `/files/api/**` — never `/api/v1` |

Two TAK endpoints are **not** implemented: `GET /login/refresh` (the servlet-session refresh
rustak has no equivalent state for — clients renew through `/oauth/token` instead), and the
`webtakScope` / `webtak-role-error.html` branch of `/login/redirect`, which gates access on a
scope claim rustak expresses with `[auth] user_acl` instead.

`GET /oauth/authorize` is rustak's own and has no TAK Server counterpart: registered clients come
from `[auth.oauth] clients`, PKCE `S256` is mandatory for public clients, `redirect_uri` is matched
byte for byte, an unregistered client or redirect URI is refused **without** redirecting (so the
endpoint can never be used as an open redirector), and there is no consent screen because every
registered client is first-party by construction.

## 5. Group-claim mapping (OIDC groups → rustak groups)

When mapping OIDC `groups` claim values onto rustak groups (`identity::provisioning`,
`groups.md`'s group model): a **bare** group name grants both `IN` and `OUT`; a name ending in the
configured **read suffix** (default `_READ`) grants `OUT` only (with the suffix stripped from the
stored group name); a name ending in the **write suffix** (default `_WRITE`) grants `IN` only. This
is TAK Server's own convention (`groupsClaim` default `"groups"`, suffixes configurable) and rustak
adopts it directly per `plan.md`'s group-mapping decision — use the **first** occurrence of the
suffix when stripping (a name like `A_READ_B_READ` truncates at the first `_READ`, not the last).
Verified 06 §4.7 step 5.

## Gotchas

- The JWT header byte-length constraint in §2 is not cosmetic — get it wrong and CloudTAK's login
  fails with `Unexpected TAK JWT Format` even though the token is perfectly valid RS256. Any change
  to `conventions.md`'s fixed header must re-verify the base64 alignment.
- Claims must stay flat — a nested object claim (e.g. an `authorities: {...}` object rather than a
  flat array) breaks CloudTAK's brace-splitting parser outright.
- Never redirect `/oauth/token` — a 3xx here is treated as a parseable success by CloudTAK and will
  fail confusingly downstream.
- `/oauth/token`'s response has **no `refresh_token`** — don't leak rustak's internal refresh-token
  model into this specific response shape even though rustak supports refresh tokens elsewhere.
  The `refresh_token` and `authorization_code` grants at the same path *do* return one; only the
  password grant's body is pinned.
- The `access_token_N` cookies are the same names TAK Server writes, so a page that reassembles
  them works unchanged — but they are `SameSite=Lax` here rather than `Strict`, because `Strict`
  would drop the cookie on the top-level navigation `/login/redirect` and `/oauth/authorize` are.
  `Lax` still withholds it from every cross-site `POST` and `fetch`.
- Bearer tokens are meaningful only where the listener policy allows non-cert auth — never accept a
  bearer token as authentication on a route that's supposed to be mTLS-only, mirroring TAK Server's
  own port-gating rationale (§3).

## Verified in

- `research/06-takserver-http-api-verified.md` §4 (`PasswordGrantAuthenticationConverter`/
  `Provider`, exact success/error JSON, JWT header/claims, bearer resolution, `/login/*` endpoint
  table, group-claim mapping) — authoritative for the wire shapes.
- `research/03-cloudtak-node-tak-contract.md` §2.1–§2.2 (CloudTAK's login flow, the JWT
  byte-alignment/flat-claims parsing behaviour, the "no OIDC client in CloudTAK today" finding) —
  authoritative for CloudTAK.
- `plan.md` "Decisions" (OIDC/CloudTAK), "Identity & auth model" (Our JWT), Appendix A.2, A.4
  "OAuth" — baseline digest and rustak's own architectural decisions, expanded here.
