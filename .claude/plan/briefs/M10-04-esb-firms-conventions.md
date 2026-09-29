# M10-04 — Bring the ESB and FIRMS plugins onto the shared sidecar conventions

**Why.** `rustak-plugin-esb` and `rustak-plugin-firms` were written by other sessions and merged by PR while M9-11..M9-14 were landing, so they predate some of what AIS and ADS-B do. Both have run cleanly in Dublin since 2026-09-22 (~2.8 MiB RSS each); this brief is about behaviour under failure and what an operator can see.

**Read first:** `M10-00-wave-rules.md`; briefs and status for M9-08, M9-12 and M9-14; `rustak-client/src/sidecar/settings.rs` (`ServiceSettings`) and how `rustak-plugin-ais` and `rustak-plugin-adsb` use it; `rustak-plugin-{ais,adsb}/src/sources/notice.rs`; `rustak-client/src/feed/publish.rs` (`FeedPublisher`, the periodic `The feed is publishing.` line); both plugins' `src/` and READMEs.

**Deliver, in both plugins.**
1. **Server-side settings through `ServiceSettings`.** Each reads the Services-page configuration (the `area` override) once at start via `context.control()?.config().await` (`rustak-plugin-esb/src/lib.rs:82`, `rustak-plugin-firms/src/lib.rs:104`), so an override silently does not apply on a start where the first control exchange fails. Use M9-14's retrying `ServiceSettings` as AIS/ADS-B do: read on link up and on recovery, apply a changed `area` while running, and log the area in effect with `area_from` on the `… is watching` line.
2. **Rate-limit and failure logging follows log-on-state-change.** Check each plugin's `sources/state.rs` against `notice.rs` in AIS/ADS-B: one line when the source starts failing or being refused, a reminder no more often than the shared cadence, one line on recovery; nothing per attempt. An explicit `poll` is a floor the provider may raise (a stated `Retry-After` is honoured above it), never a pin.
3. **A periodic counters line at `info`**, at the same cadence and in the same shape as `The feed is publishing. tracked=… published=…`, so an operator can see how many outages or detections each plugin holds. Use `FeedPublisher`'s line if the plugin publishes through it; otherwise log an equivalent line with the plugin's own nouns. Do not add a second timer if the shared one can serve.
4. **Tests** for each of the three, in each plugin, on fixtures and injected clocks: an override that arrives only after a failed first control exchange is applied; a refusal is logged once and recovery once; an explicit `poll` is raised by a stated `Retry-After`.
5. **READMEs** updated where behaviour changed.

**Out of scope:** lifting a shared `SourceState` into `rustak_client::feed` (a later brief, after this and M10-05 land) — but note in your status note what you found duplicated between the four plugins, so that brief starts from facts. Do not change `rustak-client` unless a plugin cannot otherwise use `ServiceSettings`; if you must, keep it additive and name it.

**Files you own:** everything under `rustak-plugin-esb/` and `rustak-plugin-firms/`, your status note. **Not yours:** `rustak-plugin-adsb/` (M10-05), `rustak-plugin-ais/`, `rustak-client/`.
