/**
 * What a step is, and the three things every one of them needs.
 *
 * Split out of `src/steps.ts` so the two halves of the run — getting CloudTAK
 * configured and signed in, and then exercising Data Sync — can each be read in
 * one sitting without either importing the other.
 */

import crypto from "node:crypto";

import type { Group, Mission } from "./api.js";
import type { CloudTak } from "./client.js";
import type { Enrolled } from "./enroll.js";
import type { SurfaceName } from "./surfaces.js";

/** What the steps accumulate as they go. */
export interface RunState {
  groups?: Group[];
  mission?: Mission;
  markerUid?: string;
  fileHash?: string;
  packageHash?: string;
}

/** Everything a step is given. */
export interface StepContext {
  readonly cloudtak: CloudTak;
  readonly operator: { readonly username: string; readonly password: string };
  readonly enrolled: Enrolled;
  readonly state: RunState;
}

/** One assertion in the run: it throws to fail, and returns notes to pass. */
export interface Step {
  readonly name: string;
  readonly requires: readonly SurfaceName[];
  run(context: StepContext): Promise<string[]>;
}

/** The content hash TAK Server names a stored file by. */
export function contentHash(bytes: Buffer): string {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

/** Retries a read that is allowed to be a moment behind the write. */
export async function eventually<T>(
  what: string,
  attempt: () => Promise<T | undefined>,
  attempts = 10,
  intervalMs = 1_000,
): Promise<T> {
  let last: unknown;

  for (let index = 0; index < attempts; index += 1) {
    try {
      const value = await attempt();

      if (value !== undefined) return value;
    } catch (error) {
      last = error;
    }

    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }

  throw new Error(`${what} never became true${last === undefined ? "" : `: ${String(last)}`}`);
}

/** The mission this run made, or the reason there is nothing to work on. */
export function mission(state: RunState): Mission {
  if (state.mission === undefined) throw new Error("no Data Sync was created, so there is nothing to act on.");

  return state.mission;
}
