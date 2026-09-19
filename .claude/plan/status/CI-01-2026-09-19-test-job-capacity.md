# CI-01 — The `Test` job's risk is runner variance, not test count

**Correction notice.** The first version of this note claimed the `Test` job was
structurally at 27 of its 30 minutes and proposed splitting it. A second
measurement showed that reading was wrong, and the note has been rewritten. The
original conclusion is kept in §4 so the mistake is visible rather than quietly
replaced.

---

## 1. The two runs

| Run | Commit | `Test` wall | In-test | Binaries | Tests |
|---|---|---:|---:|---:|---:|
| [35406723759](https://github.com/SierraSoftworks/rustak/actions/runs/35406723759) | `789a2bd` | **27m02s** | 1365 s | 31 | 2512 |
| [35407353006](https://github.com/SierraSoftworks/rustak/actions/runs/35407353006) | `e101336` | **11m40s** | 483 s | 32 | **2634** |

Both green. The second run has **more** tests and takes **less than half** the
time — so the cost is not the test count and not the new suites.

**Two further samples, added as they arrived** (the rule this note ends with,
applied to itself):

| Run | Commit | `Test` wall |
|---|---|---:|
| 35411593505 | `b58ce8e` (M1-10, stream robustness) | 13m35s |
| 35411906247 | `6af709c` (docs) | 14m30s |

Four samples now: **11m40s, 13m35s, 14m30s** and the single **27m02s**. The
normal band is 11–15 minutes and the outlier stands alone, which is what §2
predicts and what §4's withdrawal rests on. It also means the suite's own growth
is visible and slow — `6af709c` carries the most tests of the four and sits at
the top of the normal band, not outside it.

Per binary, the same tests:

| Binary | `789a2bd` | `e101336` | Ratio |
|---|---:|---:|---:|
| `stream_session` (11) | 257.9 s | 62.0 s | 4.2× |
| `marti_channels` (11) | 224.5 s | 54.4 s | 4.1× |
| `stream_routing` (12) | 213.5 s | 81.3 s | 2.6× |
| `stream_store` (8) | 182.9 s | 52.2 s | 3.5× |
| `rustak_server` lib (1682) | 217.5 s | 102.2 s | 2.1× |

**The runner was between two and four times slower.** That is ordinary variance
on GitHub's two-vCPU hosts — a noisy neighbour, nothing in this repository.

## 2. So what is the actual risk

The job's normal cost is **about 12 minutes**, comfortably inside 30. The risk
is that a bad runner multiplies it by ~2.8, and 12 × 2.8 = 34 — already past the
bound. The 27-minute run was that case with a little room left.

So the bound is not thin because the suite is heavy; it is thin because the
suite's cost varies by nearly 3× for reasons outside our control, and the bound
was set against the good case.

## 3. What is still true from the first measurement

The distribution of cost within a run is real and unchanged by the variance:

- The four stream/Marti binaries are **64% of all in-test time** on 42 tests,
  while the library's 1682 tests cost about 0.06–0.13 s each.
- It is **not** sleeps (`stream_support` has one 400 ms `SETTLE`; `EXPECT` is a
  5 s timeout paid only on failure) and **not** RSA
  (`stream_support/mod.rs:98` already sets `KeyType::EcdsaP256`).
- It is `-Cinstrument-coverage` over real mutual-TLS handshakes. Measured
  directly: `stream_session`'s eleven tests run **≈13 s** uninstrumented on this
  machine against 62–258 s in CI.

Those suites are doing real handshakes because that is what they exist to prove;
making them cheaper would mean making them prove less. I have not touched them.

## 4. What I proposed first, and why I withdrew it

I proposed splitting `test` into two parallel jobs (`--lib --bins` and
`--test '*'`), each uploading its own coverage for codecov to merge. On the
corrected reading that is a large change — the `ci` aggregator's `needs:`, the
required-check configuration and the number of codecov uploads all move — bought
against a problem that occurs on a slow runner rather than every run. It is not
justified yet.

**What I would do instead, if anything:** nothing, and keep watching. Two data
points do not establish a distribution. If a second run crosses ~20 minutes, the
cheapest honest response is to raise `timeout-minutes` on `test` from 30 to 45
and say plainly in `docs/ci.md` that the number covers runner variance rather
than the suite's own cost — which is a different statement from the one
`docs/ci.md` currently makes about timeouts being bug reports, and worth writing
down as such.

## 5. The lesson for this log

One run is not a measurement. The 27-minute figure was real, and the conclusion
drawn from it was wrong, because I read a single sample as a trend on a shared
two-vCPU host whose speed varies by a factor of three. Anything I report about
CI timing from here carries at least two samples or says that it does not.
