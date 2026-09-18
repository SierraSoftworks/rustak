# R-03 — Robustness review: storage, runtime, concurrency, resource limits

**Reviewer:** independent, read-only. No source was edited and no `git`/`but` write was run.
**Baseline:** `cargo check -p rustak-server --lib` clean; `cargo test -p rustak-server --lib -- store:: cot_store:: stream::` → 226 passed, 0 failed.
**Scope read:** design 01 §4–5, design 02 §2; `rustak-server/src/{db,store,cot_store,stream,files,jobs,services,missions,marti,web,auth,plugins}`; `rustak-cot/src/codec`, `rustak-cot/src/detail/marti.rs`.

Two numbers frame everything below, because they are the ones the design commits to:
**500 connected devices** and a **400 MB data package** (`[marti] upload_size_limit_mb` default, `config/marti.rs:19`).
Several defaults are set at values that are comfortable at fifty devices and wrong at five hundred.

---

## Summary

| # | Severity | Finding |
|---|---|---|
| C1 | Critical | The connect-time SA replay overruns the connection's own queue; above ~770 devices a new client is closed during its own replay |
| C2 | Critical | CoT history thrashes above 256 concurrent uids: one new segment file and ~4 SQLite transactions **per message** |
| H1 | High | The per-connection writer task is never aborted; a peer that stops reading leaks a task, a socket and a TLS session for the life of the process |
| H2 | High | `<dest group>` costs one database read per `<dest>` element, undeduplicated and uncapped |
| H3 | High | The shared outbound HTTP client has no timeout at all (OIDC, JWKS, token exchange, ACME) |
| H4 | High | `cot_store::query::history` decodes every record in the window before applying `limit` — up to 2,000,000 events per device |
| H5 | High | Nothing collects content-store orphans or temp files, although `[retention] content_orphans` is documented as if something does |
| H6 | High | Zip build and parse run synchronously on actix worker threads, uncached, on every device connection |
| M1 | Medium | Mission-package import is silently capped at actix's default 256 KiB, and doubles the body in memory |
| M2 | Medium | A callsign change is never re-indexed: `<dest callsign>` stops resolving, and the index entry leaks |
| M3 | Medium | Poison jobs retry for ever, a panicking job is silent, and job concurrency is unbounded |
| M4 | Medium | The CoT store stops before the stream drains, so history relayed during shutdown is dropped |
| M5 | Medium | The rate-limiter sweep is O(n) on every check and frees nothing while entries are locked out |
| M6 | Medium | A crash between creating a segment file and indexing it leaks the file for ever |
| M7 | Medium | `CotRecord::new` forces both encodings and copies both, per message, before the queue can drop it |
| M8 | Medium | `forget()` unlinks a segment the writer still holds open and then appends into the hole |
| L1–L7 | Low | Migration transaction behaviour, `Database::close`, whole-segment reads, multipart part count, JWKS cache busting, startup `std::fs`, UI `join_all` |

---

## Critical

### C1 — The connect-time replay overruns the connection's own queue

`rustak-server/src/stream/replay.rs:26`, called from `rustak-server/src/stream/connection.rs:132`
`rustak-server/src/stream/hub.rs:286` (`latest_sa_for`), `rustak-server/src/stream/subscription.rs:139` (`ConnHandle::send`)
Defaults: `rustak-server/src/config/stream.rs:141` (`queue_len = 256`), `:145` (`close_after_drops = 512`), `:153` (`max_connections = 1024`)

`replay_latest_sa` is a **synchronous** `fn`. It asks the hub for the latest SA of every reachable
peer and pushes all of them into the new connection's queue with `try_send`, with no await point
between pushes, before the read loop starts:

```rust
for event in events {
    if handle.send(Outbound::Event(event)) == SendResult::Sent { sent += 1; }
}
```

The queue is 256 deep. `ConnHandle::send` counts a full queue as a drop and, at
`subscription.rs:155`, calls `self.close()` once `dropped_consecutive` reaches `close_after_drops`.

**Failure scenario.** An installation runs at its configured ceiling of 1024 devices — or simply
above ~770 — and the server restarts. Device 771 completes its handshake, registers, and
`replay_latest_sa` pushes 770 events. The first 256 fit; the next 512 are consecutive drops; the
513th trips `close_after_drops` and the handle closes the connection the client has not yet read a
byte from. The client reconnects, is closed again, and the fleet sits in a reconnect loop that
looks like a network fault. The `max_connections = 1024` the operator configured is unreachable;
the real ceiling is `queue_len + close_after_drops`.

At the stated target of **500 devices** the connection survives, but ~244 of the 500 replayed
positions are silently discarded — `replay_latest_sa` only counts `SendResult::Sent` and logs that
number, so the log says "Replayed the latest position of the peers a new client can see" with a
count that is half the truth. Half the map is missing until each peer's next reporting interval.

The drops are not guaranteed on a multi-thread runtime — the writer task may drain concurrently —
but the push loop is a `try_send` with no syscall per message and the writer has to encrypt and
write a TLS record per batch, so the loop wins comfortably, and on a busy runtime (500 devices
reconnecting at once, TLS handshakes saturating the cores) it wins deterministically.

