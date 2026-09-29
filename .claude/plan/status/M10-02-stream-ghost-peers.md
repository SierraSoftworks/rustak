# M10-02 — Bound how long a vanished peer stays on the stream

**Done.** Every accepted `:8089` socket now carries TCP keepalive probes and,
on Linux, `TCP_USER_TIMEOUT`, sized from `[stream.tls] idle_timeout` by a pure
function. A peer the kernel gives up on (`ETIMEDOUT`) leaves with the new
reason `peer_timeout`. No new configuration key.

## What changed and why

M9-15 made the idle rule `max(last_rx, last_tx)`. A completed write only proves
the bytes reached our kernel, so a phone that vanished on a busy channel stayed
registered until the send buffer filled (`write_timeout`) or Linux gave up
retransmitting (~924.6 s at `tcp_retries2 = 15`). The kernel now does the
noticing, with no server-sent TAK ping.

### Sizing — `stream/peer_probe.rs` (new, 39 functional lines)

`PeerProbe::for_idle_timeout(idle_timeout)`:

- bound = `idle_timeout` clamped to `FLOOR = 10 s` ..= `CEILING = 900 s`, whole seconds;
- `keepalive_idle` = bound / 2;
- `keepalive_interval` = max(1 s, (bound − idle) / `RETRIES`), `RETRIES = 3`;
- `user_timeout` = bound.

Default 90 s → first probe at 45 s, every 15 s, 3 probes, user timeout 90 s.
The floor keeps a sub-second test timeout (or a very short production one) from
killing phones crossing a brief coverage gap; the ceiling is Linux's own
retransmission limit rounded down, so a long idle timeout is never worse than
the kernel alone.

### Platform facts (cited in the rustdoc of `peer_probe`)

- **Linux:** `SO_KEEPALIVE` + `TCP_KEEPIDLE`/`TCP_KEEPINTVL`/`TCP_KEEPCNT` +
  `TCP_USER_TIMEOUT` (RFC 5482, `tcp(7)`). Linux does not send keepalive probes
  while data is outstanding, which is why the user timeout is what bounds a peer
  under outbound traffic. `tcp(7)`: with keepalive on, `TCP_USER_TIMEOUT`
  overrides keepalive in deciding when to close — once a probe is unanswered and
  the user timeout has elapsed since the last receive, the connection closes and
  `TCP_KEEPCNT` no longer counts. The sizing makes both answers ≈ `idle_timeout`.
- **macOS:** `TCP_KEEPALIVE` (idle), `TCP_KEEPINTVL`, `TCP_KEEPCNT`; no user
  timeout. Under traffic the bound stays `write_timeout` / retransmission limit.
- **Windows:** `SIO_KEEPALIVE_VALS` (idle + interval); `TCP_KEEPCNT` on Windows
  10 1703+, otherwise fixed at 10. No user timeout.
- **Elsewhere:** `SO_KEEPALIVE` on system timers or nothing; the pre-M10-02
  bounds stand.

### Setting the options — `stream/listener_tls.rs`

The accept loop now goes through `accept(&TcpListener, &mut Tuning)`, which
accepts and calls `Tuning::apply`: `TCP_NODELAY` (moved here unchanged),
`set_tcp_keepalive` (interval and count on linux/android/macos/ios/freebsd/
netbsd/windows) and, on linux/android, `set_tcp_user_timeout`. Each option is
tried even if an earlier one failed; the first refusal is logged **once per
listener start** at `warn` (naming the option), and the connection is always
served. Safe `socket2::SockRef` only.

### The reason — shared files, exact diff for merging with M10-01

- `stream/liveness.rs`: one new variant appended **at the end**, `PeerTimeout = 11`
  (`"peer_timeout"`), appended to `ALL` (now `[Self; 11]`) and to `as_str`. No
  other change. None of the ten existing causes fitted: `read_error`/`write_error`
  are "the socket failed", `idle` is our timer, `write_timeout` is our writer's
  deadline. If M10-01 also adds a variant, one of us renumbers; the discriminants
  only need to be unique and `ALL` in declaration order.
- `stream/connection.rs`: the import line `use super::{notify, peer_probe, writer};`
  and, in `read_loop`'s `Some(Err(err))` arm, `return LeaveReason::ReadError;`
  became `return peer_probe::cause(&err, LeaveReason::ReadError);`. If M10-01
  reclassifies resets there, wrap its result the same way (or call `cause`
  first): `cause` only overrides when the `CodecError::Io` kind is `TimedOut`.
  Plus one test (`TimedOut` reader + `a_peer_the_kernel_gave_up_on_is_named_as_a_peer_timeout`)
  inserted before `a_close_from_outside_carries_the_cause_it_was_given`.
