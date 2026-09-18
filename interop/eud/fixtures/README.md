# Fixtures

Hand-written `commo-log.txt` / `commo-xml.txt` files, built from the format
strings recorded in
[`M1-00` §3.2](../../../.claude/plan/status/M1-00-eud-interop-harness-exploration.md),
so the parsers and the assertion engine can be tested with no Docker, no server
and no network.

**They are ours.** Nothing here is a capture from a real run of a GPL tool and
nothing here was copied from `atak-civ`: they are written to the documented
shape of the lines this suite asserts on, with rustak's own uids, callsigns and
ports. `conventions.md` → Tests requires fixtures to be our own, and
`interop/eud/README.md` → Licence posture says the same thing about this
directory in particular.

A fixture is therefore *not* evidence about what `commotest` prints. It is
evidence about what this suite does with what it is given — which is the part
that can be wrong without a container to notice it. The line prefixes below are
deliberately fuller than any assertion needs, because the parsers must not care
about them.
