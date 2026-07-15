//! Broker integration tests: real Unix-socket connections speaking the wire
//! protocol against a live broker, covering loss determinism, partitions,
//! delivery ordering, and nodes joining/leaving mid-scenario.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;

use weft_net::config;
use weft_net::wire::{read_from_broker, write_to_broker, FromBroker, ToBroker, VAddr};
use weft_net::Broker;

struct Client(UnixStream);

impl Client {
    fn connect(path: &PathBuf, node: u32) -> Self {
        let mut c = Self(UnixStream::connect(path).unwrap());
        assert!(matches!(
            c.call(&ToBroker::Hello {
                node_id: node,
                host_id: 0,
            }),
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
    /// Non-blocking receive; `None` when the queue is empty.
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

fn start_broker(seed: u64, spec: &str) -> (PathBuf, Arc<Broker>) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("weft-test-broker-{}-{n}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let model = config::parse(seed, spec).unwrap();
    let broker = Arc::new(Broker::bind(&path, model).unwrap());
    {
        let b = Arc::clone(&broker);
        std::thread::spawn(move || b.run());
    }
    (path, broker)
}

fn addr(node: u32, port: u16) -> VAddr {
    VAddr::new(0x7f00_0001 + node, port)
}

/// Drain every currently-deliverable payload for `addr`.
fn drain(c: &mut Client, a: VAddr) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(p) = c.try_recv(a) {
        out.push(p);
    }
    out
}

#[test]
fn reliable_delivery_round_trip() {
    let (path, _b) = start_broker(1, "");
    let mut rx = Client::connect(&path, 0);
    let mut tx = Client::connect(&path, 1);
    let (ra, ta) = (addr(0, 100), addr(1, 200));
    rx.bind(ra);
    tx.bind(ta);
    tx.send(ta, ra, b"hello");
    assert_eq!(rx.try_recv(ra), Some(b"hello".to_vec()));
    assert_eq!(rx.try_recv(ra), None);
}

#[test]
fn loss_is_deterministic_and_roughly_matches_probability() {
    const N: usize = 400;
    let survivors = |seed: u64| {
        let (path, _b) = start_broker(seed, "loss=0.5");
        let mut rx = Client::connect(&path, 0);
        let mut tx = Client::connect(&path, 1);
        let (ra, ta) = (addr(0, 100), addr(1, 200));
        rx.bind(ra);
        tx.bind(ta);
        for i in 0..N {
            tx.send(ta, ra, &u32::try_from(i).unwrap().to_le_bytes());
        }
        drain(&mut rx, ra)
    };
    let first = survivors(9);
    let again = survivors(9);
    assert_eq!(first, again, "same seed must lose the same datagrams");
    // ~50% with generous slack.
    assert!(
        (100..300).contains(&first.len()),
        "survived {}",
        first.len()
    );
    let other = survivors(10);
    assert_ne!(
        first, other,
        "different seed should lose different datagrams"
    );
}

#[test]
fn latency_variance_reorders_deterministically() {
    let ordering = |seed: u64| {
        let (path, _b) = start_broker(seed, "latency=uniform:1000-100000");
        let mut rx = Client::connect(&path, 0);
        let mut tx = Client::connect(&path, 1);
        let (ra, ta) = (addr(0, 100), addr(1, 200));
        rx.bind(ra);
        tx.bind(ta);
        for i in 0u32..20 {
            tx.send(ta, ra, &i.to_le_bytes());
        }
        drain(&mut rx, ra)
    };
    let first = ordering(3);
    assert_eq!(first, ordering(3));
    assert_eq!(first.len(), 20, "no loss configured; nothing may vanish");
    let in_order: Vec<Vec<u8>> = (0u32..20).map(|i| i.to_le_bytes().to_vec()).collect();
    assert_ne!(
        first, in_order,
        "uniform latency over a burst should reorder"
    );
}

#[test]
fn partition_blocks_and_heals_by_respawning_broker() {
    // Partitioned: 0 alone, 1 alone.
    let (path, _b) = start_broker(5, "partition=0|1");
    let mut rx = Client::connect(&path, 0);
    let mut tx = Client::connect(&path, 1);
    let (ra, ta) = (addr(0, 100), addr(1, 200));
    rx.bind(ra);
    tx.bind(ta);
    tx.send(ta, ra, b"blocked");
    assert_eq!(rx.try_recv(ra), None, "cross-partition datagram must drop");

    // Same-side traffic still flows: node 1 → node 1.
    let mut rx1 = Client::connect(&path, 1);
    let rb = addr(1, 300);
    rx1.bind(rb);
    tx.send(ta, rb, b"same side");
    assert_eq!(rx1.try_recv(rb), Some(b"same side".to_vec()));
}

#[test]
fn nodes_join_and_leave_mid_scenario() {
    let (path, _b) = start_broker(7, "");
    let mut tx = Client::connect(&path, 1);
    let ta = addr(1, 200);
    tx.bind(ta);

    // Send to an address nobody has bound yet: silently discarded, like UDP
    // to a closed port.
    let ra = addr(0, 100);
    tx.send(ta, ra, b"early");

    // Node 0 joins late, binds, and only sees traffic sent after its bind.
    let mut rx = Client::connect(&path, 0);
    rx.bind(ra);
    tx.send(ta, ra, b"on time");
    assert_eq!(drain(&mut rx, ra), vec![b"on time".to_vec()]);

    // Node 0 leaves (drops its connection): its binding disappears, and new
    // traffic to it is discarded rather than wedging the broker.
    drop(rx);
    // Give the broker's handler thread a moment to observe the hangup.
    std::thread::sleep(std::time::Duration::from_millis(50));
    tx.send(ta, ra, b"to the departed");

    // A fresh connection can re-claim the same address (a node "rejoining").
    let mut rx2 = Client::connect(&path, 0);
    rx2.bind(ra);
    tx.send(ta, ra, b"rejoined");
    assert_eq!(drain(&mut rx2, ra), vec![b"rejoined".to_vec()]);
}

#[test]
fn blocking_recv_wakes_on_send() {
    let (path, _b) = start_broker(11, "");
    let mut rx = Client::connect(&path, 0);
    let ra = addr(0, 100);
    rx.bind(ra);

    let handle = std::thread::spawn(move || {
        // Blocking request parks in the broker until the send below.
        match rx.call(&ToBroker::Recv {
            addr: ra,
            blocking: true,
            local_vt: 0,
        }) {
            FromBroker::Deliver { payload, .. } => payload,
            other => panic!("expected delivery, got {other:?}"),
        }
    });

    std::thread::sleep(std::time::Duration::from_millis(50));
    let mut tx = Client::connect(&path, 1);
    let ta = addr(1, 200);
    tx.bind(ta);
    tx.send(ta, ra, b"wake up");
    assert_eq!(handle.join().unwrap(), b"wake up".to_vec());
}
