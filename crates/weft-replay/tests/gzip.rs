//! Gzip compression (stretch goal): a `.gz` log is a transport encoding of
//! the same `weft-log` v1 text — identical records, chain, and stream digest
//! — detected by content, smaller on disk.

use std::path::{Path, PathBuf};

use weft_replay::log::{Event, Header, LogWriter, Meta, SendOutcome, FORMAT, VERSION};
use weft_replay::{replay_log, Log};

fn tmp(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("weft-gz-{}-{n}-{name}", std::process::id()))
}

fn header() -> Header {
    Header {
        format: FORMAT.into(),
        version: VERSION,
        seed: 3,
        net: "latency=uniform:1000-100000".into(),
        window_ns: 0,
        meta: Meta::default(),
    }
}

/// A repetitive-but-valid event burst (compresses well). Sends target an
/// address nobody bound, so every outcome is honestly `no_receiver` and the
/// virtual clock never advances — the replayer verifies both.
fn events() -> Vec<Event> {
    use weft_replay::log::Addr;
    let bound = Addr {
        ip: 0x7f00_0001,
        port: 100,
    };
    let src = Addr {
        ip: 0x7f00_0002,
        port: 200,
    };
    let unbound = Addr {
        ip: 0x7f00_0001,
        port: 999,
    };
    let mut out = vec![
        Event::Connect { conn: 0 },
        Event::Bind {
            conn: 0,
            addr: bound,
        },
    ];
    for i in 0..200u64 {
        out.push(Event::Send {
            conn: 1,
            src,
            dst: unbound,
            chan_seq: i,
            send_vt: 0,
            payload: "deadbeef".into(),
            outcome: SendOutcome::NoReceiver,
        });
    }
    out
}

fn write_to(path: &Path) {
    let mut w = LogWriter::create(path, &header()).unwrap();
    for e in events() {
        // vt stays 0 throughout: NoReceiver sends never schedule a delivery,
        // so the virtual-time high-water mark never advances. (Writing any
        // other vt would be a dishonest log that replay rightly rejects.)
        w.append(0, e).unwrap();
    }
    w.finish(0).unwrap();
    w.finalize().unwrap();
}

#[test]
fn gz_and_plain_logs_are_the_same_log() {
    let plain = tmp("same.weftlog");
    let gz = tmp("same.weftlog.gz");
    write_to(&plain);
    write_to(&gz);

    let lp = Log::read(&plain).unwrap();
    let lg = Log::read(&gz).unwrap();
    assert_eq!(lp, lg, "compression must not change the log's content");
    assert_eq!(lp.stream_digest(), lg.stream_digest());

    let sp = std::fs::metadata(&plain).unwrap().len();
    let sg = std::fs::metadata(&gz).unwrap().len();
    assert!(
        sg < sp / 2,
        "repetitive log should compress well: plain={sp}B gz={sg}B"
    );

    let _ = std::fs::remove_file(&plain);
    let _ = std::fs::remove_file(&gz);
}

#[test]
fn gzip_is_detected_by_content_not_extension() {
    let gz = tmp("hidden.weftlog.gz");
    write_to(&gz);
    // Strip the .gz extension: reader must still decompress via magic bytes.
    let renamed = tmp("hidden.weftlog");
    std::fs::rename(&gz, &renamed).unwrap();
    let log = Log::read(&renamed).unwrap();
    assert_eq!(log.records.len(), 203); // 202 events + End
    let _ = std::fs::remove_file(&renamed);
}

#[test]
fn compressed_log_replays_identically() {
    let gz = tmp("replay.weftlog.gz");
    write_to(&gz);
    let log = Log::read(&gz).unwrap();
    let out = replay_log(&log, Vec::new(), None).unwrap();
    assert!(out.identical, "divergence: {:?}", out.divergence);
    assert_eq!(out.stream_digest, log.stream_digest());
    let _ = std::fs::remove_file(&gz);
}

#[test]
fn tampering_with_compressed_content_is_still_detected() {
    // Decompress, corrupt one byte of a payload, recompress: the chain
    // (defined over the uncompressed text) must catch it.
    use std::io::{Read, Write};
    let gz = tmp("tamper.weftlog.gz");
    write_to(&gz);

    let bytes = std::fs::read(&gz).unwrap();
    let mut text = Vec::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_end(&mut text)
        .unwrap();
    let pos = text
        .windows(8)
        .position(|w| w == b"deadbeef")
        .expect("payload present");
    text[pos] = b'f';

    let tampered = tmp("tampered.weftlog.gz");
    let mut enc = flate2::write::GzEncoder::new(
        std::fs::File::create(&tampered).unwrap(),
        flate2::Compression::default(),
    );
    enc.write_all(&text).unwrap();
    enc.finish().unwrap();

    let err = Log::read(&tampered).unwrap_err();
    assert!(
        matches!(err, weft_replay::LogError::ChainBroken { .. }),
        "got {err}"
    );
    let _ = std::fs::remove_file(&gz);
    let _ = std::fs::remove_file(&tampered);
}
