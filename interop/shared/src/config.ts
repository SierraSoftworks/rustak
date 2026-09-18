/**
 * The little bit of TOML writing the suites need.
 *
 * A suite's configuration is assembled as `{ "<table>": { key: value } }` and
 * rendered once, rather than concatenated as text, because TOML forbids
 * defining a table twice: a scenario that wanted `[auth] anon_group_default =
 * false` on top of the launcher's `[auth]` block could not express it by
 * appending, and rustak would refuse the file rather than take the override
 * (`#[serde(deny_unknown_fields)]` and `toml`'s duplicate-key error).
 *
 * Only what a generated test configuration contains is supported — strings,
 * integers, booleans and arrays of those. Anything else throws while the file
 * is being built, which is a better place to find out than the server's parse
 * error.
 */

/** A value a generated configuration may carry. */
export type ConfigValue = string | number | boolean | readonly (string | number | boolean)[];

/** One configuration, as `{ "<dotted table name>": { key: value } }`. */
export type ConfigTables = Readonly<Record<string, Readonly<Record<string, ConfigValue>>>>;

/**
 * Merges `overrides` over `base`, table by table and key by key.
 *
 * A table only present in one of them is kept whole; a key present in both
 * takes the override's value. Tables are emitted in `base` order, with any new
 * ones appended, so the generated file reads the same way every time.
 */
export function mergeConfig(base: ConfigTables, overrides: ConfigTables): ConfigTables {
  const merged: Record<string, Record<string, ConfigValue>> = {};

  for (const table of [...Object.keys(base), ...Object.keys(overrides)]) {
    if (merged[table]) continue;

    merged[table] = { ...(base[table] ?? {}), ...(overrides[table] ?? {}) };
  }

  return merged;
}

/** Renders the tables as a configuration file rustak will parse. */
export function renderConfig(tables: ConfigTables): string {
  const lines: string[] = [];

  for (const [table, entries] of Object.entries(tables)) {
    lines.push(`[${table}]`);

    for (const [key, value] of Object.entries(entries)) {
      lines.push(`${key} = ${render(key, value)}`);
    }

    lines.push("");
  }

  return lines.join("\n");
}

/** One value, as TOML. */
function render(key: string, value: ConfigValue): string {
  if (Array.isArray(value)) {
    return `[${value.map((item) => render(key, item as ConfigValue)).join(", ")}]`;
  }

  switch (typeof value) {
    case "string":
      return JSON.stringify(value);
    case "boolean":
      return value ? "true" : "false";
    case "number":
      if (!Number.isFinite(value)) {
        throw new Error(`the value of '${key}' is ${String(value)}, which is not a TOML number.`);
      }

      return String(value);
    default:
      throw new Error(`the value of '${key}' is a ${typeof value}, which this writer cannot emit.`);
  }
}