**Fix.**
1. Do not let the replay count against `close_after_drops`. Add an `Outbound` variant, or a
   `send_replay` on `ConnHandle`, that drops without touching `dropped_consecutive`.
2. Make the replay yield: make `replay_latest_sa` `async` and `send().await` on the bounded
   channel, with an overall `tokio::time::timeout`. The writer is running; awaiting here applies
   backpressure to one new connection instead of discarding its map.
3. Derive the floor: refuse to start when `queue_len < max_connections`, or size the queue as
   `max(queue_len, max_connections + headroom)`, and say so in `config.example.toml`.
4. Add a `replay_dropped` counter to `StreamMetrics` so this is visible.

### C2 — CoT history thrashes above 256 concurrent uids

`rustak-server/src/cot_store/history.rs:29` (`MAX_OPEN_LOGS = 256`), `:138` (`touch`)
`rustak-server/src/store/append_log.rs:183` (`Segment::create` when `current.is_none()`), `:244` (`seal`)
`rustak-server/src/store/segment.rs:123` (`Segment::create`)
`rustak-server/src/db/repos/stream_segments.rs:91` (`create`), `:124` (`record_append`), `:155` (`seal`)

`HistoryWriter` keeps one `AppendLog` per **uid** and caps the map at 256, evicting least-recently
used. The access pattern from a live fleet is round-robin over every connected uid — the pathological
case for LRU. Eviction is not free: `touch` calls `log.seal()`, which is `flush()` (one SQLite write)
plus `stream_segments().seal()` (a second). Reopening then finds no unsealed row, so
`AppendLog::append` takes the `self.current.is_none()` branch and calls `Segment::create` — which
creates **a brand-new file** and inserts **a third** SQLite row. The end-of-batch
`HistoryWriter::flush` adds roughly one more `record_append` per record.

The project's own test proves the shape of it:
`cot_store/history.rs:241 an_evicted_stream_is_appended_to_again_rather_than_lost` writes
`UID-A`, `UID-B`, `UID-A` with `max_open = 1` and reads the two `UID-A` records back out of two
separate segments.

**Failure scenario.** 500 devices, SA every two seconds → ~250 relayed messages/s, all with distinct
uids, cycling past a 256-entry LRU. Every message misses. Per message: ~4 `BEGIN IMMEDIATE…COMMIT`
transactions on the single writer connection (≈1,000/s), one `create_dir_all`, one `create_new` file.
That is **~900,000 segment files per hour**, each holding one ~200-byte record, under
`<streams_dir>/cot/<uid>/`. Inodes run out long before bytes do; `ls` on a stream directory becomes
unusable; `query::history` for one device then has to open thousands of files; and the writer
connection — which every `/api/v1` write, every enrolment and every device `upsert_seen` also
queues behind — is saturated by housekeeping. The `CotStoreHandle` queue (4096) fills, and the
"The CoT store is behind" warning fires while the disk fills at the same time.

**Fix.**
1. Size the cap against the connection ceiling, not against a guess: default
   `max_open` to `stream.limits.max_connections` (or `max_connections * 2`) and make it configurable.
2. Make a miss cheap. A sealed segment that is seconds old and kilobytes long should be *adopted*
   rather than superseded — allow `AppendLog::open` to reopen the newest sealed segment when it is
   under the roll size and was sealed recently, or do not seal on eviction at all (close the file
   handle, keep the row unsealed, and rely on `recover()` on the way back in).
3. Better: key the log on something with far lower cardinality. A single `cot` stream per **day** or
   per **hour**, with the uid written into the record, turns this into one open file and makes
   retention cheaper too; the per-uid index is already a `stream_key` column the reader can filter on.
4. Whatever is chosen, add a `history_segments_created` counter and alert on segment-creation rate.

---

## High

### H1 — The writer task is never aborted, so a half-dead peer leaks a socket for ever

`rustak-server/src/stream/connection.rs:123` (spawn), `:164` (the give-up)
`rustak-server/src/stream/writer.rs:57` (`run`), `:91` (`flush`), `:135` (`feed`)

```rust
let _ = tokio::time::timeout(Duration::from_secs(5), writing).await;
```

A `timeout` around a `JoinHandle` stops **waiting**; it does not abort the task. `writer::run`
has no shutdown arm — it awaits `rx.recv()`, then `sink.feed(..).await` and `flush(..).await` on the
socket, with no timeout and no `select!` on the connection's `Shutdown` token.

