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

Six samples now: **11m40s, 13m35s, 14m30s, 15m26s, 16m12s** and the single
**27m02s**.

Those five normal samples are **monotonically increasing**, which looked like the
suite growing — so I checked before saying so, and it is not. In-test seconds
against test count:

| Commit | In-test | Tests |
|---|---:|---:|
| `e101336` | 491 s | 2698 |
| `b58ce8e` | 577 s | 2718 |
| `6af709c` | 611 s | 2718 |
| `6baf409` | 667 s | 2723 |
| `f4fb037` | 731 s | 2723 |

**Test count grew 0.9% while time grew 49%**, and the two pairs that share an
identical test count still differ by 6% and 10%. So the count explains none of
it.

The per-binary comparison between the fastest and slowest of the five says the
rest plainly — the slowdown is **uniform and multiplicative across every binary,
including ones neither landing touched**:

| Binary | `e101336` | `f4fb037` | |
|---|---:|---:|---:|
| `bootstrap` (3 tests, untouched) | 14.4 s | 48.5 s | +237% |
| `mission_dest` | 28.1 s | 65.6 s | +133% |
| `marti_channels` | 54.4 s | 99.4 s | +83% |
| `enroll_flows` (untouched) | 4.9 s | 8.1 s | +65% |
| `stream_session` | 62.0 s | 101.0 s | +63% |
| `rustak_server` lib | 102.2 s | 117.2 s | +15% |

If M1-10's stream work had added waits, the cost would sit in the stream suites.
Instead the *smallest* suites are hit hardest, which is the signature of a fixed
per-binary overhead on a slower host — process start and the `.profraw` each
instrumented binary writes on exit. Environmental, not ours.

**So there is still no trend, and no action.** A monotonic run of five is
striking, and it is also what you get by sampling six shared hosts of varying
speed in some order. The trigger in §4 stands unchanged: another run past ~20
minutes, with the per-binary shape showing the cost landing *where the code
changed*, would be the thing worth acting on.

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
