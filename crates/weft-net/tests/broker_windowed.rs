//! Windowed multi-host broker: the seq+core+recv composition must linearize
//! sends in virtual-time order regardless of the real-time order in which the
//! datagrams reach the broker. The window.rs unit tests prove the sequencer
//! itself is arrival-independent; this drives the whole live broker (real
//! sockets, blocking recv, deferred sealing) and checks the recording and the
//! delivered order are identical under two *opposite* arrival interleavings.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use weft_net::broker::Observed;
use weft_net::wire::{read_from_broker, write_to_broker, FromBroker, ToBroker, VAddr};
use weft_net::{config, Broker};

const WINDOW_NS: u64 = 100;
// Windowed mode needs lookahead (minimum latency) >= window width, else a
// receiver's reactivation bound equals a send's own time and stalls its
// delivery. Fixed 100ns latency gives exactly that lookahead.
const NET_SPEC: &str = "latency=fixed:100";

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
    fn send_at(&mut self, src: VAddr, dst: VAddr, payload: &[u8], local_vt: u64) {
        let m = ToBroker::Send {
            src,
            dst,
            payload: payload.to_vec(),
            local_vt,
        };
        assert!(matches!(self.call(&m), FromBroker::Ack { .. }));
    }
    fn recv_blocking(&mut self, addr: VAddr) -> Vec<u8> {
        match self.call(&ToBroker::Recv {
            addr,
            blocking: true,
            local_vt: 0,
        }) {
            FromBroker::Deliver { payload, .. } => payload,
            other => panic!("expected delivery, got {other:?}"),
        }
    }
}

fn addr(node: u32, port: u16) -> VAddr {
    VAddr::new(0x7f00_0001 + node, port)
}

type Recording = Vec<(u64, Vec<u8>)>;

fn start(seed: u64, recorded: &Arc<Mutex<Recording>>) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("weft-win-broker-{}-{n}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let model = config::parse(seed, NET_SPEC).unwrap();
    let sink = Arc::clone(recorded);
    let observer: weft_net::broker::Observer = Box::new(move |_vt, ev| {
        if let Observed::Send {
            send_vt, payload, ..
        } = ev
        {
            sink.lock().unwrap().push((send_vt, payload.to_vec()));
        }
    });
    let broker = Arc::new(Broker::bind_with_window(&path, model, Some(observer), WINDOW_NS).unwrap());
    std::thread::spawn(move || broker.run());
    path
}

/// Run the same two-sender scenario. `swap` flips which sender's datagram
/// reaches the broker first (each `send_at` waits for its Ack, so the arrival
/// order at the broker is exactly the call order here). The two senders sit
/// in the same window; sender A emits at local_vt 10, sender B at 20 — so the
/// virtual-time-ordered (arrival-independent) result is always A then B.
fn run_scenario(swap: bool) -> (Recording, Vec<Vec<u8>>) {
    let recorded = Arc::new(Mutex::new(Recording::new()));
    let path = start(7, &recorded);

    // Receiver connects and binds first: it stays live (frontier 0) so no
    // window can seal until it parks, which prevents a premature seal from
    // feeding sends before the receiver has bound.
    let mut rx = Client::connect(&path, 0);
    let ra = addr(0, 100);
    rx.bind(ra);

    let mut tx_a = Client::connect(&path, 1);
    let ta = addr(1, 200);
    tx_a.bind(ta);
    let mut tx_b = Client::connect(&path, 2);
    let tb = addr(2, 300);
    tx_b.bind(tb);

    // Receiver parks in two blocking recvs, leaving the sealing quorum.
    let rx_thread = std::thread::spawn(move || {
        let first = rx.recv_blocking(ra);
        let second = rx.recv_blocking(ra);
        vec![first, second]
    });
    // Let the receiver reach its parked state before the senders speak.
    std::thread::sleep(std::time::Duration::from_millis(50));

    if swap {
        tx_b.send_at(tb, ra, b"b", 20);
        tx_a.send_at(ta, ra, b"a", 10);
    } else {
        tx_a.send_at(ta, ra, b"a", 10);
        tx_b.send_at(tb, ra, b"b", 20);
    }

    // Senders release: with both closed and the receiver parked, every window
    // seals (horizon -> INFINITY) and the buffered sends feed the receiver.
    drop(tx_a);
    drop(tx_b);

    let delivered = rx_thread.join().unwrap();
    let snapshot = recorded.lock().unwrap().clone();
    (snapshot, delivered)
}

#[test]
fn recording_is_independent_of_arrival_order() {
    let (rec_ab, deliv_ab) = run_scenario(false);
    let (rec_ba, deliv_ba) = run_scenario(true);

    // The recording is the sealed (virtual-time) order, not arrival order.
    assert_eq!(
        rec_ab,
        vec![(10, b"a".to_vec()), (20, b"b".to_vec())],
        "sealed order must be by (local_vt): a@10 then b@20"
    );
    assert_eq!(
        rec_ab, rec_ba,
        "opposite arrival orders produced different recordings"
    );
    assert_eq!(
        deliv_ab,
        vec![b"a".to_vec(), b"b".to_vec()],
        "delivered order must follow anchored delivery times"
    );
    assert_eq!(
        deliv_ab, deliv_ba,
        "opposite arrival orders produced different delivery"
    );
}
