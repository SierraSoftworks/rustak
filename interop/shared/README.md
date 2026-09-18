# `interop/shared`

What `interop/node-tak` and `interop/eud` both need, kept in one place rather
than in two.

| File | What it is |
|---|---|
| `src/launch.ts` | Starts a throwaway rustak in a scratch directory with an internal CA, all three listeners bound on the loopback, and sweeps the directories a killed run left behind. `startServer({ prefix, name, host, config })`. |
| `src/config.ts` | The little bit of TOML writing that takes — assembled as tables and rendered once, because TOML has no way to define a table twice and a suite has to be able to override one key of one the launcher already emitted. |
| `src/http.ts` | One HTTP client in two flavours: the global `fetch` (node-tak, trusting `NODE_EXTRA_CA_CERTS`) and `node:https` with the authority named explicitly (the EUD runner, which stays in one process). |
| `src/bootstrap.ts` | The whole cold-start ceremony: setup token → first administrator → passkey → wizard, then creating accounts, granting channels and minting credentials. |
| `src/webauthn.ts` | A software WebAuthn authenticator, because registering a passkey is the only route to an administrator bearer token from a cold start. |
| `src/probe.ts` | Asking a server which compatibility surfaces it serves, so a scenario skips with the brief that will flip it instead of failing. |

It is **source only**: no `package.json` dependencies, no build, no install.
Each suite compiles it with its own `tsx` and `typescript`, and the
`{ "type": "module" }` here is only so that `tsc` resolves these files as ESM
when it follows an import into this directory from a suite next door.

Nothing suite-specific belongs here. What stays in a suite is what is true of
that suite alone — node-tak's `client_passwords_enabled` and its surface list,
the EUD runner's containers, scenarios and PKCS#12 handling.
