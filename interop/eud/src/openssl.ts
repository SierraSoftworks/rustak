/**
 * The two PKCS#12 jobs the runner has, both done with the `openssl` CLI.
 *
 * 1. **The truststore.** `estream:` takes a PKCS#12 holding the authority the
 *    EUD will verify rustak's certificate against — ATAK ships one, and the
 *    runner has to build the equivalent from the CA rustak generated at first
 *    start. Nothing in Node writes PKCS#12, so this shells out, the way CI
 *    shells out to `openssl` everywhere else.
 * 2. **Reading back what rustak issued.** `estream:` drops the keystore it was
 *    given at `<output-dir>/commo-enroll-cert.p12`, so the certificate rustak
 *    signed can be inspected with ordinary tooling — which is how a scenario
 *    asserts on the subject ATAK's own CSR asked for.
 *
 * Both paths try the legacy PKCS#12 algorithms first. ATAK's own keystores are
 * SHA-1/3DES, `commoncommo` writes what OpenSSL's defaults were when its
 * `cryptoutil.cpp` was written, and OpenSSL 3 needs `-legacy` for the oldest of
 * them. Preferring the old shape and falling back to the modern one keeps the
 * suite working against both without asking which it got.
 */

import { spawnSync, type SpawnSyncReturns } from "node:child_process";

/** The `openssl` to use, so a runner with a second one can say which. */
const OPENSSL = process.env.RUSTAK_EUD_OPENSSL ?? "openssl";

/** Whether `openssl` is here at all. */
export function opensslAvailable(): boolean {
  try {
    return spawnSync(OPENSSL, ["version"], { stdio: "ignore", timeout: 30_000 }).status === 0;
  } catch {
    return false;
  }
}

/** Runs `openssl`, capturing both streams. */
function run(args: readonly string[], input?: string): SpawnSyncReturns<string> {
  return spawnSync(OPENSSL, args, { encoding: "utf8", input, timeout: 60_000 });
}

/**
 * Writes a PKCS#12 truststore holding one certificate authority.
 *
 * @throws when neither the legacy nor the modern algorithm set produces a file,
 * because a scenario without a truststore cannot enrol and would otherwise fail
 * as though rustak had refused it.
 */
export function exportTruststore(caFile: string, outFile: string, password: string): string {
  const base = [
    "pkcs12",
    "-export",
    "-nokeys",
    "-in",
    caFile,
    "-out",
    outFile,
    "-name",
    "rustak-ca",
    "-passout",
    `pass:${password}`,
  ];

  const attempts: { label: string; args: string[] }[] = [
    // What a Java/ATAK truststore looks like, and what every OpenSSL reads.
    { label: "sha1/3des", args: [...base, "-certpbe", "PBE-SHA1-3DES", "-macalg", "sha1"] },
    { label: "defaults", args: base },
  ];

  const failures: string[] = [];

  for (const attempt of attempts) {
    const result = run(attempt.args);

    if (result.status === 0) return attempt.label;

    failures.push(`${attempt.label}: ${(result.stderr ?? "").trim() || `exit ${result.status}`}`);
  }

  throw new Error(
    `could not build a PKCS#12 truststore from ${caFile}:\n  ${failures.join("\n  ")}`,
  );
}

/**
 * The subject of the client certificate inside a PKCS#12, normalised.
 *
 * `CN=alice, O=rustak` — RDNs in DER order, no spaces around the equals signs,
 * so a scenario can assert that the common name comes first without caring
 * which `-nameopt` this OpenSSL defaults to. Returns `undefined` when the file
 * is not there or cannot be opened, which the caller reports as its own
 * failure rather than a crash.
 */
export function readCertSubject(p12File: string, password: string): string | undefined {
  const attempts = [
    ["pkcs12", "-in", p12File, "-passin", `pass:${password}`, "-nokeys", "-clcerts", "-legacy"],
    ["pkcs12", "-in", p12File, "-passin", `pass:${password}`, "-nokeys", "-clcerts"],
  ];

  for (const args of attempts) {
    const extracted = run(args);

    if (extracted.status !== 0 || !extracted.stdout.includes("BEGIN CERTIFICATE")) continue;

    const subject = run(["x509", "-noout", "-subject"], extracted.stdout);

    if (subject.status === 0) return normaliseSubject(subject.stdout);
  }

  return undefined;
}

/** `subject=CN = alice, O = rustak` → `CN=alice, O=rustak`. */
export function normaliseSubject(printed: string): string {
  return printed
    .trim()
    .replace(/^subject\s*=\s*/, "")
    .replace(/^\//, "")
    .replace(/\s*=\s*/g, "=")
    .replace(/\s*,\s*/g, ", ")
    .trim();
}
