/**
 * The Data Sync half of a run: a mission, a marker, a file, the change log and
 * a shared package.
 *
 * This is what M4 exists to serve, driven the way CloudTAK drives it — every
 * call routed on the mission **guid**, the marker submitted as a mission data
 * package that CloudTAK confirms by reading the mission back, and the package
 * published through `/Marti/sync/missionupload` rather than stored in
 * CloudTAK's own object store.
 */

import crypto from "node:crypto";

import {
  addsContent,
  addsResource,
  createMission,
  getMission,
  listPackages,
  markerFeature,
  missionChanges,
  parseAttached,
  parseChanges,
  parseContent,
  parseMission,
  parsePackages,
  parseSubmitted,
  sharePackage,
  submitFeatures,
  uploadFile,
} from "./missions.js";
import { NAMES } from "./settings.js";
import { contentHash, eventually, mission, type Step } from "./step-kit.js";

export const DATASYNC_STEPS: readonly Step[] = [
  {
    name: "data-sync",
    requires: ["missions"],
    async run({ cloudtak, state }) {
      const name = `${NAMES.mission}-${crypto.randomUUID().slice(0, 8)}`;

      const created = parseMission(
        "POST /api/marti/mission",
        await cloudtak.run(
          createMission({
            name,
            description: "Created by interop/cloudtak",
            keywords: ["interop", "rustak"],
            groups: [NAMES.channel],
          }),
        ),
      );

      if (created.name !== name) {
        throw new Error(`CloudTAK created '${created.name}' rather than '${name}'.`);
      }

      // Every later call routes on the guid, so read it back that way once
      // before anything depends on it.
      const fetched = parseMission(
        "GET /api/marti/missions/{guid}",
        await cloudtak.run(getMission(created.guid)),
      );

      if (fetched.guid !== created.guid) {
        throw new Error(`reading the Data Sync back by guid returned ${fetched.guid}.`);
      }

      state.mission = fetched;

      return [`Data Sync '${created.name}' is ${created.guid}`, "and reads back by guid"];
    },
  },
  {
    name: "marker",
    requires: ["missions"],
    async run({ cloudtak, state }) {
      const guid = mission(state).guid;
      const uid = `interop-marker-${crypto.randomUUID()}`;

      const feature = markerFeature({
        uid,
        callsign: NAMES.markerCallsign,
        longitude: -104.9903,
        latitude: 39.7392,
      });

      const uids = parseSubmitted(await cloudtak.run(submitFeatures(guid, [feature])));

      if (!uids.includes(uid)) {
        throw new Error(
          `CloudTAK confirmed ${JSON.stringify(uids)} rather than the marker it submitted (${uid}). It reads the mission back after uploading the package, so this means the CoT did not land in the Data Sync.`,
        );
      }

      state.markerUid = uid;

      return [`the marker '${NAMES.markerCallsign}' is in the Data Sync as ${uid}`];
    },
  },
  {
    name: "file",
    requires: ["missions", "files"],
    async run({ cloudtak, state }) {
      const guid = mission(state).guid;
      const bytes = Buffer.from(`rustak interop file ${new Date().toISOString()}\n`, "utf8");
      const hash = contentHash(bytes);

      parseAttached(await cloudtak.run(uploadFile(guid, NAMES.file, bytes)));

      // What the mission last said it holds, so a failure names the hashes that
      // are there rather than only the one that is not — a mismatch here is
      // usually the server hashing something other than the bytes it stored.
      let seen: string[] = [];

      const attached = await eventually(
        `the file ${NAMES.file} appearing in the Data Sync as ${hash} (SHA-256 of the bytes uploaded)`,
        async () => {
          const current = parseMission("GET /api/marti/missions/{guid}", await cloudtak.run(getMission(guid)));

          seen = current.contents.map((content) => content.data.hash);

          return seen.includes(hash) ? current : undefined;
        },
      ).catch((error: unknown) => {
        throw new Error(
          `${error instanceof Error ? error.message : String(error)}. The Data Sync holds: ${seen.join(", ") || "(nothing)"}`,
        );
      });

      state.fileHash = hash;

      return [
        `${NAMES.file} (${String(bytes.length)} bytes) uploaded as ${hash}`,
        `and the Data Sync now lists ${String(attached.contents.length)} content(s)`,
      ];
    },
  },
  {
    name: "changes",
    requires: ["missions"],
    async run({ cloudtak, state }) {
      const guid = mission(state).guid;
      const marker = state.markerUid;
      const file = state.fileHash;

      let seen: string[] = [];

      const changes = await eventually("the mission change log recording this run's writes", async () => {
        const current = parseChanges(await cloudtak.run(missionChanges(guid)));

        seen = current.map(
          (change) => `${change.type}(${change.contentUid ?? change.contentResource?.hash ?? "?"})`,
        );

        const sawMarker = marker === undefined || addsContent(current, marker);
        const sawFile = file === undefined || addsResource(current, file);

        return sawMarker && sawFile ? current : undefined;
      }).catch((error: unknown) => {
        throw new Error(
          `${error instanceof Error ? error.message : String(error)}. Looked for ADD_CONTENT of ${marker ?? "(no marker)"} and ${file ?? "(no file)"}; the log holds: ${seen.join(", ") || "(nothing)"}`,
        );
      });

      return [
        `${String(changes.length)} change(s) recorded`,
        `including ADD_CONTENT for ${marker ?? "(no marker)"} and for ${file ?? "(no file)"}`,
      ];
    },
  },
  {
    name: "package",
    requires: ["files"],
    async run({ cloudtak, state }) {
      const name = `${NAMES.package}-${crypto.randomUUID().slice(0, 8)}`;

      const feature = markerFeature({
        uid: `interop-package-${crypto.randomUUID()}`,
        callsign: "Interop Package Marker",
        longitude: -104.9847,
        latitude: 39.7407,
      });

      const content = parseContent(
        "PUT /api/marti/package",
        await cloudtak.run(sharePackage({ name, keywords: ["interop"], features: [feature] })),
      );

      const listed = await eventually(`the package '${name}' appearing in the package list`, async () => {
        const packages = parsePackages(await cloudtak.run(listPackages(name)));

        return packages.find((item) => item.name === name || item.hash === content.Hash);
      });

      state.packageHash = content.Hash;

      return [`shared '${name}' as ${content.Hash}`, `and it is listed as ${listed.uid}`];
    },
  },
];
