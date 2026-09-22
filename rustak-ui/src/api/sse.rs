//! Server-Sent Events, read off a `fetch` body rather than an `EventSource`.
//!
//! `EventSource` cannot send an `Authorization` header, and this console's
//! session is a bearer token; putting the token in the URL instead would put
//! it in every proxy's access log. So the feed is an ordinary authenticated
//! `fetch` whose body is read as it arrives, and this is the part of the SSE
//! format that reading needs: frames end at a blank line, `event:` names one,
//! `data:` lines are its body, and a line starting with `:` is a comment the
//! server sends so the connection is never silent.
//!
//! The parser takes bytes rather than text because a chunk boundary can fall
//! inside a UTF-8 sequence. A frame boundary cannot, so bytes are only ever
//! decoded a whole frame at a time.

/// One event off the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The `event:` field, or `message` when the frame did not name itself.
    pub event: String,

    /// The `data:` lines, joined the way the format says they are.
    pub data: String,
}

/// Whatever has arrived and is not yet a whole frame.
#[derive(Debug, Default)]
pub struct Parser {
    buffer: Vec<u8>,
}

impl Parser {
    /// Takes the next chunk of the body and answers the frames it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Frame> {
        // `\r\n` is legal and nothing here sends it; dropping the `\r` makes
        // the two spellings one.
        self.buffer
            .extend(chunk.iter().copied().filter(|byte| *byte != b'\r'));

        let mut frames = Vec::new();
        while let Some(end) = self.buffer.windows(2).position(|pair| pair == b"\n\n") {
            let block: Vec<u8> = self.buffer.drain(..end + 2).collect();
            frames.extend(frame(&String::from_utf8_lossy(&block)));
        }

        frames
    }
}

/// One blank-line-terminated block, or [`None`] when it carried no data — a
/// comment, or the `retry:` preamble.
fn frame(block: &str) -> Option<Frame> {
    let mut event = None;
    let mut data = Vec::new();

    for line in block.lines() {
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);

        match field {
            "event" => event = Some(value.to_string()),
            "data" => data.push(value),
            _ => {}
        }
    }

    (!data.is_empty()).then(|| Frame {
        event: event.unwrap_or_else(|| "message".to_string()),
        data: data.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_delivered_when_its_blank_line_arrives_and_not_before() {
        let mut parser = Parser::default();

        assert!(parser.push(b"event: upsert\ndata: {\"uid\"").is_empty());

        assert_eq!(
            parser.push(b":\"A\"}\n\nevent: rem"),
            [Frame {
                event: "upsert".to_string(),
                data: "{\"uid\":\"A\"}".to_string(),
            }]
        );
    }

    #[test]
    fn comments_and_the_retry_preamble_are_not_events() {
        let mut parser = Parser::default();

        assert!(parser.push(b"retry: 5000\n\n: keep-alive\n\n").is_empty());
    }

    #[test]
    fn a_chunk_may_end_in_the_middle_of_a_character() {
        let mut parser = Parser::default();
        let whole = "data: Zoë\n\n".as_bytes();
        let (first, second) = whole.split_at(whole.len() - 4);

        assert!(parser.push(first).is_empty());
        assert_eq!(parser.push(second)[0].data, "Zoë");
    }

    #[test]
    fn several_frames_in_one_chunk_all_arrive_and_crlf_is_the_same_as_lf() {
        let mut parser = Parser::default();
        let frames = parser.push(b"data: one\r\n\r\ndata: two\ndata: three\n\n");

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event, "message");
        assert_eq!(frames[1].data, "two\nthree");
    }
}
