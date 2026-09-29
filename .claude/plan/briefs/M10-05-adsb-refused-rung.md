# M10-05 — ADS-B: remember the rung that was refused before probing it again

**Why — four hours against adsb.lol (Dublin, 2026-09-22).** M9-12's adaptive cadence rests at 35 s with zero refusals, eases to 23 s after 60 clean polls, is refused within about five minutes and returns to 35 s: about 2.4 refusals an hour, for ever. Cheap and harmless, but the plugin keeps asking a question it has already had answered.

**Read first:** `M10-00-wave-rules.md`; brief and status for M9-12 and M9-08; `rustak-plugin-adsb/src/sources/state/cadence.rs` (and `state.rs`, `state/wording.rs`, `notice.rs` for how changes are logged); the plugin's README section on polling.

**Deliver.**
1. **A refused rung waits longer before it is probed again.** The cadence remembers the last rung at which a probe (a step down after a clean run) was refused. Each consecutive refused probe of the same rung doubles the clean run required before that rung is tried again (60 polls → 120 → 240 …), capped so that the wait never exceeds a few hours at the resting interval. A probe that holds (survives a full clean run at the lower rung) clears the memory, and so does a change of the configured interval or provider. Refusals while resting — not caused by a probe — behave exactly as today.
2. **Invariants that must still hold**, each with a test: never below `max(configured, adopted stated delay)`; a stated `Retry-After` is honoured as before; the ceiling (120 s) is unchanged; the first probe after start behaves exactly as M9-12 specified.
3. **Log on change only.** The line that announces easing back says how long the clean run was; a refused probe says when the rung will next be tried (as a number of polls or a duration). No new periodic lines.
4. **Metrics.** If the plugin reports `source.poll_effective_s`, add the probe wait beside it only if it is useful to an operator; say which you chose and why.
5. **Tests** are simulations on the existing injected clock or pure state-machine steps — no sleeping: a provider whose real budget is 30 s settles at 35 s and its refusals per simulated day fall well below today's 2.4 an hour (assert the count); a provider that relaxes its limit is followed down within the cap.
6. **README**: one or two sentences in the polling section.

**Files you own:** `rustak-plugin-adsb/src/sources/state/**`, `rustak-plugin-adsb/src/sources/state.rs`, `rustak-plugin-adsb/README.md`, your status note. Nothing outside `rustak-plugin-adsb/`.
