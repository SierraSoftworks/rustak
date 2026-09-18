//! CoT XML: the wire form every TAK client speaks.
//!
//! The writer emits exactly what TAK Server does, because clients are strict
//! about it in ways the XML specification is not:
//!
//! * a declaration followed by a single `\n`, then the event, with **no
//!   trailing newline** — messages sit head to tail on the stream;
//! * `<event>` is **never self-closing**, because CloudTAK's framing regex
//!   only matches an open/close pair;
//! * `<detail>` is omitted entirely when it holds nothing;
//! * no control characters reach the wire: CloudTAK strips `U+000B`–`U+001F`
//!   and `U+007F`–`U+009F` before parsing, so emitting them would silently
//!   corrupt a message.
//!
//! The reader is correspondingly forgiving: a byte-order mark, an XML
//! declaration, processing instructions, leading junk and trailing bytes after
//! `</event>` are all ignored, single- and double-quoted attributes are
//! equivalent, and an unknown entity is kept verbatim rather than failing the
//! message.

mod entity;
mod parse;
mod write;

pub use parse::{parse, parse_fragment, parse_str};
pub use write::{format_f64, write, write_fragment, write_into};

/// The declaration every outbound message starts with.
pub const DECLARATION: &str = r#"<?xml version="1.0" encoding="UTF-8"?>"#;

/// How deeply `<detail>` may nest before a message is rejected.
///
/// Deep nesting is the classic way to blow a recursive parser's stack; no
/// legitimate CoT comes close to this.
pub const MAX_DEPTH: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declaration_is_the_exact_literal_tak_server_emits() {
        assert_eq!(DECLARATION, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
        assert!(
            !DECLARATION.ends_with('\n'),
            "the newline is added by the writer"
        );
    }

    #[test]
    fn the_depth_cap_leaves_room_for_any_real_message() {
        assert_eq!(MAX_DEPTH, 64);
    }
}
