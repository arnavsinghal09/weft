//! Demo: record a live broker run whose latency variance violates
//! per-channel FIFO, then print the violation report.
//!
//! Usage: cargo run -p weft-replay --example demo_violation [LOG_PATH]
//! Then:  weft replay LOG_PATH --check fifo

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;

use weft_net::wire::{read_from_broker, write_to_broker, FromBroker, ToBroker, VAddr};
use weft_net::{config, Broker};
use weft_replay::invariant::PerChannelFifo;
use weft_replay::log::{Header, Meta, FORMAT, VERSION};
use weft_replay::{replay_log, report, Log, Recorder};

const SEED: u64 = 3;
const NET: &str = "latency=uniform:1000-100000";

fn call(s: &mut UnixStream, m: &ToBroker) -> FromBroker {
    write_to_broker(s, m).unwrap();
    read_from_broker(s).unwrap()
}

fn main() {
    let log_path: PathBuf = std::env::args_os().nth(1).map_or_else(
        || std::env::temp_dir().join("weft-demo.weftlog"),
        PathBuf::from,
    );
    let sock = std::env::temp_dir().join(format!("weft-demo-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock);

    let header = Header {
        format: FORMAT.into(),
        version: VERSION,
        seed: SEED,
        net: NET.into(),
        meta: Meta {
            label: Some("demo-fifo-violation".into()),
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

    // Two "nodes": a sender bursting 20 datagrams and a receiver draining.
    let (ra, ta) = (VAddr::new(0x7f00_0001, 100), VAddr::new(0x7f00_0002, 200));
    let mut rx = UnixStream::connect(&sock).unwrap();
    call(&mut rx, &ToBroker::Hello { node_id: 0 });
    call(&mut rx, &ToBroker::Bind { addr: ra });
    let mut tx = UnixStream::connect(&sock).unwrap();
    call(&mut tx, &ToBroker::Hello { node_id: 1 });
    call(&mut tx, &ToBroker::Bind { addr: ta });
    for i in 0u32..20 {
        call(
            &mut tx,
            &ToBroker::Send {
                src: ta,
                dst: ra,
                payload: i.to_le_bytes().to_vec(),
                local_vt: 0,
            },
        );
    }
    while let FromBroker::Deliver { .. } = call(
        &mut rx,
        &ToBroker::Recv {
            addr: ra,
            blocking: false,
            local_vt: 0,
        },
    ) {}
    drop(rx);
    std::thread::sleep(std::time::Duration::from_millis(50));
    drop(tx);
    std::thread::sleep(std::time::Duration::from_millis(50));

    let violations = recorder.finish().unwrap();
    let _ = std::fs::remove_file(&sock);

    println!(
        "recorded {} → {} violation(s) live\n",
        log_path.display(),
        violations.len()
    );

    // Verify by replaying, then print the full report for the first one.
    let log = Log::read(&log_path).unwrap();
    let out = replay_log(&log, vec![Box::new(PerChannelFifo::new())], None).unwrap();
    assert!(out.identical, "replay must reproduce the recording exactly");
    println!(
        "replay identical: {} ops, stream digest {:016x}\n",
        out.ops_replayed, out.stream_digest
    );
    if let Some(v) = out.violations.first() {
        print!("{}", report::render(v, &log, &log_path));
    }
}
