# M10-15 — ADS-B: the longest wait before re-probing a refused rung is a setting

**Why — maintainer's decision, 2026-09-29.** M10-05 caps the wait before a refused rung is probed again at three hours (`rustak-plugin-adsb/src/sources/state/probe.rs` `MAX_WAIT`). Against a provider that refuses every probe that is about 22 refusals a day; six hours would roughly halve it and follow a relaxed limit down more slowly. Three hours stays the default, and the operator can change it **from the Services page**, not only in the configuration file.

**Starts after M10-13 lands** (it moves the source state this touches).

**Read first:** `M10-00-wave-rules.md`; status notes for M10-05, M10-13, M9-12 and M9-14; how `rustak-plugin-adsb` publishes its `config_schema`, validates a configuration document and applies the `area` override through `ServiceSettings`; `rustak-plugin-adsb/README.md` and `config.example.toml`; the Services page's schema form (`rustak-ui`, as M10-14 leaves it).

**Deliver.**
1. **A setting** beside the source's `poll` (name it in the plugin's existing style, e.g. `probe_max_wait`), a duration, default `3h`, with a floor (no shorter than one clean run at the configured interval) and a ceiling (a day); out-of-range values are refused at `--check` and by `validate_config` with the key named.
2. **In the plugin's `config_schema`**, with title, help text, default and bounds, so the Services page draws a control for it and the server validates what an administrator saves.
3. **Applied while running** through `ServiceSettings`, like `area`: a changed value takes effect for the next probe decision without reopening the source or forgetting the refused rung; logged once on change with where the value came from.
4. **Tests** on the injected clock: the default is unchanged from M10-05; a shorter and a longer cap change the simulated refusals a day in the expected direction (assert counts); a change while a rung is remembered applies to the next wait; out-of-range values are refused.
5. **README and `config.example.toml`.**

**Files you own:** `rustak-plugin-adsb/**`, your status note. If the Services form cannot draw a duration field as it stands, make the smallest additive change in the shared schema form and name it.
