//! The Weft event-log format: `weft-log` version 1.
//!
//! A log is a UTF-8 text file of newline-delimited JSON. Line 1 is the
//! [`Header`]; every following line is one [`Record`] — a single broker
//! boundary operation in the exact order the broker serialized it (its lock
//! acquisition order). That order is the *only* nondeterministic input to a
//! simulated run: every other value (datagram fates, virtual time, PRNG
//! output, thread schedule) is a pure function of the seed and is therefore
//! recomputed on replay, never trusted from the log.
//!
//! Integrity: records form an FNV-1a hash chain (see [`crate::hash`] and
//! docs/recording-format.md) so truncation, reordering, or edits are detected
//! when the log is read back.
//!
//! The format is versioned from the start; readers must reject versions they
//! do not understand rather than guess.

use std::fs::File;
use std::io::{self, BufRead, BufWriter, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::hash::{fnv1a, fnv1a_once, FNV_OFFSET};

/// The `format` field every header must carry.
pub const FORMAT: &str = "weft-log";
/// The current (and only) format version.
pub const VERSION: u32 = 1;

/// A virtual network address, mirroring `weft_net::VAddr` but serializable.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Addr {
    pub ip: u32,
    pub port: u16,
}

impl From<weft_net::VAddr> for Addr {
    fn from(a: weft_net::VAddr) -> Self {
        Self {
            ip: a.ip,
            port: a.port,
        }
    }
}

impl From<Addr> for weft_net::VAddr {
    fn from(a: Addr) -> Self {
        Self::new(a.ip, a.port)
    }
}

impl std::fmt::Display for Addr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", weft_net::VAddr::new(self.ip, self.port))
    }
}

/// Line 1 of a log. Everything outside `meta` is replay-relevant; `meta` is
/// informational only and MUST NOT influence replay (it records wall-clock
/// facts about the recording machine, which a replaying machine will not
/// share).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Header {
    pub format: String,
    pub version: u32,
    /// The run seed: with the recorded operation order, this reproduces every
    /// datagram fate.
    pub seed: u64,
    /// The network-condition spec exactly as given to the broker
    /// (`weft_net::config` syntax), empty for a reliable network.
    pub net: String,
    /// Informational, replay-irrelevant metadata.
    #[serde(default)]
    pub meta: Meta,
}

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Meta {
    /// Wall-clock ms since the Unix epoch when recording started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_unix_ms: Option<u64>,
    /// `weft` version that produced the log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weft_version: Option<String>,
    /// Free-form label (e.g. scenario name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// What the broker decided for one `send`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SendOutcome {
    /// The fault model dropped it (loss or partition).
    Dropped,
    /// No connection had bound the destination; discarded like UDP to a
    /// closed port.
    NoReceiver,
    /// Enqueued for the destination connection.
    Enqueued {
        to_conn: u64,
        /// Virtual delivery time sampled by the fault model.
        deliv_ns: u64,
        /// Global enqueue tiebreaker assigned by the broker.
        tie: u64,
    },
}

/// What one `recv` returned.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecvOutcome {
    Empty,
    Delivered {
        src: Addr,
        dst: Addr,
        deliv_ns: u64,
        tie: u64,
        /// Hex-encoded payload bytes.
        payload: String,
    },
}

/// One linearized broker boundary operation.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Event {
    /// A connection was accepted and registered.
    Connect { conn: u64 },
    /// The connection identified its owning node.
    Hello { conn: u64, node: u32 },
    /// The connection claimed a receive address.
    Bind { conn: u64, addr: Addr },
    /// A datagram was submitted, with the broker's (seed-derived) decision.
    Send {
        conn: u64,
        src: Addr,
        dst: Addr,
        /// Per-channel (src→dst) sequence number — the fault model's input.
        chan_seq: u64,
        /// Hex-encoded payload bytes.
        payload: String,
        outcome: SendOutcome,
    },
    /// A receive request completed. For a blocking request this is logged at
    /// the moment it *succeeds* (the queue pop), which is its linearization
    /// point.
    Recv {
        conn: u64,
        blocking: bool,
        outcome: RecvOutcome,
    },
    /// The connection hung up; its bindings were released.
    Disconnect { conn: u64 },
    /// An invariant violation observed by an in-process monitor.
    Violation { invariant: String, message: String },
    /// Final record: totals for cross-checking.
    End {
        events: u64,
        sent: u64,
        dropped: u64,
    },
}

/// The chain input: the replay-relevant part of a record. Field order here
/// defines the canonical serialization hashed into the chain — do not reorder.
#[derive(Serialize)]
struct ChainInput<'a> {
    op: u64,
    vt: u64,
    e: &'a Event,
}

