# M10-00 — Rules every M10 brief shares

M10 is a backlog-closure wave (2026-09-29). Each brief is built by one agent in **its own git worktree**, made from `main` at `a13843ff`. The orchestrator collects each worktree's diff, verifies the combined result and commits it.

**Where you work.** Your current directory is your worktree: edit only there. `/Users/bpannell/dev/gh/SierraSoftworks/rustak` is the main checkout — read briefs from it (they are not in your worktree), never write to it.

**Version control.** No `git` or `but` write commands of any kind (no add, commit, stash, checkout, reset, push). Read-only inspection (`git log`, `git diff`, `git blame`, `git status`) is fine. Leave your changes uncommitted in the worktree.

**Build environment.** Several agents build at once on a 10-core, 32 GB machine with finite disk. Before any cargo command:
`export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only`
Do not set `CARGO_TARGET_DIR`; your worktree's own `target/` is yours. The machine may not reach crates.io for crates that are not already in `~/.cargo/registry`; if a new dependency cannot be fetched, say so rather than working around it.

**Read first.** `.claude/plan/conventions.md` (structure, < 300 functional lines per file, errors, tracing, security defaults, tests), then whatever your brief names.

**Standing rules.**
- Secure by default; never log secrets, tokens, keys or certificate private material.
- Quiet logs: log on a change of state, not per attempt; one line per event; routine outcomes are not `warn`/`error`.
- **No test may assert an upper time bound across work whose duration depends on the host.** Prove counts, ordering or state; keep timeouts for failing a hung wait only, and make those generous. Prefer an injected or paused clock.
- atak-civ, TAK Server and OpenTAKServer are GPL: read for facts only, copy nothing.
- Do not edit `.claude/plan/plan.md`, `.claude/plan/backlog.md` or `.github/` unless your brief says so. Stay inside the files your brief gives you; if the work needs a file outside them, make the smallest change and name it in your status note.

**Exit checks (all must pass, in your worktree).**
1. `cargo fmt --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`
4. `./scripts/check-file-length.sh`
5. `cargo test -p <crate>` for every crate you changed (the whole crate, not only your tests). The orchestrator runs the workspace-wide suite after integrating. If a timing-sensitive test you did not touch fails while the machine is loaded, run it alone before concluding anything.

**When you finish.**
1. Write `.claude/plan/status/<brief id and slug>.md` in your worktree: what changed and why, decisions taken, every file changed or added, how each new test behaves on a host ten times slower, anything left open.
2. `rm -rf target` in your worktree.
3. Final message: the files changed/added, the result of each exit check, and open questions. Report failures as failures.
