/**
 * Everything the suite decides once: paths, ports, images and names.
 *
 * Kept in one file because the compose file, the runner and the unit tests all
 * have to agree about them — the tests assert that `docker-compose.yml` and
 * these defaults still say the same thing, which is the only way a port or an
 * image tag edited in one place cannot silently diverge from the other.
 */

import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

/** `interop/cloudtak`, the compose project directory. */
export const SUITE_ROOT = path.resolve(here, "..");

/** The repository root, three levels up. */
export const REPO_ROOT = path.resolve(SUITE_ROOT, "..", "..");

/** The working directory the stack is given: generated, gitignored, thrown away. */
export const RUN_DIR = path.join(SUITE_ROOT, ".run");

/** The test CA and the server certificate rustak serves on `[web.public]`. */
export const PKI_DIR = path.join(RUN_DIR, "pki");

/** rustak's `[server] data_dir`, bind-mounted at `/data`. */
export const DATA_DIR = path.join(RUN_DIR, "rustak");

/**
 * The certificate `[web.public.tls] mode = "files"` is pointed at.
 *
 * Inside the data directory, because that is the only thing mounted into the
 * container — but *not* in `pki/`, which is where rustak keeps the internal CA
 * it issues client certificates from.
 */
export const TLS_DIR = path.join(DATA_DIR, "tls");

/** The same pair, as the container sees them. */
export const CONTAINER_TLS = {
  cert: "/data/tls/fullchain.pem",
  key: "/data/tls/server.key",
} as const;

/** Where screenshots, the compose log and a failed run's evidence are kept. */
export const ARTIFACT_DIR = process.env.RUSTAK_INTEROP_ARTIFACTS ?? path.join(SUITE_ROOT, "artifacts");

/** A port the stack publishes on the loopback, and the environment that moves it. */
function port(variable: string, fallback: number): number {
  const raw = process.env[variable];
  const parsed = raw === undefined ? Number.NaN : Number(raw);

  return Number.isInteger(parsed) && parsed > 0 ? parsed : fallback;
}

/** The host-side ports, matching `docker-compose.yml`'s defaults. */
export const PORTS = {
  /** CloudTAK's `webtak`: `/api/v1`, `/oauth/token`, `/Marti/api/tls/*`. */
  webtak: port("RUSTAK_INTEROP_WEBTAK_PORT", 8446),

  /** CloudTAK's `api`: the mutually authenticated Marti listener. */
  marti: port("RUSTAK_INTEROP_MARTI_PORT", 8443),

  /** CloudTAK's `url`: the CoT stream. */
  stream: port("RUSTAK_INTEROP_STREAM_PORT", 8089),

  /** CloudTAK's own API and UI. */
  cloudtak: port("RUSTAK_INTEROP_CLOUDTAK_PORT", 5000),
} as const;

/**
 * The three URLs CloudTAK is configured with — as the *containers* see them.
 *
 * Service names, not `localhost`: CloudTAK dials rustak across the compose
 * network, so the certificate the runner mints has to name `rustak` as well as
 * the loopback the runner itself uses.
 */
export const CONTAINER_URLS = {
  stream: "ssl://rustak:8089",
  api: "https://rustak:8443",
  webtak: "https://rustak:8446",
} as const;

/** The host names the server certificate must carry, for both callers. */
export const SERVER_NAMES = ["rustak", "localhost"] as const;

/** Where the runner reaches each half of the stack from outside the network. */
export const HOST_URLS = {
  webtak: `https://localhost:${PORTS.webtak}`,
  marti: `https://localhost:${PORTS.marti}`,
  stream: `ssl://localhost:${PORTS.stream}`,
  cloudtak: `http://localhost:${PORTS.cloudtak}`,
} as const;

/**
 * The rustak image the stack runs.
 *
 * Built by the nightly job from `rustak-server/Dockerfile` with a release
 * binary in `dist/`; a developer builds it the same way (README → Running it).
 */
export const RUSTAK_IMAGE = process.env.RUSTAK_INTEROP_RUSTAK_IMAGE ?? "rustak-interop-cloudtak:local";

/**
 * The published CloudTAK image tag the suite pins.
 *
 * `ghcr.io/dfpc-coe/cloudtak-api` is tagged `v<semver>` by CloudTAK's own GHCR
 * workflow on every release tag. Bump this deliberately: a CloudTAK release is
 * exactly the kind of change this suite exists to catch.
 */
export const CLOUDTAK_TAG = process.env.RUSTAK_INTEROP_CLOUDTAK_TAG ?? "v13.89.0";

/** The accounts and objects a run creates, all named so a stray one is obvious. */
export const NAMES = {
  /** rustak's first administrator, created by the shared bootstrap. */
  admin: "interop-admin",

  /** The account CloudTAK authenticates as, and CloudTAK's own system admin. */
  operator: "cloudtak-operator",

  /** The channel the operator is granted, and the Data Sync is scoped to. */
  channel: "Interop",

  /** The Data Sync (mission) the run creates. */
  mission: "rustak-interop-cloudtak",

  /** The marker the run puts in it. */
  markerCallsign: "Interop Marker",

  /** The file the run attaches to it. */
  file: "interop-notes.txt",

  /** The data package the run shares. */
  package: "rustak-interop-package",
} as const;
