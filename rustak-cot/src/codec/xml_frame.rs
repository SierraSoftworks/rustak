//! Splitting a byte stream into `<event>…</event>` messages.
//!
//! TAK's XML stream has no length prefix and no delimiter: messages sit head
//! to tail, an XML declaration may or may not precede each one, and the only
//! reliable landmark is the literal token `</event>`. Every implementation on
//! the wire — TAK Server, CloudTAK and ATAK — scans for it, so we do too.
//!
//! Two behaviours look like bugs and are not:
//!
//! * **bytes before the first `<event` are discarded silently.** That is how a
//!   stray `<auth>` document, a byte-order mark, an XML declaration or a
//!   `\r\n` a peer inserted between messages is tolerated; and
//! * **a message past the size cap is consumed, then reported.** Dropping it
//!   without consuming it would wedge the connection on the same bytes
//!   forever.
//!
//! `<events>` (the plural wrapper of the Marti HTTP surface) is deliberately
//! *not* matched: a start token only counts when the next byte ends the
//! element name.

use bytes::{Buf, Bytes, BytesMut};

use crate::error::FrameError;

/// The token that opens a message, minus the byte that ends the name.
const START: &[u8] = b"<event";
/// The token that closes a message.
const END: &[u8] = b"</event>";

/// Bytes that may follow `<event` in a real start tag.
const fn ends_name(byte: u8) -> bool {
    matches!(byte, b' ' | b'>' | b'\t' | b'\n' | b'\r')
}

/// Where the next message might start.
enum Start {
    /// A confirmed `<event` start tag at this offset.
    At(usize),
    /// A prefix of the start token runs to the end of the buffer: everything
    /// before this offset is junk, but the rest may yet become a start tag.
    Maybe(usize),
    /// Nothing in the buffer can become a start tag.
    None,
}

/// Finds the first `<event` whose name is properly terminated.
fn find_start(hay: &[u8]) -> Start {
    let mut from = 0;
    while let Some(offset) = memchr::memmem::find(&hay[from..], START) {
        let at = from + offset;
        match hay.get(at + START.len()) {
            Some(&byte) if ends_name(byte) => return Start::At(at),
            // The name continues (`<events>`): keep looking after this one.
            Some(_) => from = at + 1,
            // The token is at the very end; we cannot tell yet.
            None => return Start::Maybe(at),
        }
    }

    // No whole token, but the buffer may end mid-token. Keep the longest
    // suffix that is a prefix of the start token, which is also the earliest.
    for keep in (1..=START.len().min(hay.len())).rev() {
        if hay[hay.len() - keep..] == START[..keep] {
            return Start::Maybe(hay.len() - keep);
        }
    }
    Start::None
}

/// Incremental `</event>` scanner over a growing buffer.
///
/// One scanner belongs to one connection: it remembers how far it has already
/// searched so that a message arriving in many small reads still costs a
/// single pass overall, and so that a `</event>` split across two reads is
/// still found.
#[derive(Clone, Copy, Debug, Default)]
pub struct XmlScanner {
    /// Bytes at the front of the buffer already searched for [`END`]. Non-zero
    /// only while the buffer is known to be aligned on a start tag.
    scanned: usize,
}

impl XmlScanner {
    /// A scanner positioned at the start of a stream.
    #[must_use]
    pub const fn new() -> Self {
        Self { scanned: 0 }
    }

    /// Takes the next complete message from `buf`, if there is one.
    ///
    /// Leading junk is consumed either way. `max` caps a single message.
    ///
    /// # Errors
    ///
    /// [`FrameError::Oversized`] when a message, or the unterminated buffer
    /// that is accumulating towards one, passes `max`. The offending bytes are
    /// consumed before the error is returned, so the next call starts clean.
    pub fn split(&mut self, buf: &mut BytesMut, max: usize) -> Result<Option<Bytes>, FrameError> {
        if self.scanned == 0 {
            match find_start(&buf[..]) {
                Start::At(0) => {}
                Start::At(at) => buf.advance(at),
                Start::Maybe(at) => {
                    buf.advance(at);
                    return Ok(None);
                }
                Start::None => {
                    buf.clear();
                    return Ok(None);
                }
            }
        }

        // Resume far enough back that an `</event>` straddling the boundary
        // between the previous read and this one is still seen whole.
        let from = self.scanned.saturating_sub(END.len() - 1);
        if let Some(offset) = memchr::memmem::find(&buf[from..], END) {
            let end = from + offset + END.len();
            self.scanned = 0;
            if end > max {
                buf.advance(end);
                return Err(FrameError::Oversized(end));
            }
            return Ok(Some(buf.split_to(end).freeze()));
        }

        self.scanned = buf.len();
        if buf.len() > max {
            let seen = buf.len();
            buf.clear();
            self.scanned = 0;
            return Err(FrameError::Oversized(seen));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 8 * 1024 * 1024;

    fn event(uid: &str) -> String {
        format!(
            r#"<event version="2.0" uid="{uid}" type="a-f-G-U-C"><point lat="1" lon="2"/></event>"#
        )
    }

    /// Feeds `input` one byte at a time and collects every message that falls
    /// out — the worst case a TCP stack can produce.
    fn drip(input: &[u8]) -> Vec<String> {
        let mut scanner = XmlScanner::new();
        let mut buffer = BytesMut::new();
        let mut out = Vec::new();
        for byte in input {
            buffer.extend_from_slice(&[*byte]);
            while let Ok(Some(frame)) = scanner.split(&mut buffer, CAP) {
                out.push(String::from_utf8(frame.to_vec()).expect("ascii fixture"));
            }
        }
        out
    }

    #[test]
    fn a_whole_message_arriving_at_once_comes_straight_back() {
        let mut scanner = XmlScanner::new();
        let mut buffer = BytesMut::from(event("A").as_bytes());
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(event("A").as_bytes())
        );
        assert!(buffer.is_empty());
        assert_eq!(scanner.split(&mut buffer, CAP).unwrap(), None);
    }

    #[test]
    fn byte_by_byte_feeding_finds_every_message() {
        let stream = format!("{}{}{}", event("A"), event("B"), event("C"));
        assert_eq!(
            drip(stream.as_bytes()),
            vec![event("A"), event("B"), event("C")]
        );
    }

    #[test]
    fn two_messages_in_one_chunk_are_both_returned() {
        let mut scanner = XmlScanner::new();
        let mut buffer = BytesMut::from(format!("{}{}", event("A"), event("B")).as_bytes());
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(event("A").as_bytes())
        );
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(event("B").as_bytes())
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn garbage_before_the_first_start_tag_is_discarded() {
        let stream = format!(
            "<auth><cot username='u' password='p'/></auth>{}",
            event("A")
        );
        let mut scanner = XmlScanner::new();
        let mut buffer = BytesMut::from(stream.as_bytes());
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(event("A").as_bytes())
        );
    }