**Failure scenario.** A device completes the TLS handshake, sends one SA, then stops reading
(a phone that sleeps, a NAT that silently blackholes, a deliberate zero-window). Its queue fills;
`close_after_drops` fires; `ConnHandle::close` cancels the token and the read loop breaks at the
120-second idle timeout. `connection::run` unregisters, waits 5 seconds for the writer and gives up.
The writer is blocked inside `flush()` on a socket whose window is zero. Because
`tokio::io::split` only closes the underlying socket when **both** halves drop, the TCP connection,
the rustls session state and the task stay alive indefinitely — TCP keepalive is off and the OS
default would be two hours if it were on. The listener's semaphore permit *is* released, so the
connection count looks healthy while file descriptors climb. A few hundred such peers and the
process hits `EMFILE`; `accept` then fails on every new connection ("Could not accept a stream
connection" in a loop) and the server stops serving while still appearing up.

**Fix.**
1. `let writing = tokio::spawn(..)` → on timeout, `writing.abort()` (and `.await` the handle).
2. Give `writer::run` a shutdown arm: take the connection's `Shutdown` and
   `select!` it against `rx.recv()`, and wrap `feed`/`flush` in a write timeout
   (a new `[stream.limits] write_timeout`, default ~30 s).
3. Set `TCP_KEEPALIVE` on accepted sockets (`socket2`) alongside the existing `set_nodelay(true)`
   at `listener_tls.rs:160`.

### H2 — `<dest group>` is a database read per destination element, uncapped and undeduplicated

`rustak-server/src/stream/dest.rs:140` (the loop), `:233` (`group_recipients`)
`rustak-cot/src/detail/marti.rs:206` (`take_marti`), `rustak-cot/src/codec/mod.rs:55` (`MAX_MESSAGE = 8 MiB`)

`take_marti` collects **every** `<dest>` child with no cap. `select_recipients` then loops over
`addresses.groups` and calls `group_recipients` → `db.groups().get_by_name(&name).await` for each
one, sequentially, with no dedup and no cache. `rustak-cot/src/xml/parse.rs:220` caps nesting depth
(`MAX_DEPTH = 64`) but nothing caps sibling count.

**Failure scenario.** Any device with a valid certificate sends one 8 MiB XML frame whose `<marti>`
holds ~250,000 `<dest group="__ANON__"/>` elements. `handle_inbound` issues 250,000 sequential
SQLite reads for the *same* row. The read pool is two connections (`db/connection.rs:31`), so this
occupies half of it for seconds at a time while the sender's read loop is blocked and cannot be
closed by the idle timeout. The attacker pipelines; every `/api/v1` list, every Marti read and every
`resolver::resolve` on a new connection queues behind it. This is a full denial of service from one
authenticated device, and it is cheap — the frame compresses to nothing on the wire.

The uid and callsign lists have a milder version of the same problem: `Hub::resolve` takes the
registry read lock once and iterates the key list under it (`hub.rs:403-404`), so 250,000 keys is a
multi-second read-lock hold that blocks `apply_event`'s write lock and stalls **all** routing.

**Fix.**
1. Cap the destination list where it is parsed: a `MAX_DESTS` (64 is far past any real client) in
   `take_marti`, with the excess dropped and counted.
2. Deduplicate `addresses.groups`, `addresses.callsigns` and `addresses.uids` in `partition`
   (`dest.rs:187`) before any lookup.
3. Cache the group index. `Hub` already needs it and `db.groups().index()` is read once per
   connection in `resolver.rs:246`; resolve `GroupName → bitpos` from a cached index refreshed on
   the group-change event rather than from SQLite on the routing path.

### H3 — The shared outbound HTTP client has no timeout

`rustak-server/src/services/mod.rs:147`

```rust
let http_client = reqwest::Client::builder()
    .user_agent(HTTP_USER_AGENT)
    .build()
```

No `.timeout()`, no `.connect_timeout()`. reqwest's default is *no* request timeout. This one client
is used by OIDC discovery and JWKS (`web/helpers/oidc/discovery.rs:118`), the OIDC token exchange
(`web/helpers/oidc/exchange.rs:136`), and ACME through
`pki/acme/transport.rs:71` / `pki/acme/account.rs:80,102`.

**Failure scenario.** The identity provider's load balancer accepts connections and never responds
(the common partial-failure mode, not a refused connection). Every admin sign-in hangs: the actix
worker's task never completes, the browser sits on a spinner, and the request is only reclaimed when
the client gives up. Because the JWKS fetch sits inside `cache().cached(..)`, every token validation
that needs a refresh joins the same stall. On the ACME side, `jobs/acme_renew` holds its queue
reservation open against a hung directory, and the job host's `handler.timeout()` reservation is the
only thing that eventually frees the message — the task itself is still running.

Separately, `.json()` at `discovery.rs:132` reads the whole body with no size cap, so a compromised
or misbehaving IdP can stream unbounded bytes into the server's memory.

**Fix.** `.connect_timeout(Duration::from_secs(5))`, `.timeout(Duration::from_secs(30))`,
`.pool_idle_timeout(Duration::from_secs(90))` on the builder, exposed as `[server] http_timeout`.
Bound the response body: read with `bytes_stream()` under a cap (256 KiB is generous for a discovery
document and a JWKS) before deserialising.

### H4 — The CoT history query materialises the whole window before applying `limit`

`rustak-server/src/cot_store/query.rs:119`
`rustak-server/src/config/retention.rs:17` (`cot_history_max_rows = 2_000_000`)

```rust
for path in segment_paths(db, uid, from, to).await? {
    for payload in read_segment(streams_dir, &path).await? {
        match decode(&payload) { Some(event) => events.push(event), ... }
    }
}
...
events.sort_by_key(..);
events.truncate(limit);
```

`limit` is applied **last**. Every record of every overlapping segment is read off disk, decoded
into a full `Event` with its detail tree, and held in one `Vec`.

**Failure scenario.** An administrator opens the CoT browser for a busy device and asks for the last
seven days with `limit = 200`. The per-device floor is 2,000,000 frames; at a few hundred bytes of
decoded `Event` each that is well over a gigabyte of allocation for a 200-row page — and
`read_segment` additionally holds each 8 MiB file in memory whole while walking it. One request OOMs
the server. No authentication beyond "administrator" is needed, and an impatient operator retrying
makes it worse.

**Fix.** Walk segments newest-first (`segment_paths` already orders by `first_time`; reverse it),
decode lazily, and stop once `limit` events inside the window have been collected. Keep a hard
`MAX_SCANNED_RECORDS` so a window with sparse in-range records still terminates, and report
truncation to the caller. `read_segment` should stream frames from the file rather than
`tokio::fs::read`ing it whole.

### H5 — Nothing collects content-store orphans or temp files

`rustak-server/src/config/retention.rs:50` (`content_orphans = 24h`), `config.example.toml:466-470`
`rustak-server/src/store/content.rs:132` (temp path), `:241` (`iter`, written *for* a sweep)
`rustak-server/src/jobs/mod.rs:51-59` (the registered jobs)

`config.example.toml` documents the key and even explains the 24-hour delay —

> "Not zero: an upload is referenced a moment after it is stored, and a collector running in between
> would delete the file out from under the request that wrote it."

— but there is no collector. `ContentStore::iter()` exists with a doc comment saying it is "for the
orphan sweep"; nothing calls it. The registered jobs are `acme_renew`, `audit_prune`,
`mission_expiry`, `retention` (CoT only), `service_health` and `wal_checkpoint`.

Two leaks follow. Blobs whose last referencing row is gone (deleted packages at
`web/api/packages.rs:202`, `marti/sync_read.rs:202`, purged missions) stay on disk for ever. And
`<content_dir>/tmp/<uuid>` accumulates: `content.rs:138-147` removes the temp file when
`write_temp` fails, but **not** when `create_dir_all` (`:150-159`) or `rename` (`:167-170`) fails, and
nothing at all removes temp files left by a `SIGKILL`, an OOM kill or a container eviction mid-upload.

**Failure scenario.** A 400 MB package upload is interrupted by a pod restart. 400 MB sits in
`tmp/` for the life of the volume. Repeat weekly for a year with a fleet that syncs packages and the
data directory is full, with no operator-visible cause — the `resources` table says the store holds a
fraction of what `du` reports.

**Fix.** Add `jobs/content_orphans.rs`: hourly, compare `ContentStore::iter()` against
`resources.hash ∪ profile_files.hash ∪ mission archives`, and unlink anything older than
`[retention] content_orphans` that nothing refers to. Sweep `tmp/` in the same pass by mtime. Until
that exists, either delete the key from `config.example.toml` or mark it "not yet enforced" — a
documented setting that does nothing is worse than no setting.

### H6 — Zip build and parse run on actix worker threads, uncached, per connection

`rustak-server/src/marti/profiles.rs:335` (`packaged`, reached from `enrollment`, `connection`,
`tool`, `mission_package`), `:212` (`build_multifile_package`)
`rustak-server/src/profiles/builder.rs:138` (`write_zip`, deflate over an in-memory `Vec`)
`rustak-server/src/profiles/service.rs:259,285` (every profile file `read_to_end` into memory)
`rustak-server/src/missions/import.rs:52,203`; `rustak-server/src/missions/archive.rs:123`
`rustak-server/src/web/api/config_packages.rs:107`; `rustak-server/src/pki/p12.rs:110`

None of these is inside `spawn_blocking`. The workspace does use `spawn_blocking` correctly for key
generation (`pki/ca.rs:207`, `pki/server_cert.rs:165`) and argon2
(`rustak-core/src/identity/password.rs:254`), so the omission is inconsistent rather than deliberate.

**Failure scenario.** `GET /Marti/api/device/profile/connection` is what ATAK calls on **every**
connection. It reads every profile file for the caller's channels into memory
(`service.rs:285`, 8 MiB each, unbounded count) and deflates a zip synchronously. A fleet of 500
devices reconnecting after a server restart issues 500 of these within seconds. actix's worker count
defaults to the CPU count, so with 8 workers and a 200 ms deflate the listener is saturated for
~12 seconds; with a 50 MB profile set it is minutes, during which the *entire* HTTP surface — health
checks included — is unresponsive, and the orchestrator restarts the pod. There is no cache: the same
bytes are rebuilt per device per connection.

`missions/import.rs:203` is the other half: `entry.read_to_end(&mut bytes)` with no cap on the
decompressed size, so a zip bomb inflates on the worker thread too (bounded today only by M1's
accidental 256 KiB input cap — which is itself a bug, so the two do not cancel out safely).

**Fix.**
1. Wrap every zip build/parse and the PKCS#12 derivation in `tokio::task::spawn_blocking`.
2. Cache the assembled profile package keyed by (tool, channel set, `last_modified`) — the handler
   already computes `assembled.last_modified`, so the cache key is free and `If-Modified-Since` is
   already honoured.
3. Cap the decompressed total in `import_package`: track a running sum against the upload limit and
   refuse past it, and cap the manifest's entry count.

---

## Medium

### M1 — Mission-package import is capped at 256 KiB by an actix default, and doubles the body

`rustak-server/src/marti/missions/contents.rs:92` (`import`), `rustak-server/src/marti/missions/crud.rs:164`
No `PayloadConfig` exists anywhere in the workspace; actix-web 4.15's `DEFAULT_CONFIG_LIMIT` is
262,144 bytes (`actix-web-4.15.0/src/types/payload.rs:330`).

`PUT /Marti/api/missions/{name}/contents/missionpackage` takes `body: web::Bytes`, so any package
over 256 KiB is rejected with a `413` that has nothing to do with `[marti] upload_size_limit_mb`
(400 MB) or with what `/files/api/config` told the client. Every real ATAK data package exceeds it.
The existing test (`rustak-server/tests/missions_extras.rs:478`) uses a few-kilobyte zip, so the
ceiling is never exercised.

The mirror-image risk: raising the limit to 400 MB without changing the handler makes it worse —
`body.to_vec()` at `:92` copies the whole body, so a single import holds ~800 MB, and there is no
concurrency limit on the route.

**Fix.** Read the package as a `web::Payload` through the same `files::limits`-bounded reader the
sync servlets use (`marti/sync.rs:200`), spool it to the content store, and open the zip from the
file. Failing that, set an explicit `PayloadConfig` per route so the limit is the configured one,
and pass `web::Bytes` to `ZipArchive::new(Cursor::new(bytes))` without `to_vec()`.

### M2 — A callsign change is never re-indexed

`rustak-server/src/stream/hub.rs:138`, `rustak-server/src/stream/subscription.rs:317`,
`rustak-server/src/stream/registry.rs:68`

`Subscription::apply_event` overwrites `self.callsign` on every identifying SA
(`subscription.rs:317`), but `Hub::apply_event` only touches `by_callsign` when
`update.first_identity` is true (`hub.rs:138`).

**Failure scenario.** An operator renames a device from `ALPHA` to `BRAVO` mid-session — routine in
ATAK. From then on `<dest callsign="BRAVO">` resolves to nobody, so direct chat to that person
silently vanishes (or bounces as `b-t-f-s`, which is worse: the sender is told the person is not
there while they are looking at them on the map). `<dest callsign="ALPHA">` still resolves, to the
right connection, under the wrong name.

There is a leak in the same place. `Registry::unindex` removes the subscription under its *current*
callsign, so the stale `ALPHA → [ConnId]` entry is never emptied and never dropped
(`registry.rs:87-96` only removes an entry when its vector empties). One `HashMap` entry with a dead
`ConnId` leaks per rename, for the life of the process.

**Fix.** In `Hub::apply_event`, compare the callsign before and after `subscription.apply_event`; on
a change, `remove(&mut registry.by_callsign, old, id)` then insert under the new one. Same for
`client_uid`, which can also change if a client re-identifies.

### M3 — Poison jobs retry for ever, panics are silent, concurrency is unbounded

`rustak-server/src/jobs/host.rs:94`, `:115`, `:316`

`backoff` caps at `MAX_BACKOFF = 15 minutes` and there is no attempt ceiling and no dead-letter.
A message whose payload no longer deserialises, or whose handler has a deterministic bug, is retried
every fifteen minutes for the life of the installation, one `error!` line each time.

`tasks.try_join_next()` at `:94` discards the `Result`, so a handler that panics produces **no log
line at all** — the message simply reappears after its reservation window and is retried for ever,
invisibly.

`tasks.spawn(..)` at `:115` is unbounded. A backlog of due messages (after a long outage, or a
retention sweep that enqueued a lot) is spawned as fast as `try_dequeue_any` returns rows, each
holding an `AppContext` clone and doing database work on the shared writer.

**Fix.** Add `[jobs] max_attempts` (default ~10): past it, move the message to a `queues_dead` table
with the last error and stop. Replace `while tasks.try_join_next().is_some() {}` with a loop that
matches on `Err(JoinError)` and logs a panic against the partition. Bound in-flight jobs with a
semaphore or `JoinSet::len()` check (default ~8).

### M4 — The CoT store stops before the stream drains

`rustak-server/src/stream/mod.rs:143` (`context.shutdown().child()`),
`rustak-server/src/cot_store/writer.rs:83-98` (biased `select!` on `shutdown.cancelled()`), `:102`

The store's token is a child of the same shutdown that stops the listeners, and the writer loop's
`select!` is `biased` on cancellation. On `SIGTERM` the writer breaks its loop immediately, drains
whatever `try_recv` returns, closes the segments and exits — while the stream listener is still
draining connections for up to `[server] shutdown_timeout` (8 s default).

**Failure scenario.** During those 8 seconds connections keep relaying (that is the point of a
drain). `Router::record` still `try_send`s into a channel whose only consumer has gone. The sends
succeed until the 4096-deep buffer fills, then count as drops — either way nothing is written. Up to
8 seconds of relayed history is missing from the record, and the "already relayed, so it is history
that happened" invariant the module documents (`cot_store/writer.rs:100`) does not hold.

**Fix.** Give the store its own token, cancelled by `StreamRuntime::run` *after*
`listener_tls::run` returns and before the `timeout(self.drain, self.store)` at `stream/mod.rs:265`.
The store already flushes on `rx` closing, so dropping the last `CotStoreHandle` would also do it.

### M5 — The rate-limiter sweep is O(n) per check and frees nothing while entries are locked

`rustak-server/src/auth/ratelimit.rs:35` (`SWEEP_AT = 1024`), `:77`

```rust
if buckets.len() > SWEEP_AT {
    buckets.retain(|_, bucket| live(bucket, now, self.window));
}
```

`live` keeps any bucket with `locked_until > now` — a 15-minute default. The comment claims this
"bounds the memory a flood of distinct addresses can cost us"; it does not. Once the map exceeds
1024 entries that are all locked out, every subsequent `check` runs a full `retain` that removes
nothing, under a `std::sync::Mutex`, on an actix worker thread.

**Failure scenario.** An attacker posts `/api/v1/auth/login` with 100,000 distinct usernames from one
address over ten minutes. All 100,000 buckets lock out for 15 minutes and stay `live`. Every
legitimate sign-in, refresh and passkey ceremony now pays a 100,000-entry scan while holding the
mutex — quadratic in the attack's size, and it blocks the reactor thread rather than yielding.

**Fix.** Keep a `next_sweep: Instant` and sweep at most once per window instead of per check. Cap the
map (`MAX_BUCKETS`) and evict the entry with the earliest `locked_until` when full — losing a lockout
early is far better than a scan per request. Consider keying on address only when the subject
cardinality is attacker-controlled.

### M6 — A crash between creating a segment file and indexing it leaks the file for ever

`rustak-server/src/store/segment.rs:123-160`

`Segment::create` writes the file first and inserts the index row second — right, and the error path
unlinks the file. But a process killed between the two leaves a file with no row. Retention
(`append_log.rs:320`, `stream_segments.rs:243/284`) works from the index, so an unindexed file is
invisible to it and to `AppendLog::open`. The doc says such a file is "superseded on the next roll";
it is, but it is never deleted.

Related, in the same file: `repair` (`segment.rs:66`) `tokio::fs::read`s the entire segment —
up to 8 MiB — and counts frames on every `AppendLog::open` that finds an unsealed row. Under C2's
thrash this would be a full 8 MiB read per eviction cycle if any segment were ever left unsealed.

A narrower race: `AppendLog::remove_indexed` (`append_log.rs:351`) calls
`tokio::fs::remove_dir(parent)` (`:378`) after unlinking the last segment of a stream. If a writer has an open
`AppendLog` for that uid whose `current` is `None`, the next `Segment::create` fails with `ENOENT`
because only `AppendLog::open` calls `create_dir_all`. The window is short but it is real on a
long-running server.

**Fix.** Sweep unindexed segment files in the same orphan job H5 asks for (walk `<streams_dir>`,
compare against `stream_segments.segment_path`, unlink anything older than a grace period).
Have `Segment::create` `create_dir_all` the directory itself rather than relying on `AppendLog::open`.

### M7 — Every relayed message pays two encodings and two full copies before the queue can drop it

`rustak-server/src/cot_store/mod.rs:100-101`, called from `rustak-server/src/stream/router.rs:252`

```rust
xml: String::from_utf8_lossy(encoded.xml()).into_owned(),
proto: encoded.proto().to_vec(),
```

`EncodedEvent` caches both forms lazily through a `OnceLock` (`rustak-cot/src/codec/encoded.rs:29`),
which is the right design — but `CotRecord::new` forces **both** and then copies both into owned
buffers, on the sender's task, before `CotStoreHandle::record` has had a chance to `try_send` and
possibly drop the record. History is on by default, so an all-XML fleet pays a protobuf encode per
message it will never transmit.

At 500 devices / 250 msg/s with ~1 KB messages this is ~500 KB/s of allocation and a protobuf encode
per message on the hot routing path — and all of it is wasted when the store queue is full, which is
exactly when the server is already under pressure.

**Fix.** Make `CotRecord` hold `Arc<EncodedEvent>` and have the writer task take the encodings it
needs on its own task (it only needs `proto` for history and `xml` for `cot_latest`). Or reserve a
queue slot first (`try_reserve`) and build the record only if it succeeds.

### M8 — `forget()` unlinks a segment the writer still holds open, then appends into the hole

`rustak-server/src/cot_store/query.rs:180`

The doc comment says a file the writer still holds open "is unlinked anyway — … and its next segment
is a fresh one". The first half is true; the second is not. `AppendLog` keeps writing into the
unlinked inode until `would_overflow` trips at `max_segment_bytes` (8 MiB), and `flush()`'s
`record_append` silently updates zero rows because the row was deleted. So up to 8 MiB of that
device's history after the delete goes to an unreachable inode — the disk space is held until the
process exits, and the records are lost.

**Fix.** Have `forget` tell the `HistoryWriter` to drop its log for that uid before unlinking — a
control message on the store channel is enough — or have `record_append` returning `false` cause the
`AppendLog` to seal and roll.

---

## Low

- **L1 — Migrations use a `DEFERRED` transaction and re-check nothing.**
  `db/migrations.rs:163` (`c.transaction()`), `:153` (`current_version`, read outside it). Two
  processes started against one file (a rolling restart, a stray CLI) both read version 0 and both
  try to apply `0001`; one gets `SQLITE_BUSY` or a `schema_migrations` primary-key violation and
  fails start-up with an error that does not say why. Use `TransactionBehavior::Immediate` and
  re-read `MAX(id)` inside the transaction before applying.

- **L2 — `Database::close` cannot close the writer connection.**
  `db/connection.rs:317` (`Arc::try_unwrap(writer)`) is called from `runtime.rs:125` on
  `context.db().clone()`, while the context still holds a handle — so the unwrap always fails and the
  `tokio_rusqlite` worker thread is never joined. The `TRUNCATE` checkpoint does happen (their test
  `the_log_is_truncated_while_the_server_still_holds_a_handle` pins that), so this is tidiness rather
  than durability, but the `warn!` on the inside of that `if` is dead code and the intent is
  misleading. Take the `Database` by value from the context, or drop the context's handle first.

- **L3 — `AppendLog::read_range` and `read_segment` load whole segment files.**
  `store/append_log.rs:274` (`read_range`), `:437` (`read_segment`), `:445` (`tokio::fs::read`), `:454` (`records.extend(Frames::new(&bytes).map(<[u8]>::to_vec))`)
  — every frame is copied into its own `Vec`. Two full copies of an 8 MiB segment per file read.
  Stream the frames instead; `Frames` is already an iterator over a borrowed slice.

- **L4 — `read_upload` caps each multipart part but not the number of parts.**
  `web/api/profile_files.rs:197`. `MAX_FILE_BYTES` (8 MiB) is per part; the `while let` loop has no
  bound, and `actix_multipart` imposes none, so an authenticated administrator can stream
  indefinitely at 8 MiB per part. Cap the part count and the running total.

- **L5 — An unknown `kid` busts the JWKS cache and triggers an untimed outbound fetch.**
  `web/helpers/oidc/discovery.rs:94`. `force_refresh` removes the cache entry and re-fetches. A
  stream of tokens carrying random `kid` values turns every validation into an outbound request to
  the IdP (which, per H3, has no timeout). Rate-limit or debounce the forced refresh — at most one
  per minute is plenty for a key rotation.

- **L6 — `crypto::keyfile` does blocking `std::fs` under an async caller.**
  `crypto/keyfile.rs:38,78,105,126`, reached from `lib.rs:130` (`crypto::SecretStore::load` in `build_context`). Start-up only, so
  the impact is nil today, but it is the sort of thing that gets reused on a request path later.

- **L7 — `join_all` with no bound in the UI.** `rustak-ui/src/pages/group_members.rs:27` issues one
  request per user account concurrently. On a large directory this is an N-request burst at the
  server from a single page load. Use `buffer_unordered(8)`.

---

## What I checked and found sound

Worth recording so the next reviewer does not repeat it:

- **No `.await` under a `parking_lot` guard.** Every `Hub` and `ServerEvents` accessor is a plain
  `fn` returning owned data; there is no `.await` token in `stream/hub.rs` or `plugins/events.rs`
  outside their test modules. The invariant `hub.rs:3` documents is upheld.
- **No unbounded channels in production code.** The only `unbounded_channel` is a test fixture
  (`rustak-client/src/sidecar/run.rs:541`). Every production channel is bounded and the capacities
  are configured or documented.
- **The SSE ring is bounded and resumable.** `plugins/events.rs:57` (`RING = 256`) bounds both the
  broadcast depth and the replay buffer, and `web/api/events.rs:168` handles `Lagged` by refilling
  from the ring rather than ending the response.
- **Per-connection memory under a slow reader is genuinely bounded.** `queue_len` slots of
  `Arc<EncodedEvent>`, shared across recipients, with `try_send` and a consecutive-drop close.
  (C1 is about the threshold being wrong, not about the mechanism.)
- **Retention never deletes a live segment.** Both `expired_before` (`stream_segments.rs:253`) and
  `over_row_cap` (`:294`) filter `sealed = 1`.
- **Torn frames are handled.** `store/frame.rs` caps a record at 4 MiB and stops at the first
  incomplete frame; `segment::repair` truncates back to the last complete record; an index row that
  claims more than the file holds seals the segment rather than appending (`append_log.rs:400-408`).
- **Wire framing is capped and resynchronises.** `MAX_MESSAGE = 8 MiB`; an unterminated XML buffer
  past the cap is cleared (`xml_frame.rs:129`), an oversize proto length resynchronises
  (`proto_frame.rs:80`), and neither drops the connection.
- **`/Marti/sync` uploads stream** through `BoundedReader` (`files/store.rs:277`) against the
  resolved limit, and every download path streams in 64 KiB chunks. Content paths are validated
  against a 64-hex-char pattern (`content.rs:101`), so no traversal.
- **`Database::write` uses `BEGIN IMMEDIATE`**, the router never awaits the database for ordinary
  traffic, and `CotStoreHandle::record` is `try_send` with a counter.
- **The TLS handshake has its own timeout and a pre-handshake semaphore**
  (`listener_tls.rs:145,237`), which is the right order.
- **`Database::close` runs on every exit path**, including a failed bind and an abandoned drain
  (`runtime.rs:103-107,122`).

---

## Load and soak tests to add

Numbers are targets for a laptop-class CI runner unless stated. Each maps to a finding above.

### 1. Connect storm — 600 devices (C1, H6, resolver, TLS)

`rustak-server/tests/load_connect_storm.rs`, `#[ignore]` by default, run nightly.

Start the server with a test CA and 600 pre-issued client certificates. Connect all 600 within
5 seconds, each sending one identifying SA immediately.

Assert: every connection is still registered 30 s later (**catches C1 today at ≥770; use 800 for the
hard assertion**); `hub.len() == 600`; the count of `SendResult::Dropped` attributable to replay is
zero; p99 time from `accept` to first inbound message routed is < 2 s; the writer connection's
queue depth never exceeds 100. Also fetch `/Marti/api/device/profile/connection` from each device in
the same window and assert the public listener answers `/api/v1/health` in < 500 ms throughout
(catches H6).

### 2. Sustained relay soak — 500 devices, 30 minutes (C2, M7)

500 in-process `rustak-client` EUDs, each sending an SA every 2 s (~250 msg/s, ~450k messages).

Assert at the end: the number of files under `<streams_dir>/cot/` is **< 2,000**, not > 400,000
(this is the C2 assertion and it fails hard today); `stream_segments` row count < 2,000; RSS growth
over the second half of the run < 50 MB; `CotStoreHandle::dropped() == 0`; the p99 of
`handle_inbound` < 5 ms. Sample `stream_segments` insert rate — anything above ~1/s per 100 devices
is the thrash.

### 3. Slow and dead readers (H1)

Ten connections that complete the handshake, send one SA, then stop reading entirely (set
`SO_RCVBUF` small and never `read`). Plus ten that read at 1 KB/s.

Assert: after `idle_timeout + 10 s` the process's open file-descriptor count has returned to its
pre-test value ±2 (fails today — H1), `hub.len()` is back to the baseline, and the tokio task count
(via `tokio-metrics` or `/proc/self/task`) has not grown. Run the same scenario 50 times in a loop
and assert FDs are flat — that is the leak signal.

### 4. Hostile-message fuzz/DoS (H2)

A single authenticated connection sending, in order: an 8 MiB frame with 250,000
`<dest group="__ANON__"/>`; one with 250,000 `<dest callsign="X"/>`; one with 250,000 `<dest uid>`.

Assert: a second, healthy connection's round-trip latency stays under 100 ms throughout (fails today
— H2); total SQLite reads attributable to the hostile message < 10; the hostile connection is closed
or its message rejected. Extend `rustak-cot/fuzz` with a `marti` target that asserts a `MAX_DESTS`
cap once it exists.

