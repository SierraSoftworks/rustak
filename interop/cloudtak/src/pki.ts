/**
 * The test CA the stack is built around, and the CSR CloudTAK's admin
 * certificate is minted from.
 *
 * `[web.public.tls] mode = "files"` rather than `"internal"` on purpose: this
 * is the deployment shape CloudTAK forces on an operator. Its `webtak` calls go
 * through undici with full system-CA verification and no override
 * (`compat/cloudtak.md` §3), so the authority has to be one the container can
 * be *told* about — a file to hand to `NODE_EXTRA_CA_CERTS`. rustak's own
 * internal CA still exists inside the container and still issues the client
 * certificates; only the public listener's server certificate comes from here.
 *
 * `openssl` rather than a library: the EUD suite already requires it, it is on
 * every CI image, and a test CA is exactly the kind of thing that should be
 * built by the tool operators use rather than by three hundred lines of ASN.1.
 */

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

import { PKI_DIR, SERVER_NAMES, TLS_DIR } from "./settings.js";

/** How long the generated material is valid. A run lasts minutes. */
const DAYS = "30";

/** The key size. RSA because every TAK client in the field accepts it. */
const KEY = "rsa:2048";

/** Where each generated file lands. */
export const PKI = {
  caCert: path.join(PKI_DIR, "ca.crt"),
  caKey: path.join(PKI_DIR, "ca.key"),
  serverKey: path.join(PKI_DIR, "server.key"),
  serverCert: path.join(PKI_DIR, "server.crt"),
  fullChain: path.join(PKI_DIR, "fullchain.pem"),
} as const;

/** Whether `openssl` is on PATH at all. */
export function opensslAvailable(): boolean {
  try {
    return spawnSync("openssl", ["version"], { stdio: "ignore", timeout: 15_000 }).status === 0;
  } catch {
    return false;
  }
}

/** One `openssl` invocation, with its own output in the failure. */
function openssl(args: readonly string[]): void {
  const result = spawnSync("openssl", [...args], { encoding: "utf8", timeout: 60_000 });

  if (result.status !== 0) {
    throw new Error(
      `openssl ${args.join(" ")} failed (${String(result.status)}): ${result.stderr || result.stdout}`,
    );
  }
}

/** The `subjectAltName` line the server certificate needs, for both callers. */
export function serverAltNames(names: readonly string[] = SERVER_NAMES): string {
  return [...names.map((name) => `DNS:${name}`), "IP:127.0.0.1"].join(",");
}

/**
 * Generates the authority and the server certificate, and stages the pair the
 * container reads.
 *
 * Idempotent by deletion: the directory is rebuilt from scratch every run, so a
 * half-written chain from an interrupted one cannot be picked up.
 */
export function generatePki(): void {
  fs.rmSync(PKI_DIR, { recursive: true, force: true });
  fs.mkdirSync(PKI_DIR, { recursive: true });

  openssl([
    "req", "-x509", "-newkey", KEY, "-nodes",
    "-keyout", PKI.caKey, "-out", PKI.caCert, "-days", DAYS,
    "-subj", "/CN=rustak interop test CA/O=rustak interop",
    "-addext", "basicConstraints=critical,CA:TRUE,pathlen:0",
    "-addext", "keyUsage=critical,keyCertSign,cRLSign",
  ]);

  const csr = path.join(PKI_DIR, "server.csr");
  const extensions = path.join(PKI_DIR, "server.ext");

  fs.writeFileSync(
    extensions,
    [
      "basicConstraints=critical,CA:FALSE",
      "keyUsage=critical,digitalSignature,keyEncipherment",
      "extendedKeyUsage=serverAuth",
      `subjectAltName=${serverAltNames()}`,
      "",
    ].join("\n"),
    "utf8",
  );

  openssl([
    "req", "-newkey", KEY, "-nodes",
    "-keyout", PKI.serverKey, "-out", csr,
    "-subj", `/CN=${SERVER_NAMES[0]}/O=rustak interop`,
  ]);

  openssl([
    "x509", "-req", "-in", csr,
    "-CA", PKI.caCert, "-CAkey", PKI.caKey, "-CAcreateserial",
    "-out", PKI.serverCert, "-days", DAYS,
    "-extfile", extensions,
  ]);

  // rustak's `cert_file` is the full chain: the leaf first, then the authority
  // that signed it, so a client holding only the root can build the path.
  fs.writeFileSync(
    PKI.fullChain,
    `${fs.readFileSync(PKI.serverCert, "utf8").trimEnd()}\n${fs.readFileSync(PKI.caCert, "utf8").trimEnd()}\n`,
    "utf8",
  );

  fs.mkdirSync(TLS_DIR, { recursive: true });
  fs.copyFileSync(PKI.fullChain, path.join(TLS_DIR, "fullchain.pem"));
  fs.copyFileSync(PKI.serverKey, path.join(TLS_DIR, "server.key"));
  fs.chmodSync(path.join(TLS_DIR, "server.key"), 0o600);
}

/** A private key and the certificate request that goes with it. */
export interface KeyAndRequest {
  /** The PEM private key, which never leaves this machine. */
  readonly key: string;

  /** The PEM certificate request, ready to be posted to `signClient/v2`. */
  readonly csr: string;
}

/**
 * Builds the key and CSR for the certificate CloudTAK stores as its admin
 * connection (`server.auth`).
 *
 * The CN must be the account the Basic credentials authenticate as — rustak
 * checks it, case-insensitively, and replaces the rest of the subject with its
 * own configured `O`/`OU` (`compat/enrollment.md` §3). The `O`/`OU` here are
 * what `GET /Marti/api/tls/config` asked for, so the request is the one a real
 * client would build.
 */
export function generateClientRequest(
  commonName: string,
  organisation: string,
  unit: string,
): KeyAndRequest {
  const directory = fs.mkdtempSync(path.join(PKI_DIR, "client-"));
  const keyFile = path.join(directory, "client.key");
  const csrFile = path.join(directory, "client.csr");

  openssl([
    "req", "-newkey", KEY, "-nodes",
    "-keyout", keyFile, "-out", csrFile,
    "-subj", `/CN=${commonName}/O=${organisation}/OU=${unit}`,
  ]);

  return {
    key: fs.readFileSync(keyFile, "utf8"),
    csr: fs.readFileSync(csrFile, "utf8"),
  };
}