    #[test]
    fn a_declaration_between_messages_is_discarded() {
        let declaration = crate::xml::DECLARATION;
        let stream = format!("{declaration}\n{}{declaration}\n{}", event("A"), event("B"));
        assert_eq!(drip(stream.as_bytes()), vec![event("A"), event("B")]);
    }

    #[test]
    fn carriage_returns_between_messages_are_tolerated() {
        let stream = format!("\r\n{}\r\n{}\r\n", event("A"), event("B"));
        assert_eq!(drip(stream.as_bytes()), vec![event("A"), event("B")]);
    }

    #[test]
    fn a_newline_may_end_the_start_tag_name() {
        let stream = "<event\n version=\"2.0\" uid=\"A\"><point lat=\"1\" lon=\"2\"/></event>";
        assert_eq!(drip(stream.as_bytes()), vec![stream.to_owned()]);
    }

    #[test]
    fn the_plural_events_wrapper_is_not_a_start_tag() {
        let stream = format!("<events>{}</events>", event("A"));
        // The wrapper is junk; only the inner message is framed, and the
        // trailing `</events>` is discarded as junk on the next pass.
        assert_eq!(drip(stream.as_bytes()), vec![event("A")]);
    }

    #[test]
    fn a_closing_token_split_across_reads_is_still_found() {
        let whole = event("A");
        let (head, tail) = whole.split_at(whole.len() - 4);
        let mut scanner = XmlScanner::new();
        let mut buffer = BytesMut::from(head.as_bytes());
        assert_eq!(scanner.split(&mut buffer, CAP).unwrap(), None);
        buffer.extend_from_slice(tail.as_bytes());
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(whole.as_bytes())
        );
    }

    #[test]
    fn a_start_token_split_across_reads_is_not_thrown_away() {
        let whole = event("A");
        let mut scanner = XmlScanner::new();
        let mut buffer = BytesMut::from(&b"junk<ev"[..]);
        assert_eq!(scanner.split(&mut buffer, CAP).unwrap(), None);
        assert_eq!(&buffer[..], &b"<ev"[..], "the partial token must survive");
        buffer.extend_from_slice(&whole.as_bytes()[3..]);
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(whole.as_bytes())
        );
    }

    #[test]
    fn an_oversized_message_is_reported_and_consumed() {
        let padding = "x".repeat(200);
        let big = format!(r#"<event uid="A" note="{padding}"><point lat="1" lon="2"/></event>"#);
        let mut scanner = XmlScanner::new();
        let mut buffer = BytesMut::from(format!("{big}{}", event("B")).as_bytes());
        // A cap that the padded message breaks and an ordinary one does not.
        let cap = 128;
        assert!(big.len() > cap && event("B").len() <= cap);

        assert_eq!(
            scanner.split(&mut buffer, cap),
            Err(FrameError::Oversized(big.len()))
        );
        // The stream resynchronises on the very next message.
        assert_eq!(
            scanner.split(&mut buffer, cap).unwrap().as_deref(),
            Some(event("B").as_bytes())
        );
    }

    #[test]
    fn an_unterminated_buffer_past_the_cap_is_dropped() {
        let mut scanner = XmlScanner::new();
        let mut buffer = BytesMut::from(format!("<event uid=\"A\" {}", "x".repeat(200)).as_bytes());
        assert!(matches!(
            scanner.split(&mut buffer, 64),
            Err(FrameError::Oversized(_))
        ));
        assert!(buffer.is_empty(), "the buffer is reset, not left to grow");
        assert_eq!(scanner.split(&mut buffer, 64).unwrap(), None);
    }

    #[test]
    fn a_closing_token_with_no_opening_one_is_discarded() {
        let mut scanner = XmlScanner::new();
        let mut buffer = BytesMut::from(&b"</event></event>"[..]);
        assert_eq!(scanner.split(&mut buffer, CAP).unwrap(), None);
        assert!(buffer.is_empty());
    }

    #[test]
    fn junk_with_no_start_token_never_accumulates() {
        let mut scanner = XmlScanner::new();
        let mut buffer = BytesMut::new();
        for _ in 0..100 {
            buffer.extend_from_slice(&[0xFF; 64]);
            assert_eq!(scanner.split(&mut buffer, CAP).unwrap(), None);
            assert!(buffer.is_empty());
        }
    }
}
