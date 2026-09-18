# iTAK — manual compatibility checklist

iTAK is **best-effort only** (`plan.md` "Decisions" table, "Deferred" row: "iTAK-specific quirks").
rustak does not target byte-for-byte iTAK compatibility the way it does for ATAK-CIV and CloudTAK,
there is no iTAK source in this project's research corpus, and there is no automated coverage for it
and none is planned. This page exists so that "best-effort" has a floor: the one thing rustak
explicitly builds for iTAK (a config-package variant) gets checked before a release, and anything
beyond that is recorded as unverified rather than silently assumed to work.

If you don't have an iOS device with iTAK installed, it is reasonable to skip this page for a given
release and say so in the release notes — it is not a release gate the way [`atak.md`](atak.md) is.

## 0. What is already covered automatically

Nothing iTAK-specific. The generic Enterprise Sync and Marti HTTP contract iTAK presumably also
speaks (it is, like WinTAK, a TAK Product Center client) is covered by the same suites listed in
[`atak.md`](atak.md) §0, but none of them drive iTAK itself.

## 1. Manual config package import

This is the one iTAK path rustak actively builds for — `POST /api/v1/config-packages` with
`variant: "itak"` — and the one already partly checked, in [`profiles.md`](profiles.md) §5:

1. Build the package (`variant: "itak"`) and transfer `<user>_CONFIG_iTAK.zip` to the device.
2. Import it.
   - [ ] iTAK reads the flat archive (`config.pref`, `truststore.p12`, no manifest) and the server
         appears in its connection list.
   - [ ] The unsuffixed `caLocation`/`caPassword` keys in the app preference group were honoured.
3. **Unverified — this is the known gap to check first if step 2 fails.** rustak writes
   `cert/truststore.p12` for this variant (the path ATAK's certificate sorter produces), because there
   is no source-level confirmation that iTAK re-homes a package's certificate the same way. If import
   succeeds but the connection does not trust the server, try a package built with a bare
   `truststore.p12` at the archive root instead, and record whichever one actually works in
   `.claude/plan/compat/profiles.md` (not just here — that file is the design contract other briefs
   read, and this finding belongs there).

## 2. Everything else — to confirm, if attempted

None of the following has ever been checked against a real iTAK install. If you have a device and
time, run the equivalent of [`atak.md`](atak.md)'s sections and record what you find; if not, leave
this section as "not attempted" in the release notes rather than assuming a pass.

- [ ] **QR enrolment.** **To confirm** whether iTAK supports scanning the same
  `tak://com.atakmap.app/enroll?...` URL ATAK-CIV does, or has its own enrolment flow.
- [ ] **Channels UI.** **To confirm** whether iTAK has a Channels overlay, and whether it honours the
  same `bitpos`/`direction` model.
- [ ] **Chat.** **To confirm** whether iTAK sends/receives `b-t-f` GeoChat at all.
- [ ] **Package send/receive.** **To confirm** whether iTAK implements peer file-share (`b-f-t-r`) or
  only server-hosted Enterprise Sync browsing.
- [ ] **Data Sync.** **To confirm** whether iTAK has a Data Sync UI comparable to ATAK's — historically
  iTAK has shipped with a materially smaller feature set than ATAK-CIV, but that is general knowledge
  about the product, not something read from source for this project, so treat it as an unverified
  claim to check rather than a fact to design around.
- [ ] **Disconnect/reconnect, revocation.** The server-side behaviour (25-second timeout, `t-x-d-d`,
  TLS-handshake-level cert refusal) is identical for every client, per `streaming.md` and the identity
  design — only iTAK's own visible reaction is unverified.

## Known gaps

- **iTAK-specific quirks are out of scope by design**, not by oversight — see `plan.md`'s "Deferred"
  row. This page will stay thin until someone chooses to invest in verifying iTAK from source or from
  hands-on testing; it is not an implementation backlog.
- Video, ExCheck, federation, QUIC and plaintext/anonymous streaming are out of scope for every
  client, same as [`atak.md`](atak.md).
- The iTAK config-package keystore path (§1 step 3) is the one concrete open question on this page;
  everything else is simply unattempted.

## Verified in

- `plan.md` "Decisions" table — the explicit "best-effort only" scope decision for iTAK.
- `.claude/plan/compat/profiles.md` "Corrections from the M3-02 implementation" — the iTAK keystore
  path is flagged unverified there too; this page and that one should stay in sync on the finding.
- Nothing else on this page has a verified source; that is the point of it.
