/**
 * Filling a scenario's `commotest` script in.
 *
 * A scenario cannot know the port the server bound, the account the runner
 * created or the one-time token it minted, so its script lines carry
 * placeholders and the runner substitutes them just before the container
 * starts. The set is closed: a `{placeholder}` the runner does not know is a
 * mistake in the scenario file, and it is refused while the file is loaded
 * rather than passed through to a parser that accepts nonsense quietly.
 */

/** Every placeholder a scenario may use. */
export const PLACEHOLDERS = [
  /** The address the EUD reaches rustak at — an address, not a name, so a `--network host` container resolves nothing. */
  "host",
  /** `[stream.tls]`, what `estream:`'s `<port>` and `sstream:`'s port are. */
  "stream_port",
  /** `[web.public]`, what ATAK calls `<eport>` and defaults to 8446. */
  "enroll_port",
  /** `[web.marti]`, the mutually authenticated listener mission packages go to. */
  "marti_port",
  /** The account this EUD enrols as. */
  "username",
  /** Its one-time enrolment token, which ATAK sends as the Basic *password*. */
  "token",
  /** The truststore inside the container: the PKCS#12 holding rustak's CA. */
  "truststore",
  /** The mounted output directory inside the container, always `/work`. */
  "work",
  /** This EUD's CoT uid. */
  "uid",
  /** Its callsign. */
  "callsign",
] as const;

/** The name of a placeholder. */
export type Placeholder = (typeof PLACEHOLDERS)[number];

/** What the runner substitutes. */
export type Substitutions = Readonly<Record<Placeholder, string>>;

/** Substitutes every `{placeholder}` in one string. */
export function render(text: string, values: Substitutions): string {
  return text.replace(/\{([a-z_]+)\}/g, (whole, name: string) => {
    if (!(PLACEHOLDERS as readonly string[]).includes(name)) {
      throw new Error(`'${whole}' is not a placeholder the runner fills.`);
    }

    return values[name as Placeholder];
  });
}

/**
 * The whole argv one container is given.
 *
 * `commotest <uid> <callsign> <output-dir> { <wait-seconds> <command> } …`, with
 * the output directory always `/work` — the image's `WORKDIR`, and what the
 * runner mounts this EUD's scratch directory at.
 */
export function renderArgv(
  eud: { readonly uid: string; readonly callsign: string; readonly script: readonly string[] },
  values: Substitutions,
): string[] {
  return [
    render(eud.uid, values),
    render(eud.callsign, values),
    values.work,
    ...eud.script.map((entry) => render(entry, values)),
  ];
}
