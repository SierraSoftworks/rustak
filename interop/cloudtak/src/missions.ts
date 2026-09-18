/**
 * The Data Sync half of CloudTAK's REST API: missions, their contents, their
 * change log, and data packages.
 *
 * Same rules as `src/api.ts` — pure builders and parsers, transport elsewhere —
 * and the same reason: this is the part of the surface M4 exists to serve, so
 * its shapes are asserted against fixtures rather than only against a server.
 *
 * Two CloudTAK behaviours are worth knowing while reading this:
 *
 * - **`PUT /marti/missions/{guid}/cot` is not a CoT POST.** CloudTAK builds a
 *   mission data package out of the features, uploads it to the mission and
 *   then *confirms* the result by reading the mission back, so a `200` here
 *   means every feature really is in the Data Sync. It is the strongest single
 *   assertion in the suite.
 * - **A mission GUID routes differently from a mission name.** node-tak sends
 *   `/Marti/api/missions/guid/{guid}/…` when the value parses as a UUID, so
 *   every call below goes through the GUID form — the one CloudTAK uses in
 *   anger, and the one a name-only implementation fails.
 */

import { expectObject, expectTakList, type Call, type Content, type Mission, type MissionChange } from "./api.js";

/** How long a marker stays fresh. Minutes, because a run is minutes. */
const STALE_MS = 10 * 60 * 1000;

/** Creates a Data Sync. CloudTAK adds `creatorUid: ANDROID-CloudTAK-{email}` itself. */
export function createMission(options: {
  readonly name: string;
  readonly description: string;
  readonly keywords: readonly string[];
  readonly groups?: readonly string[];
}): Call {
  return {
    method: "POST",
    path: "/api/marti/mission",
    body: {
      name: options.name,
      description: options.description,
      keywords: [...options.keywords],
      ...(options.groups === undefined ? {} : { group: [...options.groups] }),
    },
  };
}

/** Reads one mission back, contents and all. */
export function getMission(guid: string): Call {
  return { method: "GET", path: `/api/marti/missions/${encodeURIComponent(guid)}` };
}

/** The bare mission object CloudTAK returns from create and get alike. */
export function parseMission(what: string, body: unknown): Mission {
  const object = expectObject(what, body);

  const name = object.name;
  const guid = object.guid;

  if (typeof name !== "string" || typeof guid !== "string" || guid.length === 0) {
    throw new Error(
      `${what} answered without a name and a guid — CloudTAK stores the guid as the Data Sync's identity and every later call routes on it. Keys: ${Object.keys(object).join(", ")}`,
    );
  }

  return {
    name,
    guid,
    keywords: Array.isArray(object.keywords) ? (object.keywords as string[]) : [],
    createTime: typeof object.createTime === "string" ? object.createTime : "",
    token: typeof object.token === "string" ? object.token : undefined,
    contents: Array.isArray(object.contents) ? (object.contents as Mission["contents"]) : [],
  };
}

/** One marker, in the GeoJSON CloudTAK's map submits. */
export function markerFeature(options: {
  readonly uid: string;
  readonly callsign: string;
  readonly longitude: number;
  readonly latitude: number;
  readonly now?: Date;
}): Record<string, unknown> {
  const now = options.now ?? new Date();
  const stale = new Date(now.getTime() + STALE_MS);

  return {
    id: options.uid,
    type: "Feature",
    properties: {
      callsign: options.callsign,
      type: "a-f-G-U-C",
      how: "h-g-i-g-o",
      time: now.toISOString(),
      start: now.toISOString(),
      stale: stale.toISOString(),
    },
    geometry: { type: "Point", coordinates: [options.longitude, options.latitude] },
  };
}

/** Submits features to a Data Sync, with the confirmation CloudTAK insists on. */
export function submitFeatures(guid: string, features: readonly Record<string, unknown>[]): Call {
  return {
    method: "PUT",
    path: `/api/marti/missions/${encodeURIComponent(guid)}/cot`,
    body: { features: [...features] },
  };
}

/** The uids CloudTAK confirmed are in the mission. */
export function parseSubmitted(body: unknown): string[] {
  const object = expectObject("PUT /api/marti/missions/{guid}/cot", body);

  if (!Array.isArray(object.uids)) {
    throw new Error(
      `CloudTAK confirmed the submission without listing uids, which means it could not read the mission back. Answer: ${JSON.stringify(object).slice(0, 300)}`,
    );
  }

  return object.uids as string[];
}

