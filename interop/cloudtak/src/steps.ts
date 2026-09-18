/**
 * What a run asserts, in the order an operator would do it.
 *
 * Every step drives **CloudTAK's** REST API and asserts on what CloudTAK says
 * came back from rustak — never on rustak directly. That is the whole value of
 * this suite over `interop/node-tak`: node-tak proves the wire shapes are right
 * for the library CloudTAK uses; this proves CloudTAK itself, with its own
 * certificate handling, its own mission-package building and its own database,
 * gets the answers it needs from them.
 *
 * The steps themselves live next door — `steps-setup.ts` for configuration and
 * sign-in, `steps-datasync.ts` for the mission and package work — and this is
 * the order they run in.
 */

import { DATASYNC_STEPS } from "./steps-datasync.js";
import { SETUP_STEPS } from "./steps-setup.js";

export type { RunState, Step, StepContext } from "./step-kit.js";
export { contentHash } from "./step-kit.js";

/** Every step, in the order a run performs them. */
export const STEPS = [...SETUP_STEPS, ...DATASYNC_STEPS] as const;