### 5. Package and mission-package limits (M1, H6)

Parameterised over 1 MB / 50 MB / 400 MB / 401 MB, against `POST /Marti/sync/upload`,
`POST /api/v1/packages` and `PUT /Marti/api/missions/{m}/contents/missionpackage`.

Assert: 400 MB succeeds on all three (the mission-package leg fails today at 256 KiB — M1);
401 MB is refused with the Marti-shaped `400`; peak RSS during the 400 MB upload is
< 200 MB (streaming, not buffering); `<content_dir>/tmp/` is empty afterwards on both the success
and the refusal path. Add a zip-bomb case: a 10 MB package that inflates to 10 GB must be refused,
not inflated (H6).

### 6. Crash and restart consistency (M6, append-log recovery)

Run the relay soak for 60 s, `SIGKILL` the process, restart, and assert: every segment file on disk
has an index row and every row has a file; `PRAGMA integrity_check` and `PRAGMA foreign_key_check`
are empty; the sum of `record_count` matches what a full scan of the files finds; the newest segment
of each stream ends on a complete frame. Repeat 20 times with the kill at a random offset. Also kill
mid-upload and assert the orphan sweep (once H5 exists) reclaims `tmp/` within
`[retention] content_orphans`.

### 7. Shutdown budget (M4, H1)