/** Uploads a file and attaches it to the mission in one call. */
export function uploadFile(guid: string, name: string, bytes: Buffer): Call {
  return {
    method: "POST",
    path: `/api/marti/missions/${encodeURIComponent(guid)}/upload?name=${encodeURIComponent(name)}`,
    raw: bytes,
    contentType: "application/octet-stream",
  };
}

/**
 * Accepts what attaching answered.
 *
 * CloudTAK returns the TAK envelope from `PUT …/contents` verbatim, and TAK
 * Server's own answer there is the updated mission — so the useful assertion is
 * that it parsed at all and that the hash shows up when the mission is read
 * back, which is what the step does.
 */
export function parseAttached(body: unknown): Record<string, unknown> {
  return expectObject("POST /api/marti/missions/{guid}/upload", body);
}

/** The mission's change log, which is what a subscribed client polls. */
export function missionChanges(guid: string, secago = 3600): Call {
  return {
    method: "GET",
    path: `/api/marti/missions/${encodeURIComponent(guid)}/changes?secago=${String(secago)}`,
  };
}

/** The changes, oldest first is not guaranteed — the step matches rather than indexes. */
export function parseChanges(body: unknown): MissionChange[] {
  const changes = expectTakList<MissionChange>("GET /api/marti/missions/{guid}/changes", body);

  for (const change of changes) {
    if (typeof change.type !== "string") {
      throw new Error(`a mission change arrived without a type: ${JSON.stringify(change).slice(0, 200)}`);
    }
  }

  return changes;
}

/** Whether a change log records a content addition for this uid. */
export function addsContent(changes: readonly MissionChange[], uid: string): boolean {
  return changes.some((change) => change.type === "ADD_CONTENT" && change.contentUid === uid);
}

/** Whether a change log records a file addition with this hash. */
export function addsResource(changes: readonly MissionChange[], hash: string): boolean {
  return changes.some(
    (change) => change.type === "ADD_CONTENT" && change.contentResource?.hash === hash,
  );
}

/**
 * Shares a data package built from features.
 *
 * `public: true` is the branch that uploads through `/Marti/sync/missionupload`
 * and publishes the package to the server's package list; the private branch
 * uploads it as an ordinary file instead. An empty package is refused by
 * CloudTAK before it reaches rustak, so a feature is always included.
 */
export function sharePackage(options: {
  readonly name: string;
  readonly keywords: readonly string[];
  readonly features: readonly Record<string, unknown>[];
}): Call {
  return {
    method: "PUT",
    path: "/api/marti/package",
    body: {
      name: options.name,
      public: true,
      keywords: [...options.keywords],
      destinations: [],
      assets: [],
      basemaps: [],
      features: [...options.features],
    },
  };
}

/** What the TAK Server stored, as CloudTAK reports it. */
export function parseContent(what: string, body: unknown): Content {
  const object = expectObject(what, body);
  const hash = object.Hash;
  const name = object.Name;

  if (typeof hash !== "string" || hash.length === 0) {
    throw new Error(
      `${what} answered without a 'Hash' — the content hash is how every later call names the file. Keys: ${Object.keys(object).join(", ")}`,
    );
  }

  return {
    UID: typeof object.UID === "string" ? object.UID : hash,
    Hash: hash,
    Name: typeof name === "string" ? name : "",
    SubmissionUser: typeof object.SubmissionUser === "string" ? object.SubmissionUser : undefined,
    CreatorUid: typeof object.CreatorUid === "string" ? object.CreatorUid : undefined,
  };
}

/** Lists published packages, which is `/Marti/sync/search` on the far side. */
export function listPackages(filter: string): Call {
  return { method: "GET", path: `/api/marti/package?filter=${encodeURIComponent(filter)}` };
}

/** One row of the package list: CloudTAK folds the search results by UID. */
export interface PackageSummary {
  readonly uid: string;
  readonly name: string;
  readonly hash: string;
}

/** The package list, as CloudTAK's own pagination shape rather than a TAK envelope. */
export function parsePackages(body: unknown): PackageSummary[] {
  const object = expectObject("GET /api/marti/package", body);

  if (!Array.isArray(object.items)) {
    throw new Error(
      `GET /api/marti/package answered without an 'items' array. Keys: ${Object.keys(object).join(", ")}`,
    );
  }

  return (object.items as Record<string, unknown>[]).map((item) => ({
    uid: typeof item.uid === "string" ? item.uid : String(item.uid ?? ""),
    name: typeof item.name === "string" ? item.name : "",
    hash: typeof item.hash === "string" ? item.hash : "",
  }));
}
