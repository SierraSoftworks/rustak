# CI-01 — `mp-download` fails because the mission-package URL is built from `[server] name`

**Found by:** nightly `interop-eud`, run
[35376450618](https://github.com/SierraSoftworks/rustak/actions/runs/35376450618),
job `Interop: EUD harness`, scenario `mp-download` (sha `b802803`).
Not a harness problem — the harness is doing what M2-09 §4 said it would, and the
answer it got back is wrong.

**Owner:** whoever holds `rustak-server/src/marti/sync_read.rs` and
`rustak-server/src/web/helpers/request.rs` (M3-01 enterprise sync). CI-01 does not
edit non-test `src/`.

---

## 1. What the run shows

ALPHA uploads a mission package addressed to BRAVO, and rustak accepts it:

```
[alpha] Mission package 0 to EUD-MPDL-BRAVO, completed upload to server! 1282 bytes uploaded,
        URL is https://rustak-interop-eud-mp-download/Marti/sync/content?hash=e661814544d5…
```

`rustak-interop-eud-mp-download` is the scenario server's **`[server] name`** — a
display name, not a host. BRAVO is then told to fetch that URL and cannot:

```
[bravo] Receive of MP /work/payload.dat from MPDL-ALPHA requested - assigned output file …
[bravo] Receive of MP … attempt 2 of 10 FAILED! Bytes 0 of 1052
… ten attempts, 0 of 1052 bytes every time
```

So the failure the scenario reports —
`[bravo] commo-log.txt has no line matching /Receive of MP .* result OK/` — is
accurate, and the upload half (`mp-upload`) passes because nothing there has to
resolve the URL.

## 2. Why the URL came out that way

`rustak-server/src/marti/sync_read.rs:221`:

```rust
pub fn content_url(request: &HttpRequest, context: &AppContext, hash: &str) -> String {
    let config = context.config();
    let base = match &config.marti.public_host {
        Some(host) => format!("https://{host}"),
        None => crate::web::helpers::request::request_base_url(config.server.trust_proxy, request)
            .unwrap_or_else(|| format!("https://{}", config.server.name)),
    };

    format!("{base}/Marti/sync/content?hash={hash}")
}
```

`[marti] public_host` is unset in the scenario, so it falls to `request_base_url`,
which returned `None` — and the last resort is `config.server.name`.

`request_base_url` returns `None` **for every HTTP/2 client**, which is what ATAK
is here. `rustak-server/src/web/helpers/request.rs:107` delegates to
`base_url_from`, whose first act is:

```rust
let host = if trust_proxy {
    leftmost(headers, "x-forwarded-host").or_else(|| header_str(headers, "host"))
} else {
    header_str(headers, "host")
}?;                                  // ← `None` when there is no `host` header
```

Over HTTP/2 there is no `host` header: the authority travels as the `:authority`
pseudo-header and actix puts it on the request URI, not into `HeaderMap`. The
run's own logs show it directly — every request from the node runner carries
`host: "localhost:43025"` in `http.headers`, and every request from `commotest`
carries `authorization`, `accept`, `content-type` and `content-length` **and no
`host`**. The listener advertises `h2` via ALPN and libcurl takes it.

## 3. The fix I believe is right

Two independent changes; the first is the bug, the second is the safety net.

1. **`request_base_url` should read the authority, not only the header.** Over
   HTTP/1.1 the `Host` header is the authority; over HTTP/2 it is
   `request.uri().authority()`. Falling back to it keeps the existing
   `trust_proxy` precedence intact:

   ```rust
   pub fn request_base_url(trust_proxy: bool, request: &HttpRequest) -> Option<String> {
       let secure = request.app_config().secure();

       base_url_from(trust_proxy, request.headers(), secure.then_some("https")).or_else(|| {
           // HTTP/2 carries the authority as `:authority`, which actix puts on
           // the URI rather than into the header map, so a request from any h2
           // client has no `host` header at all.
           let authority = request.uri().authority()?.as_str();
           Some(format!("{}://{authority}", if secure { "https" } else { "http" }))
       })
   }
   ```

   Deliberately *not* `request.connection_info()`: its doc comment already
   records why (it consults the forwarding headers whether or not a proxy is
   trusted), and that reasoning still holds.

2. **The last resort should be a URL, not a name.** `config.server.name` is a
   display name — `config.example.toml` documents it as such and the wizard
   writes whatever the operator typed. `config.server.base_url` is the field
   that *is* an external URL. Suggested:

   ```rust
   None => request_base_url(config.server.trust_proxy, request)
       .or_else(|| config.server.base_url.clone())
       .unwrap_or_else(|| format!("https://{}", config.server.name)),
   ```

   Keeping `server.name` at the very end preserves today's behaviour for a
   deployment that has set neither, and `sync_contract.rs:318`
   (`https://tak.example.com/Marti/sync/content?hash=…`) still holds because that
   test sets `public_host`.

## 4. Blast radius beyond mission packages

`request_base_url` has one other caller:
`rustak-server/src/web/api/passkey.rs:363`, which derives the WebAuthn relying
party from it. It is shielded today because `settings::base_url` (the wizard's
recorded identity) is tried first and is set on any installation that has been
through setup — but an installation that has not, reached over HTTP/2, gets
"This server does not know what host it is reached on" rather than a ceremony.
Fix 1 closes that too.

## 5. A test that would have caught it

There is no test that drives `content_url` over HTTP/2, and an
`actix_web::test::TestRequest` always builds an HTTP/1-shaped request with a
`Host` header, so a unit test cannot reach this. Two options:

- A unit test on `request_base_url` that builds a `TestRequest` with **no**
  `host` header and a URI carrying an authority, asserting the authority wins.
  That pins the new branch without needing a real h2 connection.
- `rustak-server/tests/sync_contract.rs` already asserts the `public_host` shape;
  a sibling asserting the no-`public_host`, no-`Host`-header case would pin the
  precedence in fix 2.

Either is cheap and belongs with the fix.

## 6. Interim workaround (not applied)

Setting `[marti] public_host = "127.0.0.1:8443"` in `mp-download.toml`'s
`[config]` would make the scenario pass tomorrow. I have **not** done it: the
scenario's value is that it tells us what a stock deployment does, and pinning
`public_host` would hide exactly the bug above. If the fix is going to take a
while and a green nightly matters more, that is the one-line change, in
`interop/eud/scenarios/mp-download.toml`.