500 connections relaying, then `SIGTERM`. Assert: the process exits within
`shutdown_timeout + DATABASE_CLOSE_TIMEOUT + 1 s`; `rustak.sqlite-wal` is zero bytes; **every message
relayed between the signal and the last connection closing is present in the history** (fails today —
M4); no `tokio` task outlives the process's main future.

### 8. Job host resilience (M3)

Enqueue a message whose payload does not deserialise, one whose handler panics, and 1,000 due
messages at once.

Assert: the poison message reaches a dead-letter after `max_attempts` and stops logging (fails today);
the panic produces exactly one `error!` naming the partition (silent today); in-flight jobs never
exceed the configured concurrency; the queue drains in bounded time and the writer connection is not
starved (measure `/api/v1/health` latency during the drain).

### 9. Credential-flood micro-benchmark (M5)

A `criterion` bench over `RateLimiter::check` with 1, 1k, 10k and 100k live locked-out buckets.

Assert: `check` stays O(1) — under 1 µs at 100k buckets. Today it is a 100k-entry `retain` per call.

### 10. Long-window history read (H4)

Seed one stream key with 2,000,000 records across ~250 segments, then
`GET /api/v1/cot/{uid}/history?from=…&to=…&limit=200`.

Assert: peak RSS delta < 100 MB and wall time < 2 s (today it materialises every record before
truncating).