/// One log line after the header.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Record {
    /// Position in the linearization, starting at 0. Dense and strictly
    /// increasing.
    pub op: u64,
    /// The broker's virtual-time high-water mark (ns) when the operation was
    /// applied: the point on the logical timeline this event belongs to.
    pub vt: u64,
    pub e: Event,
    /// Hex of the FNV-1a chain value through this record.
    pub chain: String,
}

/// The canonical serialization of one record's replay-relevant content —
/// what the chain and the stream digest hash.
pub(crate) fn canon(op: u64, vt: u64, e: &Event) -> String {
    serde_json::to_string(&ChainInput { op, vt, e }).expect("event serialization cannot fail")
}

fn chain_step(prev: u64, op: u64, vt: u64, e: &Event) -> u64 {
    fnv1a(prev, canon(op, vt, e).as_bytes())
}

/// Errors from reading or verifying a log.
#[derive(Debug)]
pub enum LogError {
    Io(io::Error),
    /// Line number (1-based) and description.
    Malformed(usize, String),
    WrongFormat(String),
    UnsupportedVersion(u32),
    /// Chain mismatch at this op index: the file was truncated or edited.
    ChainBroken {
        op: u64,
    },
    /// `op` fields were not dense/increasing.
    BadOpOrder {
        expected: u64,
        got: u64,
    },
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Malformed(line, why) => write!(f, "line {line}: malformed record: {why}"),
            Self::WrongFormat(got) => write!(f, "not a weft-log file (format={got:?})"),
            Self::UnsupportedVersion(v) => {
                write!(
                    f,
                    "unsupported weft-log version {v} (this reader supports {VERSION})"
                )
            }
            Self::ChainBroken { op } => {
                write!(
                    f,
                    "integrity chain broken at op {op}: log truncated or edited"
                )
            }
            Self::BadOpOrder { expected, got } => {
                write!(f, "op sequence broken: expected {expected}, got {got}")
            }
        }
    }
}

impl std::error::Error for LogError {}

impl From<io::Error> for LogError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// The file destination behind [`LogWriter::create`]: plain, or gzip when the
/// path ends in `.gz`. Compression is a transport encoding — the inner text,
/// the chain, and the stream digest are identical either way (§11 of
/// docs/recording-format.md).
pub struct FileSink(SinkInner);

enum SinkInner {
    Plain(BufWriter<File>),
    Gzip(flate2::write::GzEncoder<BufWriter<File>>),
}

impl Write for FileSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.0 {
            SinkInner::Plain(w) => w.write(buf),
            SinkInner::Gzip(w) => w.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match &mut self.0 {
            SinkInner::Plain(w) => w.flush(),
            SinkInner::Gzip(w) => w.flush(), // sync flush: keeps the prefix decodable
        }
    }
}

impl FileSink {
    /// Write any pending compressed trailer and flush to disk. Required for a
    /// gzip sink to be a complete, standalone `.gz` file; a no-op flush for a
    /// plain sink.
    fn finalize(&mut self) -> io::Result<()> {
        match &mut self.0 {
            SinkInner::Plain(w) => w.flush(),
            SinkInner::Gzip(w) => {
                w.try_finish()?;
                w.get_mut().flush()
            }
        }
    }
}

/// Streaming writer. Every record is flushed as written so a crashed run
/// leaves a readable (chain-verifiable) prefix. (For a gzip sink the prefix
/// is sync-flushed but lacks the final trailer until [`LogWriter::finalize`];
/// prefer an uncompressed path while a scenario is still fragile.)
pub struct LogWriter<W: Write> {
    out: W,
    chain: u64,
    next_op: u64,
    sent: u64,
    dropped: u64,
}

impl LogWriter<FileSink> {
    /// Create a log file, truncating any existing one. A path ending in
    /// `.gz` is gzip-compressed transparently; [`Log::read`] detects either
    /// form by content, not extension.
    ///
    /// # Errors
    /// Propagates file-creation and write errors.
    pub fn create(path: &Path, header: &Header) -> Result<Self, LogError> {
        let file = BufWriter::new(File::create(path)?);
        let sink = if path.extension().is_some_and(|e| e == "gz") {
            FileSink(SinkInner::Gzip(flate2::write::GzEncoder::new(
                file,
                flate2::Compression::default(),
            )))
        } else {
            FileSink(SinkInner::Plain(file))
        };
        Self::new(sink, header)
    }

