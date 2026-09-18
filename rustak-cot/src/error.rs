//! Error types for the CoT model, the XML parser/writer and the wire codecs.
//!
//! Every error here is a *message-level* failure: the streaming layers drop the
//! offending message and carry on, they never tear a connection down because a
//! peer sent something we could not read.
//!
//! These types are hand-written rather than derived so that `rustak-cot` stays
//! dependency-light (it is the wasm-safe leaf of the workspace).

use std::fmt;

/// A time attribute whose value is not a CoT timestamp.
///
/// Returned by [`crate::CotTime::parse`], which has no attribute name to
/// report; the XML parser upgrades it to [`ParseError::BadTime`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadTime {
    /// The offending value, verbatim.
    pub value: String,
}

impl fmt::Display for BadTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} is not a CoT timestamp", self.value)
    }
}

impl std::error::Error for BadTime {}

/// Why a chunk of CoT XML could not be turned into an [`crate::Event`].
#[derive(Debug)]
#[non_exhaustive]
pub enum ParseError {
    /// The XML itself is malformed (unbalanced tags, bad encoding, ...).
    Xml(quick_xml::Error),
    /// No `<event>` element was found in the input.
    MissingEvent,
    /// The event has no `<point>` with `lat`/`lon`.
    ///
    /// TAK Server drops such messages before they reach any filter, so we
    /// refuse to build an [`crate::Event`] without a point at all.
    MissingPoint,
    /// A numeric attribute did not parse.
    BadNumber {
        /// Attribute name, e.g. `lat`.
        attr: String,
        /// The offending value, verbatim.
        value: String,
    },
    /// A time attribute did not parse.
    BadTime {
        /// Attribute name, one of `time`, `start`, `stale`.
        attr: String,
        /// The offending value, verbatim.
        value: String,
    },
    /// An attribute name, element name or value was not valid UTF-8.
    Utf8,
    /// The message exceeded the caller's size budget.
    TooLarge,
    /// The `<detail>` tree nested deeper than [`crate::xml::MAX_DEPTH`].
    TooDeep,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xml(e) => write!(f, "malformed CoT XML: {e}"),
            Self::MissingEvent => f.write_str("no <event> element in the message"),
            Self::MissingPoint => f.write_str("the event has no <point lat= lon=>"),
            Self::BadNumber { attr, value } => {
                write!(f, "attribute {attr}={value:?} is not a number")
            }
            Self::BadTime { attr, value } => {
                write!(f, "attribute {attr}={value:?} is not a CoT timestamp")
            }
            Self::Utf8 => f.write_str("the message is not valid UTF-8"),
            Self::TooLarge => f.write_str("the message is larger than the configured limit"),
            Self::TooDeep => f.write_str("the <detail> tree is nested too deeply"),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Xml(e) => Some(e),
            _ => None,
        }
    }
}

impl From<quick_xml::Error> for ParseError {
    fn from(value: quick_xml::Error) -> Self {
        Self::Xml(value)
    }
}

impl From<quick_xml::errors::IllFormedError> for ParseError {
    fn from(value: quick_xml::errors::IllFormedError) -> Self {
        Self::Xml(quick_xml::Error::IllFormed(value))
    }
}

/// Why a `TakMessage` could not be turned into an [`crate::Event`].
#[derive(Debug)]
#[non_exhaustive]
pub enum ConvertError {
    /// The message carried only a `takControl`, with no event to convert.
    NoCotEvent,
    /// The `xmlDetail` fragment could not be parsed.
    BadXmlDetail(ParseError),
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCotEvent => f.write_str("the TakMessage carries no cotEvent"),
            Self::BadXmlDetail(e) => write!(f, "the xmlDetail fragment is malformed: {e}"),
        }
    }
}

impl std::error::Error for ConvertError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BadXmlDetail(e) => Some(e),
            Self::NoCotEvent => None,
        }
    }
}

impl From<ParseError> for ConvertError {
    fn from(value: ParseError) -> Self {
        Self::BadXmlDetail(value)
    }
}

/// Why a stream could not be split into messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameError {
    /// A single message exceeded the per-message cap; the buffer was reset.
    Oversized(usize),
    /// A protobuf length prefix was not a valid LEB128 varint.
    BadVarint,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oversized(n) => write!(f, "message of {n} bytes exceeds the frame limit"),
            Self::BadVarint => f.write_str("malformed protobuf length prefix"),
        }
    }
}

impl std::error::Error for FrameError {}

/// The union of everything that can go wrong inside a stream codec.
#[derive(Debug)]
#[non_exhaustive]
pub enum CodecError {
    /// The underlying transport failed.
    Io(std::io::Error),
    /// The stream could not be framed.
    Frame(FrameError),
    /// A framed XML message could not be parsed.
    Parse(ParseError),
    /// A decoded `TakMessage` could not be converted.
    Convert(ConvertError),
    /// A framed protobuf payload could not be decoded.
    Decode(prost::DecodeError),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "stream I/O failed: {e}"),
            Self::Frame(e) => write!(f, "{e}"),
            Self::Parse(e) => write!(f, "{e}"),
            Self::Convert(e) => write!(f, "{e}"),
            Self::Decode(e) => write!(f, "malformed TakMessage: {e}"),
        }
    }
}

impl std::error::Error for CodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Frame(e) => Some(e),
            Self::Parse(e) => Some(e),
            Self::Convert(e) => Some(e),
            Self::Decode(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for CodecError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<FrameError> for CodecError {
    fn from(value: FrameError) -> Self {
        Self::Frame(value)
    }
}

impl From<ParseError> for CodecError {
    fn from(value: ParseError) -> Self {
        Self::Parse(value)
    }
}

impl From<ConvertError> for CodecError {
    fn from(value: ConvertError) -> Self {
        Self::Convert(value)
    }
}

impl From<prost::DecodeError> for CodecError {
    fn from(value: prost::DecodeError) -> Self {
        Self::Decode(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_errors_describe_the_offending_attribute() {
        let err = ParseError::BadNumber {
            attr: "lat".into(),
            value: "north".into(),
        };
        assert_eq!(err.to_string(), r#"attribute lat="north" is not a number"#);
    }

    #[test]
    fn bad_time_upgrades_into_a_codec_error_chain() {
        let err = CodecError::from(ParseError::BadTime {
            attr: "stale".into(),
            value: "soon".into(),
        });
        assert!(err.to_string().contains("stale"));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn frame_errors_are_cheap_to_compare() {
        assert_eq!(FrameError::Oversized(10), FrameError::Oversized(10));
        assert_ne!(FrameError::Oversized(10), FrameError::BadVarint);
    }
}
