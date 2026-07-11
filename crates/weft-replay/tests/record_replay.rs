//! End-to-end proof for Phase 5: record a *live* broker run (real Unix
//! sockets, real handler threads, real OS scheduling), then replay the log
//! and reproduce the byte-identical event stream — including the invariant
//! violation — many times over.
//!
//! This is the core validation demanded by the phase: deliberately trigger a
//! violation (uniform latency variance reorders a burst, breaking per-channel
//! FIFO — the same fault the kvreplica demo exploits), record it, and replay
//! it to the exact same failure at least 10 times.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;

use weft_net::wire::{read_from_broker, write_to_broker, FromBroker, ToBroker, VAddr};
use weft_net::{config, Broker};
use weft_replay::invariant::PerChannelFifo;
use weft_replay::log::{Event, Header, Meta, FORMAT, VERSION};
use weft_replay::{replay_log, report, Log, Recorder};

struct Client(UnixStream);

impl Client {
    fn connect(path: &PathBuf, node: u32) -> Self {
        let mut c = Self(UnixStream::connect(path).unwrap());
        assert!(matches!(
            c.call(&ToBroker::Hello { node_id: node }),
            FromBroker::Ack { .. }
        ));
        c
    }
    fn call(&mut self, m: &ToBroker) -> FromBroker {
        write_to_broker(&mut self.0, m).unwrap();
        read_from_broker(&mut self.0).unwrap()
    }
    fn bind(&mut self, addr: VAddr) {
        assert!(matches!(
            self.call(&ToBroker::Bind { addr }),
            FromBroker::Ack { .. }
        ));
    }
    fn send(&mut self, src: VAddr, dst: VAddr, payload: &[u8]) {
        let m = ToBroker::Send {
            src,
            dst,
            payload: payload.to_vec(),
            local_vt: 0,
        };
        assert!(matches!(self.call(&m), FromBroker::Ack { .. }));
    }
    fn try_recv(&mut self, addr: VAddr) -> Option<Vec<u8>> {
        match self.call(&ToBroker::Recv {
            addr,
            blocking: false,
            local_vt: 0,
        }) {
            FromBroker::Deliver { payload, .. } => Some(payload),
            FromBroker::Empty { .. } => None,
            FromBroker::Ack { .. } => panic!("unexpected Ack"),
        }
    }
}

fn addr(node: u32, port: u16) -> VAddr {
    VAddr::new(0x7f00_0001 + node, port)
}

fn tmp(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("weft-rr-{}-{n}-{name}", std::process::id()))
}

const SEED: u64 = 3;
const NET: &str = "latency=uniform:1000-100000";

/// Record one live run: 20-datagram burst under reordering latency, drained
/// by the receiver. Returns the log path.
fn record_live_run(label: &str) -> PathBuf {
    let sock = tmp(&format!("{label}.sock"));
    let log_path = tmp(&format!("{label}.weftlog"));
    let _ = std::fs::remove_file(&sock);

    let header = Header {
        format: FORMAT.into(),
        version: VERSION,
        seed: SEED,
        net: NET.into(),
        meta: Meta {
            label: Some(label.into()),
            ..Meta::default()
        },
    };
    let recorder =
        Recorder::create(&log_path, &header, vec![Box::new(PerChannelFifo::new())]).unwrap();

    let model = config::parse(SEED, NET).unwrap();
    let broker = Arc::new(Broker::bind_with(&sock, model, Some(recorder.observer())).unwrap());
    {
        let b = Arc::clone(&broker);
        std::thread::spawn(move || b.run());
    }

    let mut rx = Client::connect(&sock, 0);
    let mut tx = Client::connect(&sock, 1);
    let (ra, ta) = (addr(0, 100), addr(1, 200));
    rx.bind(ra);
    tx.bind(ta);
    for i in 0u32..20 {
        tx.send(ta, ra, &i.to_le_bytes());
    }
    let mut got = Vec::new();
    while let Some(p) = rx.try_recv(ra) {
        got.push(p);
    }
    assert_eq!(got.len(), 20, "no loss configured; nothing may vanish");

    // Close the node connections and let the broker observe the hangups, so
    // the log ends with deterministic disconnects... actually disconnect
    // *order* between two racing hangups is OS-scheduled, which is exactly
    // the kind of input the log exists to capture. Serialize them here so the
    // scenario itself is tidy: drop rx, then tx.
    drop(rx);
    std::thread::sleep(std::time::Duration::from_millis(50));
    drop(tx);
    std::thread::sleep(std::time::Duration::from_millis(50));

    let violations = recorder.finish().unwrap();
    assert!(
        !violations.is_empty(),
        "uniform latency 1000-100000 over a 20-datagram burst must reorder \
         (the broker_integration test asserts the same fact)"
    );
    let _ = std::fs::remove_file(&sock);
    log_path
}