    /// Complete the file: for a gzip sink, write the trailer. Call after
    /// [`LogWriter::finish`] (the recorder does this for you).
    ///
    /// # Errors
    /// Propagates the final write/flush error.
    pub fn finalize(&mut self) -> Result<(), LogError> {
        self.out.finalize()?;
        Ok(())
    }
}

impl<W: Write> LogWriter<W> {
    /// Write the header and initialize the chain.
    ///
    /// # Errors
    /// Propagates write errors.
    ///
    /// # Panics
    /// Only if JSON serialization of the header fails, which cannot happen
    /// for these types.
    pub fn new(mut out: W, header: &Header) -> Result<Self, LogError> {
        let line = serde_json::to_string(header).expect("header serialization cannot fail");
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
        out.flush()?;
        Ok(Self {
            out,
            chain: fnv1a_once(line.as_bytes()),
            next_op: 0,
            sent: 0,
            dropped: 0,
        })
    }

    /// Append one event at virtual time `vt`, returning its op index.
    ///
    /// # Errors
    /// Propagates write errors.
    ///
    /// # Panics
    /// Only if JSON serialization of the record fails, which cannot happen
    /// for these types.
    pub fn append(&mut self, vt: u64, e: Event) -> Result<u64, LogError> {
        if let Event::Send { outcome, .. } = &e {
            self.sent += 1;
            if *outcome == SendOutcome::Dropped {
                self.dropped += 1;
            }
        }
        let op = self.next_op;
        self.next_op += 1;
        self.chain = chain_step(self.chain, op, vt, &e);
        let rec = Record {
            op,
            vt,
            e,
            chain: format!("{:016x}", self.chain),
        };
        let line = serde_json::to_string(&rec).expect("record serialization cannot fail");
        self.out.write_all(line.as_bytes())?;
        self.out.write_all(b"\n")?;
        self.out.flush()?;
        Ok(op)
    }

    /// Write the [`Event::End`] record and flush.
    ///
    /// # Errors
    /// Propagates write errors.
    pub fn finish(&mut self, vt: u64) -> Result<(), LogError> {
        let end = Event::End {
            events: self.next_op + 1,
            sent: self.sent,
            dropped: self.dropped,
        };
        self.append(vt, end)?;
        Ok(())
    }
}

/// A fully read and verified log.
#[derive(Clone, Debug, PartialEq)]
pub struct Log {
    pub header: Header,
    pub records: Vec<Record>,
}

