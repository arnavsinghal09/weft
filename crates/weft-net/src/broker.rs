//! The central broker: every simulated datagram passes through here instead of
//! the kernel network stack, so the seeded [`FaultModel`] has complete control.
//!
//! One Unix-socket connection per virtual socket in a node. A per-connection
//! handler thread reads [`ToBroker`] requests; shared state (the routing table
//! and per-connection delivery queues) sits behind one mutex, with a condition
//! variable to wake blocked `recv`s.
//!
//! Delivery order: the broker treats a burst of sends as concurrent and orders
//! a destination's queue purely by sampled latency (ties broken by a global
//! enqueue counter for determinism). This deliberately maximizes reordering
//! exposure — see docs/network-model.md.

use std::collections::{BinaryHeap, HashMap};
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::fault::FaultModel;
use crate::wire::{
    read_to_broker, write_from_broker, FromBroker, ToBroker, VAddr,
};

struct Pending {
    deliv: u64,
    tie: u64,
    src: VAddr,
    dst: VAddr,
    payload: Vec<u8>,
}

impl PartialEq for Pending {
    fn eq(&self, o: &Self) -> bool {
        (self.deliv, self.tie) == (o.deliv, o.tie)
    }
}
impl Eq for Pending {}
impl Ord for Pending {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        // Reversed so a `BinaryHeap` (max-heap) yields the *smallest*
        // (deliv, tie) first.
        (o.deliv, o.tie).cmp(&(self.deliv, self.tie))
    }
}
impl PartialOrd for Pending {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

#[derive(Default)]
struct Conn {
    queue: BinaryHeap<Pending>,
    /// Whether the peer has closed (so a blocked recv can give up).
    closed: bool,
}

struct State {
    model: FaultModel,
    /// Which connection receives datagrams for a bound address.
    bound: HashMap<VAddr, usize>,
    conns: HashMap<usize, Conn>,
    /// Per-channel (src→dst) datagram counter, feeding the fault model.
    seq: HashMap<(VAddr, VAddr), u64>,
    /// Global enqueue counter, the deterministic delivery tiebreaker.
    tie: u64,
    /// Total datagrams sent / dropped, for stats.
    sent: u64,
    dropped: u64,
}

/// A running broker. Clone-free; share via the `Arc` inside.
pub struct Broker {
    listener: UnixListener,
    shared: Arc<(Mutex<State>, Condvar)>,
    /// Global logical time (nanoseconds) for process orchestration.
    /// Updated as messages are delivered. Used by event scheduler to trigger
    /// crashes, restarts, and partition changes at deterministic times.
    pub global_logical_time: Arc<AtomicU64>,
}

impl Broker {
    /// Bind the broker to a Unix socket `path` with the given fault model.
    ///
    /// # Errors
    /// Propagates the bind error (e.g. the path already exists).
    pub fn bind(path: &std::path::Path, model: FaultModel) -> io::Result<Self> {
        let listener = UnixListener::bind(path)?;
        Ok(Self {
            listener,
            shared: Arc::new((
                Mutex::new(State {
                    model,
                    bound: HashMap::new(),
                    conns: HashMap::new(),
                    seq: HashMap::new(),
                    tie: 0,
                    sent: 0,
                    dropped: 0,
                }),
                Condvar::new(),
            )),
            global_logical_time: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Accept and serve connections until the listener errors (e.g. is closed
    /// at shutdown). Each connection gets its own handler thread.
    ///
    /// # Panics
    ///
    /// Panics if the state lock is poisoned, which cannot happen: no holder
    /// performs a panicking operation.
    pub fn run(&self) {
        for (id, stream) in self.listener.incoming().enumerate() {
            let Ok(stream) = stream else { break };
            self.shared.0.lock().unwrap().conns.insert(id, Conn::default());
            let shared = Arc::clone(&self.shared);
            let global_time = Arc::clone(&self.global_logical_time);
            thread::spawn(move || handle_conn(id, stream, &shared, &global_time));
        }
    }

    /// Snapshot of `(datagrams_sent, datagrams_dropped)` for reporting.
    ///
    /// # Panics
    ///
    /// Same non-condition as [`Self::run`].
    #[must_use]
    pub fn stats(&self) -> (u64, u64) {
        let st = self.shared.0.lock().unwrap();
        (st.sent, st.dropped)
    }
}

fn handle_conn(
    id: usize,
    stream: UnixStream,
    shared: &Arc<(Mutex<State>, Condvar)>,
    global_time: &Arc<AtomicU64>,
) {
    let mut reader = io::BufReader::new(stream.try_clone().expect("dup unix stream"));
    let mut writer = stream;

    // Serve until EOF or a protocol error ends the connection.
    while let Ok(msg) = read_to_broker(&mut reader) {
        match msg {
            ToBroker::Hello { .. } => {
                let _ = write_from_broker(&mut writer, &FromBroker::Ack);
            }
            ToBroker::Bind { addr } => {
                shared.0.lock().unwrap().bound.insert(addr, id);
                let _ = write_from_broker(&mut writer, &FromBroker::Ack);
            }
            ToBroker::Send { src, dst, payload } => {
                route_send(shared, global_time, src, dst, &payload);
                let _ = write_from_broker(&mut writer, &FromBroker::Ack);
            }
            ToBroker::Recv { addr: _, blocking } => {
                let out = recv_next(shared, id, blocking);
                let _ = write_from_broker(&mut writer, &out);
            }
        }
    }

    // Cleanup on disconnect: drop the connection, unbind its addresses, and
    // wake anyone blocked (in case this was the only possible sender).
    let (lock, cvar) = &**shared;
    let mut st = lock.lock().unwrap();
    st.conns.remove(&id);
    st.bound.retain(|_, cid| *cid != id);
    cvar.notify_all();
}

fn route_send(
    shared: &Arc<(Mutex<State>, Condvar)>,
    global_time: &Arc<AtomicU64>,
    src: VAddr,
    dst: VAddr,
    payload: &[u8],
) {
    let (lock, cvar) = &**shared;
    let mut st = lock.lock().unwrap();
    st.sent += 1;

    let seq = {
        let e = st.seq.entry((src, dst)).or_insert(0);
        let v = *e;
        *e += 1;
        v
    };
    let fate = st.model.fate(src, dst, seq, payload.len());
    if fate.dropped {
        st.dropped += 1;
        return;
    }
    let Some(&cid) = st.bound.get(&dst) else {
        // No socket is bound to the destination: the datagram is discarded,
        // exactly as a UDP packet to a closed port would be.
        return;
    };
    let tie = st.tie;
    st.tie += 1;
    if let Some(conn) = st.conns.get_mut(&cid) {
        conn.queue.push(Pending {
            deliv: fate.delay_ns,
            tie,
            src,
            dst,
            payload: payload.to_vec(),
        });
        // Update global logical time: track the latest delivery time scheduled
        let current = global_time.load(Ordering::Relaxed);
        let delivery_time = fate.delay_ns;
        if delivery_time > current {
            global_time.store(delivery_time, Ordering::Relaxed);
        }
        cvar.notify_all();
    }
}

fn recv_next(shared: &Arc<(Mutex<State>, Condvar)>, id: usize, blocking: bool) -> FromBroker {
    let (lock, cvar) = &**shared;
    let mut st = lock.lock().unwrap();
    loop {
        if let Some(conn) = st.conns.get_mut(&id) {
            if let Some(p) = conn.queue.pop() {
                return FromBroker::Deliver {
                    src: p.src,
                    dst: p.dst,
                    payload: p.payload,
                };
            }
            if !blocking || conn.closed {
                return FromBroker::Empty;
            }
        } else {
            return FromBroker::Empty;
        }
        st = cvar.wait(st).unwrap();
    }
}
