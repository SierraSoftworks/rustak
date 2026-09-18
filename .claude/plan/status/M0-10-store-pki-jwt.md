# M0-10 — content store, append log, root CA, JWT issuer — complete

Brief: `.claude/plan/briefs/M0-10-store-pki-jwt.md`
Read first: `.claude/plan/conventions.md`; `plan.md` → Storage, Identity & auth model, "Design
artefacts and reconciled decisions" (TLS row); `design/01-foundations-storage-ci.md` §4.6, §6.2,
§8 step 10; `design/03-identity-pki-acme-auth.md` §2 (file layout), §3 (`keys.rs`, `ca.rs`,
`pem.rs`), §5 (`oauth_server/jwt.rs`, `keys.rs`); `research/03-cloudtak-node-tak-contract.md` §2.1
(node-tak's JWT parser — the test oracle); status files `M0-06`, `M0-07`, `M0-08`.

## What was built

Three module trees, replacing the empty stubs. `lib.rs` was **not** touched — it already declared
`pub mod store; pub mod pki; pub mod auth;`. `services/`, `jobs/` and `prelude.rs` (the concurrent
brief's files) were not touched either; every entry point takes `&Database` / `&SecretStore`
directly, as instructed.

| File | Functional lines | Contents |
|---|---:|---|
| `store/mod.rs` | 6 | module doc (why neither store is SQLite); `mod`/`pub use` |
| `store/frame.rs` | 52 | `encode_frame`, base-128 varint put/read, `Frames` iterator + `valid_len`, `MAX_RECORD_BYTES` |
| `store/segment.rs` | 228 | `Segment` (create/adopt/write/pending accounting/`would_overflow`/`touches`), `repair` (scan + truncate), `segment_name`, `encode_component` |
| `store/append_log.rs` | 260 | `AppendLog::{open, append, flush, seal, read_range, prune_before}`, `AppendLogOptions`, `DEFAULT_SEGMENT_BYTES`, recovery dispatch |
| `store/content.rs` | 209 | `ContentStore::{new, prepare, path_for, put, put_bytes, open, exists, remove, iter}`, `ContentRef` |
| `pki/mod.rs` | 6 | module doc (why rustak is its own CA; we choose the subject); `pub use` |
| `pki/keys.rs` | 39 | `KeyType` (re-exported from config), `generate_key`, `key_pair_from_pkcs8`, `signature_algorithm` |
| `pki/ca.rs` | 285 | `CaMaterial`, `load_or_create_root_ca`, `ca_certificate_path`, `RootCaRecord`, root params, `ca.crt` export |
| `pki/pem.rs` | 53 | `bare_base64_64col`, `pem_certificate`, `parse_pem_chain`, `sha256_fingerprint` |
| `auth/mod.rs` | 2 | module doc + `pub mod jwt;` (M0-11 adds the rest) |
| `auth/jwt.rs` | 268 | `JWT_HEADER_JSON`, `AccessClaims`, `SigningKey`, `JwtIssuer::{load_or_create, issue, verify, token_key_json, jwks, rotate, active_kid, issuer, audience}` |

118 unit tests (117 run, 1 `#[ignore]` throughput check), each file with its single trailing
column-0 `#[cfg(test)] mod tests`.

**No `Cargo.toml` change was needed.** Everything used (`rcgen`, `rsa`, `x509-parser`, `pem`,
`jsonwebtoken`, `sha2`, `base64`, `hex`, `rand`, `uuid`, `rustls-pki-types`, `tempfile`) was already
declared for `rustak-server`. `prost`/`bytes` were **not** added: the varint framing is 25 lines of
`store/frame.rs` and is byte-identical to protobuf's `uint64` varint (pinned by a test asserting
`300 → [0xac, 0x02]`), so a dependency would have bought nothing but a coupling.

## Brief requirements, point by point

### 1. `store/`

- **`AppendLog` is payload-agnostic.** A record is `varint(len) + len opaque bytes`; the timestamp
  given to `append` is *index metadata only* and is never written into the file. That keeps the
  file the "varint-delimited protobuf stream" plan.md specifies, readable by `protoc --decode_raw`
  and any protobuf library, and means the log never has to learn a schema.
  - **Consequence, documented on the method:** `read_range(from, to)` resolves to **segment**
    granularity — it returns every record of every segment the window touches, and the caller (which
    can decode a payload) filters precisely. The log cannot do better without either parsing
    payloads or adding a per-record timestamp to the frame, and both were ruled out above.
- **Crash-safe on open.** `AppendLog::open` looks up `stream_segments.open_segment`, scans the file
  with `Frames`, truncates any trailing partial frame, then reconciles the row:
  - file holds *more* than the row claims (records reached the disk, the index write did not) →
    `record_append` the difference and carry on in that segment;
  - file holds *less* than the row claims (a lost write) → **seal it and roll**, so the index can
    never point past the end of a file;
  - file missing entirely → seal the row, roll;
  - file already at the roll size → seal, roll.
  Nothing is fsynced — writes are `write_all` + `flush` — which is exactly why this repair exists.
- **Rolls segments** at `AppendLogOptions::max_segment_bytes` (default `DEFAULT_SEGMENT_BYTES` =
  8 MiB). An *empty* segment never rolls, so a record larger than the roll size still gets a file.
- **Indexes via `stream_segments`**, using only the repository M0-07 landed (`create`,
  `record_append`, `seal`, `open_segment`, `overlapping`, `expired_before`, `delete`). No raw SQL
  and no change to `db/`.
- **Prunes.** `AppendLog::prune_before(db, root, before)` is an associated function, because the
  retention job sweeps every stream in one pass. File first, then the row (a row without a file is
  recoverable; a file without a row is invisible and leaks); an unlinkable file is logged and left
  indexed so the next sweep retries; the now-empty stream directory is removed best-effort.
- **`ContentStore` is content-addressed with an atomic rename.** `put` streams into
  `<root>/tmp/<uuid>` while hashing with SHA-256, then renames to `<root>/<aa>/<hash>`. The
  temporary directory is *inside* the store so the rename stays within one filesystem. Storing the
  same bytes twice is a no-op. `path_for` refuses anything that is not 64 lowercase hex characters,
  which is what stops a hash taken from a request path naming a file outside the store (tested with
  `../../etc/passwd`, uppercase, wrong length).

**Tests.** The brief's four named cases are all present in `store::append_log::tests`:
`a_partial_frame_left_by_a_crash_is_truncated_on_open` (crash truncation),
`a_full_segment_rolls_to_the_next_file` (roll), `a_window_reads_only_the_segments_it_touches`
(range reads), `a_hundred_thousand_appends_are_fast_enough` (`#[ignore]`d throughput sanity).
Plus recovery of an under-counting index row, of an over-claiming one, of a missing file, the
unflushed-tail read, pruning, stream isolation and the traversal-safety case.

Throughput check, release build:

```
$ cargo test -p rustak-server --lib --release -- store::append_log::tests::a_hundred_thousand --ignored --nocapture
running 1 test
100k appends in 943.139166ms
test store::append_log::tests::a_hundred_thousand_appends_are_fast_enough ... ok
```

~106 000 records/s including the index writes, against a budget of 60 s.

### 2. `pki/`

- **`KeyType {Rsa2048, Rsa3072, EcdsaP256}`** — *re-exported from `crate::config::pki::KeyType`
  rather than redeclared.* M0-06 already landed that enum with exactly those three variants and the
  `rsa-2048`/`rsa-3072`/`ecdsa-p256` spellings `config.example.toml` documents. A second identical
  enum plus a conversion would have been two things to keep in step for no gain, so `pki::keys`
  does `pub use crate::config::KeyType;` and `pki::KeyType` resolves to it.
- **`generate_key`** uses the pure-Rust `rsa` crate for RSA (`RsaPrivateKey::new` → PKCS#8 →
  `rcgen::KeyPair::from_pkcs8_der_and_sign_algo(_, &PKCS_RSA_SHA256)`) and rcgen for ECDSA, as the
  design requires — rcgen 0.14 *can* generate RSA through `aws-lc-rs`, but that is the C keygen path
  the design avoids for cross-compilation. `load_or_create_root_ca` runs it in `spawn_blocking`.
- **`key_pair_from_pkcs8(der, kind)`** takes the key type rather than inferring it: several
  signature algorithms fit the same RSA key, and a certificate whose signature algorithm changed
  between runs because a guess changed would be a miserable failure to diagnose.
- **`load_or_create_root_ca(db, secrets, pki, data_dir)`** — rcgen params per design 03 §3: CN from
  `[pki] ca_common_name` plus `subject_entries()` in order, `IsCa::Ca(Unconstrained)`, key usages
  `[KeyCertSign, CrlSign, DigitalSignature]`, a 128-bit serial with the top bit cleared,
  `KeyIdMethod::Sha256`, validity from `[pki] ca_validity`. Creation is an **insert that does not
  overwrite**, so two processes starting together cannot each mint a CA — the loser reads back and
  adopts the winner's.
- **`ca.crt` under `<data_dir>/pki/`**, rewritten on every start when it is missing or stale, so a
  deleted export comes back without an operator asking.
- **`pem.rs`** — `bare_base64_64col` (standard alphabet, 64 columns, trailing newline, no armour:
  the `signedCert` body node-tak re-armours itself), `pem_certificate`, `parse_pem_chain` (via the
  `pem` crate, since `rustls-pemfile` is not a workspace dependency; a private key sharing the file
  is skipped rather than refused), `sha256_fingerprint` (lowercase hex, no colons).

**Deviations, both deliberate and both worth a second opinion:**

1. **Where the root CA is stored.** The brief says "cert DER in kv partition `pki`, key `Sealed`
   with `SecretContext::CaKey`". Those two halves cannot both be satisfied literally, because M0-08
   landed `SecretContext::CaKey { certificate: CertificateId }` (design 01 §5) while design 03 §3
   still described the older `PkiKey { name: "root_ca" }`. I followed the brief's storage location —
   one `RootCaRecord` in the `pki` partition of `kv` — and bound the sealed key to
   **`CertificateId::new(0)`**, a documented sentinel: SQLite row ids start at 1, so zero can never
   collide with a `certificates` row and names "the root CA slot" instead. The AAD is still stable
   and unique, which is the property the sealing design depends on.
   **Note for M2:** design 01 §6.3 says `POST /setup/ca` writes the CA into `certificates(kind='ca')`
   — that table exists and has `key_sealed`. If M2 moves the CA there, the key must be **re-sealed**
   under its real `CertificateId` as part of the move, or the old ciphertext will not open.
   `RootCaRecord` carries a `version` field so that migration can tell the formats apart.
2. **`PkiError`.** Design 03 names one; the landed code (config, db, crypto) uses
   `human_errors::Error` throughout and conventions mandate it, so `pki` does too. Nothing else has
   a `PkiError` to be consistent with yet.
3. **`CaMaterial` timestamps are `chrono::DateTime<Utc>`**, not `time::OffsetDateTime` as design 03
   sketches. `time` is not a workspace dependency and adding one just to name a field in our own
   struct was not worth it; rcgen's own `not_before`/`not_after` are set through
   `rcgen::date_time_ymd`, which is day-granular. `not_before` is therefore midnight UTC of the
   creation day — always already valid, which is the safe direction for clients with skewed clocks.
4. **`sha256_fingerprint` returns `String`**, not a `Sha256Fingerprint` newtype (design 03 §3). No
   such type exists yet, and `db::repos::certificates` stores the fingerprint as `String`. M2 can
   introduce the newtype across both at once.

**Tests.** CA round-trip parsed by `x509-parser`: `is_ca()` true, `key_cert_sign`/`crl_sign`/
`digital_signature` set, self-issued subject == issuer, 128-bit positive serial, validity following
`ca_validity`, a reload producing the same fingerprint, the reloaded key actually signing a leaf,
the stored key sealed (and refused under a different encryption key), `ca.crt` exported and
restored after deletion, name entries in order with unknown attributes dropped, and the RSA-2048
default path.

### 3. `auth/jwt.rs`

- **Header byte-exact**: `{"alg":"RS256","typ":"JWT"}`, 27 bytes → 36 base64url characters, no
  padding. Tokens are assembled by hand (`HEADER_B64 . payload_b64 . sign(...)`) rather than through
  `jsonwebtoken::encode`, because that serialises a `Header` struct and would not give these bytes.
- **Flat claims, `aud` a string**: `AccessClaims { sub, aud, iss, iat, nbf, exp, jti, scope, dev? }`
  in wire order. `issue` **refuses** to mint a token whose payload contains more than one `}` — the
  design suggested a `debug_assert!`; a hard error was chosen instead, because a release build that
  silently emitted such a token would break CloudTAK login in a way nobody would trace back to a
  scope string.
- **RS256-only verification**: `decode_header` is checked before anything else and
  `Validation::algorithms` is `[RS256]`, so both classic forgeries (`alg: none`, `alg: HS256` keyed
  on the public key we publish) are refused. Audience, issuer, `exp` and `nbf` are all required,
  with 60 s leeway. Every failure collapses to one `Kind::User` "Your session is no longer valid",
  so the response is never a token oracle.
- **Previous-key rollover**: tokens carry no `kid` (no room in a fixed 27-byte header), so
  verification tries the active key then each retired one; only an `InvalidSignature` moves on to
  the next key, so an expired or misaddressed token fails immediately rather than being retried
  against every key. `rotate(db, secrets)` mints a new key, retires the old in `oauth_keys` and
  keeps it for verification; `load_or_create` reloads that state across a restart.
- **`token_key_json`** = `{"alg":"SHA256withRSA","value":"<SPKI PEM>"}` (the Spring shape TAK
  clients expect at `/oauth/token_key`); **`jwks`** = `{"keys":[{kty,use,alg,kid,n,e}…]}`, active
  key first.
- Keys are RSA-2048 from the `rsa` crate in `spawn_blocking`, sealed with
  `SecretContext::JwtSigningKey { kid }` (kid = first 8 bytes of `sha256(SPKI)`, hex) and stored in
  `oauth_keys` with `alg = RS256`, `purpose = access_token`.
- `issue` takes `&Username` / `Option<&DeviceUid>` rather than design 03's `&User`, because
  `identity::User` is M0-11's type and does not exist yet. `load_or_create` takes `&AuthConfig` plus
  a `base_url` fallback for the issuer, matching `AuthConfig::issuer`'s documented default.

**The CloudTAK-style lenient-parse simulation** (`cloudtaks_lenient_parser_reads_the_subject`,
`the_payload_carries_exactly_one_closing_brace`) transcribes node-tak's `parse()` from research 03
§2.1: strip the dots, decode the **whole token** as one base64 blob, split on `}`, parse `split[1] +
"}"`, read `sub`. Two fidelity details the first cut got wrong and the test now models correctly:
Node's `Buffer.from(x, 'base64')` accepts the **URL-safe** alphabet (our payload contains `-`/`_`),
and it tolerates a trailing partial group (our 342-character RS256 signature leaves one). The test
asserts `sub` == the username, `aud` == `"rustak"`, and that `exp`/`iat`/`nbf` decode as numbers.

## Concurrency note

`services/`, `jobs/` and `prelude.rs` were being written by another agent throughout. The first
build failed on *their* half-landed `prelude.rs` → `jobs::{Job, JobContext}`; it resolved on its own
and nothing of theirs was edited. The whole workspace is green as of this writing.

## Exit checks

```
$ cargo test -p rustak-server --lib -- store:: pki:: auth::

running 118 tests
test auth::jwt::tests::a_claim_that_would_break_the_lenient_parser_is_refused ... ok
test auth::jwt::tests::an_hs256_or_unsigned_token_is_refused ... ok
test auth::jwt::tests::a_token_we_issued_verifies ... ok
test auth::jwt::tests::an_expired_token_is_refused ... ok
test auth::jwt::tests::a_token_for_another_audience_or_issuer_is_refused ... ok
test auth::jwt::tests::a_token_within_the_leeway_is_still_accepted ... ok
test auth::jwt::tests::cloudtaks_lenient_parser_reads_the_subject ... ok
test auth::jwt::tests::the_header_is_the_documented_bytes_exactly ... ok
test auth::jwt::tests::the_payload_carries_exactly_one_closing_brace ... ok
test auth::jwt::tests::a_token_signed_before_a_rotation_still_verifies ... ok
test auth::jwt::tests::the_issuer_falls_back_to_the_server_url ... ok
test auth::jwt::tests::a_tampered_payload_is_refused ... ok
test auth::jwt::tests::the_key_survives_a_restart ... ok
test auth::jwt::tests::the_published_key_material_is_public_only ... ok
test auth::jwt::tests::two_tokens_never_share_an_identifier ... ok
test auth::jwt::tests::a_token_signed_by_another_server_is_refused ... ok
test auth::jwt::tests::the_stored_key_is_sealed ... ok
test pki::ca::tests::a_key_sealed_under_a_different_encryption_key_is_refused ... ok
test pki::ca::tests::a_second_start_reloads_the_same_authority ... ok
test pki::ca::tests::every_short_attribute_name_an_operator_writes_is_understood ... ok
test pki::ca::tests::a_new_installation_gets_a_certificate_authority ... ok
test pki::ca::tests::configured_name_entries_land_in_the_subject_in_order ... ok
test pki::ca::tests::the_certificate_is_exported_for_operators ... ok
test pki::ca::tests::the_certificate_may_sign_certificates_and_revocation_lists ... ok
test pki::ca::tests::the_key_is_usable_after_a_reload ... ok
test pki::ca::tests::the_serial_is_128_bits_and_positive ... ok
test pki::ca::tests::the_stored_key_is_sealed_rather_than_readable ... ok
test pki::ca::tests::the_validity_follows_the_configured_window ... ok
test pki::ca::tests::an_rsa_authority_works_too ... ok
test pki::keys::tests::a_key_never_renders_its_secret_half ... ok
test pki::keys::tests::an_ecdsa_key_round_trips_through_its_pkcs8_encoding ... ok
test pki::keys::tests::an_rsa_key_round_trips_through_its_pkcs8_encoding ... ok
test pki::keys::tests::every_key_type_signs_with_the_algorithm_its_certificates_declare ... ok
test pki::keys::tests::rubbish_is_not_a_private_key ... ok
test pki::keys::tests::two_generated_keys_are_different_keys ... ok
test pki::pem::tests::a_bare_body_wraps_at_64_columns_and_ends_with_a_newline ... ok
test pki::pem::tests::a_body_that_is_exactly_one_line_gets_exactly_one_newline ... ok
test pki::pem::tests::a_chain_keeps_the_order_it_was_written_in ... ok
test pki::pem::tests::a_file_with_no_certificate_is_a_user_error ... ok
test pki::pem::tests::a_fingerprint_is_lowercase_hex_with_no_separators ... ok
test pki::pem::tests::a_key_sharing_the_file_is_skipped_rather_than_refused ... ok
test pki::pem::tests::a_pem_certificate_is_the_bare_body_between_the_armour ... ok
test pki::pem::tests::a_rendered_certificate_reads_back_as_the_same_bytes ... ok
test pki::pem::tests::an_empty_body_is_still_newline_terminated ... ok
test store::append_log::tests::a_hundred_thousand_appends_are_fast_enough ... ignored, throughput sanity check; run with --ignored
test store::append_log::tests::a_hostile_stream_key_cannot_escape_its_directory ... ok
test store::append_log::tests::a_full_segment_rolls_to_the_next_file ... ok
test store::append_log::tests::a_missing_segment_file_does_not_stop_the_log ... ok
test store::append_log::tests::a_partial_frame_left_by_a_crash_is_truncated_on_open ... ok
test store::append_log::tests::a_record_larger_than_a_segment_still_gets_its_own_file ... ok
test store::append_log::tests::a_record_larger_than_the_frame_limit_is_refused ... ok
test store::append_log::tests::a_reopened_log_carries_on_in_the_same_segment ... ok
test store::append_log::tests::a_window_reads_only_the_segments_it_touches ... ok
test store::append_log::tests::an_index_row_claiming_more_than_its_file_is_sealed_rather_than_appended_to ... ok
test store::append_log::tests::pruning_leaves_the_segment_still_being_written ... ok
test store::append_log::tests::pruning_unlinks_sealed_segments_and_forgets_their_rows ... ok
test store::append_log::tests::records_read_back_in_write_order ... ok
test store::append_log::tests::records_that_never_reached_the_index_are_counted_on_open ... ok
test store::append_log::tests::the_unflushed_tail_of_the_open_segment_is_still_readable ... ok
test store::append_log::tests::two_streams_keep_separate_directories ... ok
test store::content::tests::a_blob_is_named_by_its_sha256 ... ok
test store::content::tests::a_blob_lands_in_a_fanout_directory ... ok
test store::content::tests::a_blob_reads_back_byte_for_byte ... ok
test store::content::tests::a_hash_that_is_not_a_hash_is_refused ... ok
test store::content::tests::an_empty_blob_is_storable ... ok
test store::content::tests::listing_an_absent_store_is_empty_rather_than_an_error ... ok
test store::content::tests::listing_reports_every_blob_and_ignores_anything_else ... ok
test store::content::tests::nothing_is_left_in_the_temporary_directory ... ok
test store::content::tests::opening_a_missing_blob_is_a_user_error ... ok
test store::content::tests::removing_reports_whether_anything_was_there ... ok
test store::content::tests::storing_the_same_bytes_twice_is_a_no_op ... ok
test store::frame::tests::a_torn_length_prefix_stops_the_scan ... ok
test store::frame::tests::a_torn_payload_stops_the_scan_at_the_last_whole_record ... ok
test store::frame::tests::a_varint_longer_than_ten_bytes_is_corruption ... ok
test store::frame::tests::a_varint_round_trips_at_every_width ... ok
test store::frame::tests::an_absurd_length_is_refused_rather_than_allocated ... ok
test store::frame::tests::an_empty_segment_yields_nothing ... ok
test store::frame::tests::every_complete_record_is_yielded_in_order ... ok
test store::frame::tests::small_lengths_use_a_single_byte_the_way_protobuf_does ... ok
test store::segment::tests::a_key_is_encoded_reversibly_and_never_as_a_traversal ... ok
test store::segment::tests::a_segment_accumulates_until_it_is_asked_for_its_pending_counts ... ok
test store::segment::tests::a_segment_knows_when_it_is_full_and_what_window_it_covers ... ok
test store::segment::tests::an_encoded_key_always_fits_a_path_component ... ok
test store::segment::tests::an_unusable_key_is_refused_rather_than_mangled ... ok
test store::segment::tests::repairing_a_file_that_is_not_there_reports_so ... ok
test store::segment::tests::repairing_counts_whole_records_and_trims_the_rest ... ok
test store::segment::tests::segment_names_sort_in_write_order_and_never_repeat ... ok

test result: ok. 117 passed; 0 failed; 1 ignored; 0 measured; 270 filtered out; finished in 0.71s
```

(The substring filter also pulls in `crypto::store::` and `config::auth::`/`config::pki::` tests,
which are M0-06's and M0-08's and also pass; they are elided above for length.)

```
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.77s

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.59s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 6 other files

$ cargo fmt --all --check
(no output; exit 0)

$ ./scripts/check-file-length.sh
(no output; exit 0)
```

`check-file-length.sh` enumerates with `git ls-files`, so it does not yet see these files — they are
untracked and this brief makes no `git`/`but` writes. Every new file was measured by hand with the
script's own awk rule:

```
rustak-server/src/store/mod.rs                   6
rustak-server/src/store/frame.rs                52
rustak-server/src/store/segment.rs             228
rustak-server/src/store/content.rs             209
rustak-server/src/store/append_log.rs          260
rustak-server/src/pki/mod.rs                     6
rustak-server/src/pki/keys.rs                   39
rustak-server/src/pki/ca.rs                    285
rustak-server/src/pki/pem.rs                    53
rustak-server/src/auth/mod.rs                    2
rustak-server/src/auth/jwt.rs                  268
```

`store/append_log.rs` was 426 lines before `store/segment.rs` was split out of it; the split is by
responsibility (the log orchestrates, the segment is one file), not by line count.

Whole-workspace regression check: `cargo test --workspace` → 12 binaries, all `ok`, 0 failed.

## Notes for the briefs that follow

- **M0-11 / M2 (`auth/`)**: `auth/mod.rs` declares only `pub mod jwt;`. Add `principal`, `resolve`,
  `basic`, `bearer`, `ratelimit`, `cache`, `acl`, `oauth_server/*` beside it. `JwtIssuer::verify`
  returns `AccessClaims` with the `jti` the `revoked_jtis` repo keys on; revocation is deliberately
  *not* checked inside `verify`, because that would make a pure function `async` and put a database
  read on every request — the bearer middleware should do it.
- **M2 (`pki/`)**: `CaMaterial::issuer()` hands out the `rcgen::Issuer` `issue_client_cert` needs,
  and `chain_der()` the chain `signClient/v2` returns. See deviation 1 above before moving the CA
  record into `certificates(kind='ca')`.
- **M1 (CoT history)**: `AppendLog::flush()` is the batching seam — append a batch, flush once, and
  SQLite sees one write per batch rather than one per record (the log also self-flushes every 256
  records or 256 KiB). `AppendLog::prune_before` is the retention job's entry point.
- **`ContentStore::prepare()`** should be called at startup so a bad `content_dir` fails at boot;
  `put` calls it too, so it is safe either way.
