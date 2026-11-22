//! Reads a capture file that a receiver is still writing: only the complete
//! records appended since the last read, and the whole file again once it has
//! been replaced or truncated.

use std::io::{self, SeekFrom};
use std::path::Path;

use tokio::io::{AsyncReadExt, AsyncSeekExt};
use uas_cot::remoteid::capture::{self, CaptureError, CapturedFrame};

const SHB: [u8; 4] = [0x0a, 0x0d, 0x0d, 0x0a];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Format {
    Pcap { be: bool },
    Pcapng { be: bool },
    Ridf,
}

/// What one [`CaptureTail::read`] found.
pub struct Appended {
    /// The Remote ID frames of the complete records appended since the last read.
    pub frames: Vec<CapturedFrame>,
    /// The file was replaced or truncated since the last read, so `frames`
    /// start from the top of a new file.
    pub restarted: bool,
}

#[derive(Default)]
pub struct CaptureTail {
    format: Option<Format>,
    /// What each batch of records is parsed behind: the pcap or RIDF file
    /// header, or pcapng's section header and interface description blocks.
    header: Vec<u8>,
    /// Bytes past the last complete record, waiting for the rest of it.
    partial: Vec<u8>,
    /// Where in the file the next read starts.
    offset: u64,
    identity: Option<u64>,
}

impl CaptureTail {
    pub async fn read(&mut self, path: &Path) -> io::Result<Appended> {
        let mut file = tokio::fs::File::open(path).await?;
        let meta = file.metadata().await?;
        let identity = identity(&meta);
        let restarted = self.offset > 0 && (meta.len() < self.offset || identity != self.identity);
        if restarted {
            *self = Self::default();
        }
        self.identity = identity;

        file.seek(SeekFrom::Start(self.offset)).await?;
        let mut bytes = std::mem::take(&mut self.partial);
        self.offset += file.read_to_end(&mut bytes).await? as u64;

        let records = self.complete_records(bytes).map_err(invalid)?;
        let frames = if records.is_empty() {
            Vec::new()
        } else {
            let mut whole = self.header.clone();
            whole.extend_from_slice(&records);
            capture::frames(&whole).map_err(invalid)?
        };
        Ok(Appended { frames, restarted })
    }

    /// Moves header bytes into `self.header`, keeps a trailing incomplete
    /// record in `self.partial`, and returns the complete records between.
    fn complete_records(&mut self, bytes: Vec<u8>) -> Result<Vec<u8>, CaptureError> {
        let mut at = 0;
        let mut format = match self.format {
            Some(format) => format,
            None => {
                let Some((format, header)) = sniff(&bytes)? else {
                    self.partial = bytes;
                    return Ok(Vec::new());
                };
                self.header = bytes[..header].to_vec();
                at = header;
                format
            }
        };

        let mut records = Vec::new();
        loop {
            let rest = &bytes[at..];
            let len = match format {
                Format::Pcap { be } => u32_at(rest, 8, be).map(|n| 16 + n as usize),
                Format::Ridf => rest
                    .get(..2)
                    .map(|n| 2 + usize::from(u16::from_le_bytes([n[0], n[1]]))),
                Format::Pcapng { be } => {
                    let be = if rest.starts_with(&SHB) {
                        match rest.get(8..12) {
                            Some([0x1a, 0x2b, 0x3c, 0x4d]) => true,
                            Some([0x4d, 0x3c, 0x2b, 0x1a]) => false,
                            Some(_) => return Err(CaptureError::UnknownFormat),
                            None => break,
                        }
                    } else {
                        be
                    };
                    match u32_at(rest, 4, be).map(|n| n as usize) {
                        Some(n) if n < 12 => return Err(CaptureError::Truncated { offset: at }),
                        Some(n) if rest.starts_with(&SHB) && n <= rest.len() => {
                            // A new section: parse the old one's packets first.
                            if !records.is_empty() {
                                break;
                            }
                            format = Format::Pcapng { be };
                            self.header = rest[..n].to_vec();
                            at += n;
                            continue;
                        }
                        n => n,
                    }
                }
            };
            let Some(len) = len.filter(|&n| n <= rest.len()) else {
                break;
            };
            let record = &rest[..len];
            match format {
                // Interface descriptions are what later packets refer to.
                Format::Pcapng { be } if u32_at(record, 0, be) == Some(1) => {
                    self.header.extend_from_slice(record);
                }
                _ => records.extend_from_slice(record),
            }
            at += len;
        }
        self.format = Some(format);
        self.partial = bytes[at..].to_vec();
        Ok(records)
    }
}

