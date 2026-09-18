//! Resolving XML entity and character references.
//!
//! quick-xml hands back general references as their own events and leaves
//! attribute values undecoded, so rustak resolves them itself. That is
//! deliberate: an entity we do not recognise is kept exactly as the sender
//! wrote it rather than failing the whole message, which is what a relay
//! should do with something it does not understand.

/// Resolves an attribute value: character references expand, literal tabs and
/// newlines normalise to spaces per the XML specification, and an entity we do
/// not recognise is left exactly as the sender wrote it.
pub(super) fn unescape_attr(raw: &str) -> String {
    if !raw.contains(['&', '\t', '\r', '\n']) {
        return raw.to_owned();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(index) = rest.find(['&', '\t', '\r', '\n']) {
        out.push_str(&rest[..index]);
        rest = &rest[index..];
        let mut chars = rest.chars();
        let first = chars.next().unwrap_or('&');
        if first != '&' {
            out.push(' ');
            rest = chars.as_str();
            continue;
        }
        match reference_end(rest) {
            Some(end) => {
                match entity_char(&rest[1..end]) {
                    Some(resolved) => out.push(resolved),
                    None => out.push_str(&rest[..=end]),
                }
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = chars.as_str();
            }
        }
    }
    out.push_str(rest);
    out
}

/// The offset of the `;` that closes a reference starting at `rest[0]`.
///
/// Returns [`None`] when the `&` is dangling — a character that cannot appear
/// in a reference name ends the scan — so that a stray ampersand does not
/// swallow a well-formed entity later in the same value.
fn reference_end(rest: &str) -> Option<usize> {
    for (index, character) in rest.char_indices().skip(1) {
        match character {
            ';' => return Some(index),
            'a'..='z' | 'A'..='Z' | '0'..='9' | '#' | '_' | '.' | '-' | ':' => {}
            _ => return None,
        }
    }
    None
}

/// The character an entity or character reference stands for.
pub(super) fn entity_char(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("amp", Some('&'))]
    #[case("lt", Some('<'))]
    #[case("gt", Some('>'))]
    #[case("quot", Some('"'))]
    #[case("apos", Some('\''))]
    #[case("#66", Some('B'))]
    #[case("#x44", Some('D'))]
    #[case("#X44", Some('D'))]
    #[case("#128752", Some('\u{1f6f0}'))]
    #[case("nbsp", None)]
    #[case("", None)]
    #[case("#", None)]
    #[case("#xZZ", None)]
    #[case("#999999999", None)]
    fn references_resolve_or_are_left_to_the_caller(
        #[case] name: &str,
        #[case] expected: Option<char>,
    ) {
        assert_eq!(entity_char(name), expected, "{name}");
    }

    #[test]
    fn attribute_values_resolve_known_references_only() {
        assert_eq!(unescape_attr("plain"), "plain");
        assert_eq!(unescape_attr("a &amp; b"), "a & b");
        assert_eq!(
            unescape_attr("&lt;x&gt; &quot;y&quot; &apos;z&apos;"),
            r#"<x> "y" 'z'"#
        );
        assert_eq!(unescape_attr("&#66;&#x44;"), "BD");
        assert_eq!(unescape_attr("&unknown; &#xZZ;"), "&unknown; &#xZZ;");
    }

    #[test]
    fn a_dangling_ampersand_survives_verbatim() {
        assert_eq!(unescape_attr("a & b"), "a & b");
        assert_eq!(unescape_attr("trailing &"), "trailing &");
        assert_eq!(unescape_attr("&"), "&");
        assert_eq!(unescape_attr("&;"), "&;");
    }

    #[test]
    fn a_dangling_ampersand_does_not_swallow_a_later_entity() {
        assert_eq!(unescape_attr("a & b &amp; c"), "a & b & c");
        assert_eq!(unescape_attr("Q&A &lt;end&gt;"), "Q&A <end>");
        assert_eq!(unescape_attr("&<&amp;"), "&<&");
    }

    #[test]
    fn literal_whitespace_normalises_the_way_xml_requires() {
        assert_eq!(unescape_attr("a\tb\nc\rd"), "a b c d");
        assert_eq!(unescape_attr("\t&amp;\n"), " & ");
    }
}