#[test]
fn live_run_records_a_verifiable_log_with_the_violation() {
    let path = record_live_run("verify");
    let log = Log::read(&path).unwrap();

    // Chain verified by Log::read. The violation is in the log itself,
    // anchored to an op and a virtual time.
    let viol: Vec<_> = log
        .records
        .iter()
        .filter(|r| matches!(r.e, Event::Violation { .. }))
        .collect();
    assert!(!viol.is_empty(), "recorded log must contain the violation");
    assert!(viol[0].vt > 0, "violation must sit on the logical timeline");

    // And it ends properly.
    assert!(matches!(log.records.last().unwrap().e, Event::End { .. }));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn recorded_violation_replays_identically_ten_times() {
    let path = record_live_run("replay10");
    let log = Log::read(&path).unwrap();
    let reference_digest = log.stream_digest();

    let mut reference_violations = None;
    for run in 0..10 {
        // Fresh read every time: nothing carries over between replays.
        let log = Log::read(&path).unwrap();
        let out = replay_log(&log, vec![Box::new(PerChannelFifo::new())], None).unwrap();

        assert!(out.identical, "replay {run} diverged: {:?}", out.divergence);
        assert_eq!(
            out.stream_digest, reference_digest,
            "replay {run} produced a different stream digest"
        );
        assert!(
            !out.violations.is_empty(),
            "replay {run} lost the violation"
        );

        match &reference_violations {
            None => reference_violations = Some(out.violations),
            Some(first) => assert_eq!(
                &out.violations, first,
                "replay {run}: violations differ from run 0 — same failure \
                 must reproduce at the same op and virtual time"
            ),
        }
    }

    // The violation is anchored: same op, same vt, every time.
    let v = &reference_violations.unwrap()[0];
    assert_eq!(v.invariant, "per-channel-fifo");
    assert!(v.vt > 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn two_recordings_of_the_same_scenario_may_differ_but_each_replays_exactly() {
    // Honesty check: the *linearization* is OS-scheduled, so two live
    // recordings need not be identical to each other — that is precisely why
    // it must be recorded. What is guaranteed: each recording replays to
    // itself, byte for byte.
    let p1 = record_live_run("a");
    let p2 = record_live_run("b");
    let l1 = Log::read(&p1).unwrap();
    let l2 = Log::read(&p2).unwrap();

    for (log, name) in [(&l1, "a"), (&l2, "b")] {
        let out = replay_log(log, Vec::new(), None).unwrap();
        assert!(out.identical, "recording {name} failed self-replay");
        assert_eq!(out.stream_digest, log.stream_digest());
    }
    let _ = std::fs::remove_file(&p1);
    let _ = std::fs::remove_file(&p2);
}

#[test]
fn violation_report_names_the_exact_reproduction() {
    let path = record_live_run("report");
    let log = Log::read(&path).unwrap();
    let out = replay_log(&log, vec![Box::new(PerChannelFifo::new())], None).unwrap();
    let text = report::render(&out.violations[0], &log, &path);

    // Zero-archaeology: seed, net spec, log path, op anchor, replay command.
    assert!(text.contains(&format!("seed        : {SEED}")));
    assert!(text.contains(NET));
    assert!(text.contains(path.to_str().unwrap()));
    assert!(text.contains("weft replay"));
    assert!(text.contains("--until"));
    let _ = std::fs::remove_file(&path);
}