- `stream/metrics.rs`: **not changed** — `LeaveCounters` is sized by
  `LeaveReason::ALL.len()`, so the new counter slot comes for free.
- `stream/writer.rs` (outside the brief's list; smallest change): the two
  `Ok(Err(err))` arms in `flush` and `fed` record
  `peer_probe::cause(&err, LeaveReason::WriteError)` instead of
  `LeaveReason::WriteError`, because `TCP_USER_TIMEOUT` expiring under traffic
  surfaces on the write. One import line and one test.
- `stream/mod.rs` (outside the list): `pub mod peer_probe;` and a line in the
  module table.

## Can a blackholed peer be tested in the `Test` job? No.

To blackhole, packets between two sockets must be dropped *inside the kernel*
so neither side's TCP ACKs. Without privileges that is not possible:
`iptables`/`nft`/`pf` need root or `CAP_NET_ADMIN`; a network namespace needs
`CAP_SYS_ADMIN` or unprivileged user namespaces (restricted by AppArmor on
Ubuntu 24.04 runners and not portable to macOS); `TCP_REPAIR` needs
`CAP_NET_ADMIN`. A userspace proxy that stops forwarding does not help — its own
kernel keeps ACKing — and neither does `SIGSTOP` on a peer process. Provoking
`TCP_USER_TIMEOUT` via a zero window is possible on Linux but needs megabytes of
traffic and ≥ 10 s of wall clock, and `write_timeout` would race it; not added.

What is proven instead:

- options read back from a socket that came through the listener's own `accept`
  path (Linux in CI: keepalive on, idle/interval/count, user timeout; macOS
  locally: keepalive on, idle/interval/count; Windows builds but CI does not run
  tests there, and on Windows only `keepalive()` and nodelay are asserted);
- `ETIMEDOUT` → `peer_timeout` at the mapping function, through the read loop,
  and through the writer;
- the sizing at default, very short, very long, and every whole second 0..=1000.

## Tests, and what they do on a host ten times slower

| Test | Clock | 10× slower host |
|---|---|---|
| `peer_probe::tests::*` (5) | none, pure | identical |
| `listener_tls::tests::an_accepted_socket_carries_the_probe_sized_from_the_idle_timeout` | none; one loopback connect, then getsockopt | identical; no timing asserted |
| `listener_tls::tests::a_refused_option_is_reported_once_per_listener_not_per_connection` | none | identical |
| `connection::tests::a_peer_the_kernel_gave_up_on_is_named_as_a_peer_timeout` | none; reader fails at once | identical |
| `writer::tests::a_write_the_kernel_gave_up_on_is_named_as_a_peer_timeout` | none; writer fails at once | identical |

## Docs

- `config.example.toml` `[stream.tls] idle_timeout`: the old "write_timeout
  catches a vanished peer" paragraph replaced with what sizes the probes, the
  floor/ceiling, `peer_timeout`, and per-platform linger.
- `docs/deployment.md` **When a stream connection is reclaimed**: the same, with
  a per-platform table, the Linux keepalive/user-timeout interaction, old
  Windows' fixed count, the once-per-start `warn`, and a `peer_timeout` row in
  the reason table.

## Files

New: `rustak-server/src/stream/peer_probe.rs`, this note.
Changed: `Cargo.toml` (`socket2 = { version = "0.6.5", features = ["all"] }` in
`[workspace.dependencies]` — already in the lock as a transitive dependency;
`all` is needed for `with_retries`, the user timeout and the getters),
`Cargo.lock` (one line: rustak-server depends on socket2),
`rustak-server/Cargo.toml`, `rustak-server/src/stream/{listener_tls,liveness,connection,writer,mod}.rs`,
`config.example.toml`, `docs/deployment.md`.

## Left open

- The Linux- and Windows-only `cfg` branches were not compiled here (macOS
  host; no Linux or Windows std installed, no network to add one). The socket2
  0.6.5 signatures were checked by reading its source. CI's Test (Linux) and
  build (Windows) jobs are the first compilation of those branches.
- Module docs in `liveness.rs` and `connection.rs` still say a vanished peer
  under outbound traffic is `write_timeout`'s job. Left alone to keep the diff in
  M10-01's files minimal; worth a sentence after both merge.
- Nothing was run against a real phone dropping off a network.
