/**
 * What every interop suite calls the installation it starts.
 *
 * `[server] name` is free text an operator types into a configuration file, and
 * for three days in production this installation's was `SierraSoftworks TAK`.
 * The flow tag rustak stamps on every relayed message is an XML attribute
 * *named* after it, so every message it relayed went out as
 * `<_flow-tags_ TAK-Server-SierraSoftworks TAK="…">` — which CloudTAK's sax
 * parser reads as an attribute with no value. It dropped every message off its
 * socket and answered `500` for any mission's `/cot` document. No suite caught
 * it, because every harness, fixture and interop configuration called the
 * server `rustak`, `rustak-1` or `rustak-interop`: one-word names that happen
 * to be legal wherever they were interpolated.
 *
 * So the ordinary name an interop run uses is one a careless implementation
 * breaks on. Each character is legal in a display name and illegal or special
 * somewhere it might be written without escaping:
 *
 * | character | where it bites |
 * | --------- | -------------- |
 * | space | ends an XML name; ends an unquoted HTTP header token |
 * | `&` | must be `&amp;` in XML text and attribute values; separates query parameters |
 * | `(` `)` | illegal in an XML name; `separators` in RFC 9110's token production |
 * | `.` | legal in an XML name but not at its start, and a label separator in DNS |
 * | `ä` | legal in an XML name, illegal in a header value, a DNS label and an ASCII filename |
 *
 * This is the same string as `rustak-server`'s `config::TEST_SERVER_NAME` and
 * the one `e2e/scripts/start-server.mjs` writes; changing one means changing
 * all three.
 */
export const HOSTILE_SERVER_NAME = "Rustak Test & Co. (näme)";

/**
 * The hostile name, with a suffix that says which suite or scenario it is.
 *
 * The suffix only exists so that a person reading a log with several servers in
 * it can tell them apart; it is appended inside the parentheses so that the
 * punctuation the name is here for stays where it is.
 */
export function hostileServerName(suffix?: string): string {
  return suffix === undefined ? HOSTILE_SERVER_NAME : `Rustak Test & Co. (näme: ${suffix})`;
}