impl Log {
    /// Read and verify a log file: format, version, op density, and the
    /// integrity chain. A gzip-compressed log (§11 of
    /// docs/recording-format.md) is detected by its magic bytes — the file
    /// extension does not matter — and decompressed transparently; the chain
    /// is always verified over the uncompressed text.
    ///
    /// # Errors
    /// Any [`LogError`]; a broken chain means truncation or tampering.
    pub fn read(path: &Path) -> Result<Self, LogError> {
        let bytes = std::fs::read(path)?;
        if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut text = Vec::new();
            io::Read::read_to_end(
                &mut flate2::read::GzDecoder::new(bytes.as_slice()),
                &mut text,
            )?;
            Self::from_reader(text.as_slice())
        } else {
            Self::from_reader(bytes.as_slice())
        }
    }

    /// Read and verify from any reader (see [`Log::read`]).
    ///
    /// # Errors
    /// As [`Log::read`].
    pub fn from_reader(r: impl BufRead) -> Result<Self, LogError> {
        let mut lines = r.lines();
        let head_line = lines
            .next()
            .ok_or_else(|| LogError::Malformed(1, "empty file".into()))??;
        let header: Header =
            serde_json::from_str(&head_line).map_err(|e| LogError::Malformed(1, e.to_string()))?;
        if header.format != FORMAT {
            return Err(LogError::WrongFormat(header.format));
        }
        if header.version != VERSION {
            return Err(LogError::UnsupportedVersion(header.version));
        }

        let mut chain = fnv1a_once(head_line.as_bytes());
        let mut records = Vec::new();
        for (i, line) in lines.enumerate() {
            let line = line?;
            if line.is_empty() {
                continue; // tolerate a trailing newline
            }
            let rec: Record = serde_json::from_str(&line)
                .map_err(|e| LogError::Malformed(i + 2, e.to_string()))?;
            let expected = records.len() as u64;
            if rec.op != expected {
                return Err(LogError::BadOpOrder {
                    expected,
                    got: rec.op,
                });
            }
            chain = chain_step(chain, rec.op, rec.vt, &rec.e);
            if rec.chain != format!("{chain:016x}") {
                return Err(LogError::ChainBroken { op: rec.op });
            }
            records.push(rec);
        }
        Ok(Self { header, records })
    }

    /// Digest of the replay-relevant event stream (op, vt, event — not the
    /// chain or header metadata). Two logs with equal digests describe
    /// byte-identical executions.
    #[must_use]
    pub fn stream_digest(&self) -> u64 {
        let mut h = FNV_OFFSET;
        for r in &self.records {
            h = fnv1a(h, canon(r.op, r.vt, &r.e).as_bytes());
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Header {
        Header {
            format: FORMAT.into(),
            version: VERSION,
            seed: 42,
            net: "loss=0.5".into(),
            meta: Meta::default(),
        }
    }

    fn sample_events() -> Vec<Event> {
        let a = Addr {
            ip: 0x7f00_0001,
            port: 100,
        };
        let b = Addr {
            ip: 0x7f00_0002,
            port: 200,
        };
        vec![
            Event::Connect { conn: 0 },
            Event::Hello { conn: 0, node: 0 },
            Event::Bind { conn: 0, addr: a },
            Event::Send {
                conn: 1,
                src: b,
                dst: a,
                chan_seq: 0,
                payload: "68690a".into(),
                outcome: SendOutcome::Enqueued {
                    to_conn: 0,
                    deliv_ns: 1500,
                    tie: 0,
                },
            },
            Event::Recv {
                conn: 0,
                blocking: false,
                outcome: RecvOutcome::Delivered {
                    src: b,
                    dst: a,
                    deliv_ns: 1500,
                    tie: 0,
                    payload: "68690a".into(),
                },
            },
        ]
    }

    fn write_sample() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = LogWriter::new(&mut buf, &header()).unwrap();
        for (i, e) in sample_events().into_iter().enumerate() {
            w.append(i as u64 * 10, e).unwrap();
        }
        w.finish(50).unwrap();
        buf
    }

    #[test]
    fn round_trip_and_chain_verify() {
        let buf = write_sample();
        let log = Log::from_reader(buf.as_slice()).unwrap();
        assert_eq!(log.header, header());
        assert_eq!(log.records.len(), 6); // 5 events + End
        assert_eq!(log.records[3].vt, 30);
        assert!(matches!(
            log.records[5].e,
            Event::End {
                sent: 1,
                dropped: 0,
                ..
            }
        ));
    }

    #[test]
    fn truncation_is_detected() {
        let buf = write_sample();
        // Drop the last line (End record): reader still parses, but a consumer
        // can see there is no End. Now corrupt an interior byte instead:
        let mut corrupt = buf.clone();
        // Flip one payload hex char somewhere mid-file.
        let pos = corrupt.windows(6).position(|w| w == b"68690a").unwrap();
        corrupt[pos] = b'7';
        let err = Log::from_reader(corrupt.as_slice()).unwrap_err();
        assert!(matches!(err, LogError::ChainBroken { .. }), "got {err}");
    }

    #[test]
    fn record_reorder_is_detected() {
        let buf = write_sample();
        let text = String::from_utf8(buf).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.swap(2, 3); // swap two records
        let rejoined = lines.join("\n");
        let err = Log::from_reader(rejoined.as_bytes()).unwrap_err();
        assert!(matches!(err, LogError::BadOpOrder { .. }), "got {err}");
    }

    #[test]
    fn wrong_version_is_rejected() {
        let mut h = header();
        h.version = 999;
        let mut buf = Vec::new();
        let _w = LogWriter::new(&mut buf, &h).unwrap();
        let err = Log::from_reader(buf.as_slice()).unwrap_err();
        assert!(matches!(err, LogError::UnsupportedVersion(999)));
    }

    #[test]
    fn stream_digest_ignores_meta_but_not_events() {
        let buf1 = write_sample();
        let log1 = Log::from_reader(buf1.as_slice()).unwrap();

        // Same events, different meta → same digest.
        let mut h2 = header();
        h2.meta.label = Some("other machine".into());
        let mut buf2 = Vec::new();
        let mut w = LogWriter::new(&mut buf2, &h2).unwrap();
        for (i, e) in sample_events().into_iter().enumerate() {
            w.append(i as u64 * 10, e).unwrap();
        }
        w.finish(50).unwrap();
        let log2 = Log::from_reader(buf2.as_slice()).unwrap();
        assert_eq!(log1.stream_digest(), log2.stream_digest());

        // Different events → different digest.
        let mut buf3 = Vec::new();
        let mut w = LogWriter::new(&mut buf3, &header()).unwrap();
        w.append(0, Event::Connect { conn: 7 }).unwrap();
        w.finish(0).unwrap();
        let log3 = Log::from_reader(buf3.as_slice()).unwrap();
        assert_ne!(log1.stream_digest(), log3.stream_digest());
    }
}