/// The format and header length of a capture, or `None` when too little of
/// it has been written to tell.
fn sniff(b: &[u8]) -> Result<Option<(Format, usize)>, CaptureError> {
    let Some(magic) = b.get(..4) else {
        return Ok(None);
    };
    let (format, header) = match magic {
        b"RIDF" => (Format::Ridf, 8),
        [0x0a, 0x0d, 0x0d, 0x0a] => (Format::Pcapng { be: false }, 0),
        [0xd4, 0xc3, 0xb2, 0xa1] | [0x4d, 0x3c, 0xb2, 0xa1] => (Format::Pcap { be: false }, 24),
        [0xa1, 0xb2, 0xc3, 0xd4] | [0xa1, 0xb2, 0x3c, 0x4d] => (Format::Pcap { be: true }, 24),
        _ => return Err(CaptureError::UnknownFormat),
    };
    Ok((b.len() >= header).then_some((format, header)))
}

fn u32_at(b: &[u8], at: usize, be: bool) -> Option<u32> {
    let n: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
    Some(if be {
        u32::from_be_bytes(n)
    } else {
        u32::from_le_bytes(n)
    })
}

fn invalid(error: CaptureError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(unix)]
fn identity(meta: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.ino())
}

#[cfg(not(unix))]
fn identity(_: &std::fs::Metadata) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use uas_cot::remoteid::{BasicId, IdType, Message, Transmitter, Transport, UaType};

    fn frames(n: u8) -> Vec<CapturedFrame> {
        (0..n)
            .map(|i| CapturedFrame {
                time_us: 1_763_700_000_000_000 + u64::from(i) * 1000,
                transmitter: Transmitter::Ble([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, i]),
                transport: Transport::Bluetooth4,
                counter: Some(i),
                payload: Message::BasicId(
                    BasicId::new(
                        IdType::SerialNumber,
                        UaType::HelicopterOrMultirotor,
                        "1596F35074529ABCDEFG",
                    )
                    .unwrap(),
                )
                .encode()
                .unwrap()
                .to_vec(),
                beacon: None,
                rssi_dbm: None,
                utc_clock: true,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_capture_being_written_is_read_one_complete_record_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rid.ridf");
        let bytes = capture::to_ridf(&frames(3)).unwrap();
        let mut tail = CaptureTail::default();

        // Cut part-way through the last record, as a buffered writer leaves it.
        std::fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
        let first = tail.read(&path).await.unwrap();
        assert_eq!(first.frames.len(), 2);
        assert!(!first.restarted);

        std::fs::write(&path, &bytes).unwrap();
        let second = tail.read(&path).await.unwrap();
        assert_eq!(second.frames.len(), 1);

        assert!(tail.read(&path).await.unwrap().frames.is_empty());

        // Truncated and rewritten shorter: read from the start again.
        let shorter = capture::to_ridf(&frames(1)).unwrap();
        std::fs::write(&path, &shorter).unwrap();
        let again = tail.read(&path).await.unwrap();
        assert!(again.restarted);
        assert_eq!(again.frames.len(), 1);
    }

    #[tokio::test]
    async fn a_pcap_header_written_in_pieces_is_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rid.pcap");
        let header = capture::pcap_header(105);
        let mut tail = CaptureTail::default();

        std::fs::write(&path, &header[..10]).unwrap();
        assert!(tail.read(&path).await.unwrap().frames.is_empty());
        std::fs::write(&path, &header).unwrap();
        assert!(tail.read(&path).await.unwrap().frames.is_empty());
        let mut with_packet = header.clone();
        capture::pcap_push(&mut with_packet, 1_763_700_000_000_000, &[0u8; 40]);
        std::fs::write(&path, &with_packet[..with_packet.len() - 5]).unwrap();
        assert!(tail.read(&path).await.unwrap().frames.is_empty());
        std::fs::write(&path, &with_packet).unwrap();
        // Not a Remote ID frame, but parsed without a Truncated error.
        assert!(tail.read(&path).await.unwrap().frames.is_empty());
    }
}
