/**
 * Getting CloudTAK pointed at rustak and signed in.
 *
 * These three steps are the operator's first five minutes, and between them
 * they exercise every Tier 1 surface: `PATCH /api/server` makes CloudTAK call
 * `/files/api/config` over mTLS, run the `/oauth/token` password grant and
 * enrol a certificate through `/Marti/api/tls/*` before it will save anything
 * at all (`compat/cloudtak.md` §2); signing in proves the JWT is one CloudTAK
 * can read and the certificate one the Marti listener accepts; and the channel
 * round-trip is `/Marti/api/groups/*` through node-tak's whole-row update.
 */

import {
  configureServer,
  listGroups,
  login,
  parseGroups,
  parseLogin,
  parseServer,
  toggled,
  updateGroups,
} from "./api.js";
import { CONTAINER_URLS, NAMES } from "./settings.js";
import type { Step } from "./step-kit.js";

export const SETUP_STEPS: readonly Step[] = [
  {
    name: "configure-server",
    requires: ["filesConfig", "oauthToken", "tlsConfig"],
    async run({ cloudtak, operator, enrolled }) {
      const answer = await cloudtak.run(
        configureServer({
          name: "rustak (interop)",
          url: CONTAINER_URLS.stream,
          api: CONTAINER_URLS.api,
          webtak: CONTAINER_URLS.webtak,
          username: operator.username,
          password: operator.password,
          cert: enrolled.cert,
          key: enrolled.key,
        }),
      );

      const server = parseServer(answer);

      if (server.api !== CONTAINER_URLS.api || server.webtak !== CONTAINER_URLS.webtak) {
        throw new Error(`CloudTAK stored ${server.api} / ${server.webtak}, not the three URLs it was given.`);
      }

      if (!server.auth || server.certificate === undefined) {
        throw new Error("CloudTAK accepted the configuration without recording the admin certificate.");
      }

      return [
        `CloudTAK ${server.version} is configured against ${server.api}`,
        `the admin certificate it kept is ${server.certificate.subject}`,
        "which means /files/api/config answered with an integer uploadSizeLimit over mTLS",
      ];
    },
  },
  {
    name: "login",
    requires: ["oauthToken"],
    async run({ cloudtak, operator }) {
      const session = parseLogin(await cloudtak.run(login(operator.username, operator.password)));

      if (session.email !== operator.username) {
        throw new Error(
          `CloudTAK signed in as '${session.email}' rather than '${operator.username}' — the JWT's 'sub' claim is what it takes for the account name.`,
        );
      }

      cloudtak.authenticate(session.token);

      // GET /api/login is where CloudTAK probes the user's own certificate
      // against the mutually authenticated listener, so it is the cheapest
      // proof that the certificate it enrolled during the sign-in works.
      await cloudtak.run({ method: "GET", path: "/api/login" });

      return [
        `signed in as ${session.email} with access '${session.access}'`,
        "and CloudTAK's certificate probe against the Marti listener passed",
      ];
    },
  },
  {
    name: "channels",
    requires: ["groups"],
    async run({ cloudtak, state }) {
      const listed = parseGroups("GET /api/marti/group", await cloudtak.run(listGroups()));

      if (listed.length === 0) throw new Error("CloudTAK sees no channels at all for the signed-in user.");

      const target = listed.find((group) => group.name === NAMES.channel) ?? listed[0];
      const wanted = !target.active;

      const after = parseGroups(
        "PUT /api/marti/group",
        await cloudtak.run(updateGroups(toggled(listed, target.name))),
      );

      const updated = after.find((group) => group.name === target.name);

      if (updated === undefined || updated.active !== wanted) {
        throw new Error(
          `toggling '${target.name}' to ${String(wanted)} was not reflected in the list that came back (${String(updated?.active)}).`,
        );
      }

      // Put it back, so the rest of the run sees the channels it was granted.
      state.groups = parseGroups(
        "PUT /api/marti/group",
        await cloudtak.run(updateGroups(toggled(after, target.name))),
      );

      return [
        `${String(listed.length)} channel(s): ${listed.map((group) => group.name).join(", ")}`,
        `'${target.name}' toggled to ${String(wanted)} and back`,
      ];
    },
  },
];
