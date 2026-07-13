//! The central broker: every simulated datagram passes through here instead of
//! the kernel network stack, so the seeded [`FaultModel`] has complete control.
//!
//! One Unix-socket connection per virtual socket in a node. A per-connection
//! handler thread reads [`ToBroker`] requests; all decisions are made by the
//! pure [`Core`](crate::core::Core) state machine behind one mutex, with a
//! condition variable to wake blocked `recv`s. Because replay
//! (`weft-replay`) drives the same `Core`, live behavior and replayed
//! behavior cannot drift apart silently.
//!
//! Delivery order: the broker treats a burst of sends as concurrent and orders
//! a destination's queue purely by sampled latency (ties broken by a global
//! enqueue counter for determinism). This deliberately maximizes reordering
//! exposure — see docs/network-model.md.
//!
//! # Recording
//!
//! The lock-serialized order in which requests reach the `Core` is the one
//! run input that is *not* a pure function of the seed (it depends on how the
//! OS schedules the node processes). An [`Observer`] installed via
//! [`Broker::bind_with`] sees every operation at exactly that linearization
//! point — while the state lock is held — which is what makes a recorded log
//! a faithful, replayable capture of the run. See docs/recording-format.md.

use std::io;
use std::io::{Read, Write};
use std::net::{TcpListener, ToSocketAddrs};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::core::{Core, RecvResult, SendResult};
use crate::fault::FaultModel;
use crate::window::WindowSequencer;
use crate::wire::{read_to_broker, write_from_broker, FromBroker, ToBroker, VAddr};

/// One linearized broker operation, delivered to an [`Observer`] at its
/// linearization point (under the state lock, decisions already made).
#[derive(Debug)]
pub enum Observed<'a> {
    Connect {
        conn: u64,
    },
    Hello {
        conn: u64,
        node: u32,
    },
    Bind {
        conn: u64,
        addr: VAddr,
    },
    Send {
        conn: u64,
        src: VAddr,
        dst: VAddr,
        chan_seq: u64,
        /// The sender local virtual time the delivery was anchored to (0 in
        /// single-host mode). Recorded so replay recomputes the same delivery.
        send_vt: u64,
        payload: &'a [u8],
        result: &'a SendResult,
    },
    /// A completed receive. For a blocking request this fires when the pop
    /// *succeeds* (its linearization point); the empty polls a blocked recv
    /// makes while waiting are not operations and are not observed.
    Recv {
        conn: u64,
        blocking: bool,
        result: &'a RecvResult,
    },
    Disconnect {
        conn: u64,
    },
}

/// Callback invoked for every linearized operation, with the core's
/// virtual-time high-water mark (ns) after the operation. Called while the
/// broker's state lock is held: implementations must not call back into the
/// broker and should return quickly.
pub type Observer = Box<dyn FnMut(u64, Observed<'_>) + Send>;

struct State {
    core: Core,
    observer: Option<Observer>,
    /// Largest |request local_vt - core vt| seen across all operations —
    /// the measured clock-skew bound (docs/MULTI_HOST_ARCHITECTURE.md).
    max_skew_ns: u64,
    /// `Some` in windowed multi-host mode: ops are buffered and ordered by the
    /// sequencer, not routed on arrival (docs/MULTI_HOST_CLOCK_PROTOCOL.md).
    seq: Option<WindowSequencer>,
}

impl State {
    fn new(model: FaultModel, observer: Option<Observer>, window_ns: u64) -> Self {
        Self {
            core: Core::new(model),
            observer,
            max_skew_ns: 0,
            seq: (window_ns > 0).then(|| WindowSequencer::new(window_ns)),
        }
    }

    /// Seal every window the sequencer now can, and feed the newly-assigned
    /// sends through `Core` in assigned order (their fates and anchored
    /// delivery times are drawn here, deterministically). No-op when not
    /// windowed. Returns whether anything sealed (so the caller can notify
    /// held receives).
    fn seal_and_feed(&mut self, global_time: &AtomicU64) -> bool {
        let Some(seq) = self.seq.as_mut() else {
            return false;
        };
        let assigned = seq.seal();
        if assigned.is_empty() {
            return false;
        }
        for op in assigned {
            let (chan_seq, result) = self.core.send(op.src, op.dst, &op.payload, op.local_vt);
            global_time.fetch_max(self.core.vt(), Ordering::Relaxed);
            if let Some(obs) = &mut self.observer {
                obs(
                    self.core.vt(),
                    Observed::Send {
                        conn: op.conn,
                        src: op.src,
                        dst: op.dst,
                        chan_seq,
                        send_vt: op.local_vt,
                        payload: &op.payload,
                        result: &result,
                    },
                );
            }
        }
        true
    }
}

impl State {
    fn track_skew(&mut self, local_vt: u64) {
        // local_vt == 0 means a clock-less caller (tests, old shims): skip.
        if local_vt > 0 {
            let skew = local_vt.abs_diff(self.core.vt());
            self.max_skew_ns = self.max_skew_ns.max(skew);
        }
    }
}

impl State {
    fn observe(&mut self, ev: Observed<'_>) {
        if let Some(obs) = &mut self.observer {
            obs(self.core.vt(), ev);
        }
    }
}

/// The broker's accept source: Unix socket (single-host) or TCP
/// (multi-host). Same wire protocol, same handler, same determinism —
/// transport carries the linearization, it never defines it.
enum Listener {
    Unix(UnixListener),
    Tcp(TcpListener),
}

/// A running broker. Clone-free; share via the `Arc` inside.
pub struct Broker {
    listener: Listener,
    shared: Arc<(Mutex<State>, Condvar)>,
    /// True when this broker runs the windowed multi-host protocol (a
    /// [`WindowSequencer`] is installed): sends are buffered and sealed in
    /// virtual-time order rather than routed on arrival. Captured once at
    /// bind so every handler thread agrees.
    windowed: bool,
    /// Global logical time (nanoseconds) for process orchestration.
    /// Updated as messages are delivered. Used by event scheduler to trigger
    /// crashes, restarts, and partition changes at deterministic times.
    pub global_logical_time: Arc<AtomicU64>,
}

impl Broker {
    fn from_listener(listener: Listener, model: FaultModel, observer: Option<Observer>, window_ns: u64) -> Self {
        Self {
            listener,
            shared: Arc::new((
                Mutex::new(State::new(model, observer, window_ns)),
                Condvar::new(),
            )),
            windowed: window_ns > 0,
            global_logical_time: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Bind the broker to a Unix socket `path` with the given fault model.
    ///
    /// # Errors
    /// Propagates the bind error (e.g. the path already exists).
    pub fn bind(path: &std::path::Path, model: FaultModel) -> io::Result<Self> {
        Self::bind_with(path, model, None)
    }

    /// [`Broker::bind`] with an [`Observer`] that records every linearized
    /// operation (the recording entry point).
    ///
    /// # Errors
    /// Propagates the bind error (e.g. the path already exists).
    pub fn bind_with(
        path: &std::path::Path,
        model: FaultModel,
        observer: Option<Observer>,
    ) -> io::Result<Self> {
        Self::bind_with_window(path, model, observer, 0)
    }

    /// [`Broker::bind_with`] with a windowed sequencer of width `window_ns`
    /// (the multi-host clock protocol, docs/MULTI_HOST_CLOCK_PROTOCOL.md).
    /// `window_ns == 0` selects the single-host arrival-routed broker.
    ///
    /// # Errors
    /// Propagates the bind error (e.g. the path already exists).
    pub fn bind_with_window(
        path: &std::path::Path,
        model: FaultModel,
        observer: Option<Observer>,
        window_ns: u64,
    ) -> io::Result<Self> {
        let listener = Listener::Unix(UnixListener::bind(path)?);
        Ok(Self::from_listener(listener, model, observer, window_ns))
    }

    /// Bind the broker to a TCP address for multi-host runs. Identical
    /// semantics to [`Broker::bind_with`]; only the transport differs.
    ///
    /// # Errors
    /// Propagates the bind error.
    pub fn bind_tcp(
        addr: impl ToSocketAddrs,
        model: FaultModel,
        observer: Option<Observer>,
    ) -> io::Result<Self> {
        Self::bind_tcp_window(addr, model, observer, 0)
    }

    /// [`Broker::bind_tcp`] with a windowed sequencer of width `window_ns`.
    ///
    /// # Errors
    /// Propagates the bind error.
    pub fn bind_tcp_window(
        addr: impl ToSocketAddrs,
        model: FaultModel,
        observer: Option<Observer>,
        window_ns: u64,
    ) -> io::Result<Self> {
        let listener = Listener::Tcp(TcpListener::bind(addr)?);
        Ok(Self::from_listener(listener, model, observer, window_ns))
    }

    /// The TCP address actually bound (for port-0 binds in tests).
    ///
    /// # Errors
    /// Fails when the broker is on a Unix socket.
    pub fn tcp_addr(&self) -> io::Result<std::net::SocketAddr> {
        match &self.listener {
            Listener::Tcp(l) => l.local_addr(),
            Listener::Unix(_) => Err(io::Error::other("broker is on a unix socket")),
        }
    }

    /// Accept and serve connections until the listener errors (e.g. is closed
    /// at shutdown). Each connection gets its own handler thread.
    ///
    /// # Panics
    ///
    /// Panics if the state lock is poisoned, which cannot happen: no holder
    /// performs a panicking operation.
    pub fn run(&self) {
        let mut next_id = 0u64;
        loop {
            // Accept from whichever transport this broker was bound on.
            let stream: Box<dyn StreamPair> = match &self.listener {
                Listener::Unix(l) => match l.accept() {
                    Ok((s, _)) => Box::new(s),
                    Err(_) => break,
                },
                Listener::Tcp(l) => match l.accept() {
                    Ok((s, _)) => {
                        let _ = s.set_nodelay(true);
                        Box::new(s)
                    }
                    Err(_) => break,
                },
            };
            let id = next_id;
            next_id += 1;
            {
                let mut st = self.shared.0.lock().unwrap();
                st.core.connect(id);
                st.observe(Observed::Connect { conn: id });
            }
            let shared = Arc::clone(&self.shared);
            let global_time = Arc::clone(&self.global_logical_time);
            let windowed = self.windowed;
            thread::spawn(move || handle_conn(id, stream, &shared, &global_time, windowed));
        }
    }

    /// Snapshot of `(datagrams_sent, datagrams_dropped)` for reporting.
    ///
    /// # Panics
    ///
    /// Same non-condition as [`Self::run`].
    #[must_use]
    pub fn stats(&self) -> (u64, u64) {
        self.shared.0.lock().unwrap().core.stats()
    }

    /// Largest observed |node local virtual time - broker logical time|
    /// across all operations that carried a local clock (ns).
    ///
    /// # Panics
    ///
    /// Same non-condition as [`Self::run`].
    #[must_use]
    pub fn max_skew_ns(&self) -> u64 {
        self.shared.0.lock().unwrap().max_skew_ns
    }
}

/// A duplexable byte stream: both halves of the shim connection.
trait StreamPair: Read + Write + Send {
    fn try_clone_reader(&self) -> io::Result<Box<dyn Read + Send>>;
}

impl StreamPair for std::os::unix::net::UnixStream {
    fn try_clone_reader(&self) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(self.try_clone()?))
    }
}

impl StreamPair for std::net::TcpStream {
    fn try_clone_reader(&self) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(self.try_clone()?))
    }
}

fn handle_conn(
    id: u64,
    stream: Box<dyn StreamPair>,
    shared: &Arc<(Mutex<State>, Condvar)>,
    global_time: &Arc<AtomicU64>,
    windowed: bool,
) {
    let mut reader = io::BufReader::new(stream.try_clone_reader().expect("dup stream"));
    let mut writer = stream;

    // Serve until EOF or a protocol error ends the connection.
    while let Ok(msg) = read_to_broker(&mut reader) {
        match msg {
            ToBroker::Hello { node_id } => {
                // No state change, but the identity is recorded in linear
                // order so a log names its participants. In windowed mode this
                // is also where the connection joins the sealing quorum (host
                // id 0 until per-host ids arrive with hostd, B3 — node_id is
                // globally unique so the sort key is already a total order).
                let vt = {
                    let mut st = shared.0.lock().unwrap();
                    if let Some(seq) = st.seq.as_mut() {
                        seq.register(id, 0, node_id);
                    }
                    st.observe(Observed::Hello {
                        conn: id,
                        node: node_id,
                    });
                    st.core.vt()
                };
                let _ = write_from_broker(&mut writer, &FromBroker::Ack { vt });
            }
            ToBroker::Bind { addr } => {
                let mut st = shared.0.lock().unwrap();
                st.core.bind(id, addr);
                st.observe(Observed::Bind { conn: id, addr });
                let vt = st.core.vt();
                drop(st);
                let _ = write_from_broker(&mut writer, &FromBroker::Ack { vt });
            }
            ToBroker::Send {
                src,
                dst,
                payload,
                local_vt,
            } => {
                let vt = if windowed {
                    admit_send(shared, global_time, id, src, dst, payload, local_vt)
                } else {
                    route_send(shared, global_time, id, src, dst, &payload, local_vt)
                };
                let _ = write_from_broker(&mut writer, &FromBroker::Ack { vt });
            }
            ToBroker::Recv {
                addr: _,
                blocking,
                local_vt,
            } => {
                let out = if windowed {
                    recv_windowed(shared, global_time, id, blocking, local_vt)
                } else {
                    recv_next(shared, id, blocking, local_vt)
                };
                let _ = write_from_broker(&mut writer, &out);
            }
            ToBroker::Frontier { local_vt } => {
                // Idle-guest frontier release (windowed only): advance the
                // connection's promise and try to seal. Ignored otherwise.
                let vt = if windowed {
                    declare_frontier(shared, global_time, id, local_vt)
                } else {
                    shared.0.lock().unwrap().core.vt()
                };
                let _ = write_from_broker(&mut writer, &FromBroker::Ack { vt });
            }
        }
    }

    // Cleanup on disconnect: drop the connection, unbind its addresses, and
    // wake anyone blocked (in case this was the only possible sender). In
    // windowed mode the connection also leaves the sealing quorum, which may
    // let held windows seal, so feed and notify.
    let (lock, cvar) = &**shared;
    let mut st = lock.lock().unwrap();
    st.core.disconnect(id);
    if st.seq.is_some() {
        if let Some(seq) = st.seq.as_mut() {
            seq.close(id);
        }
        st.seal_and_feed(global_time);
    }
    st.observe(Observed::Disconnect { conn: id });
    cvar.notify_all();
}

fn route_send(
    shared: &Arc<(Mutex<State>, Condvar)>,
    global_time: &Arc<AtomicU64>,
    conn: u64,
    src: VAddr,
    dst: VAddr,
    payload: &[u8],
    local_vt: u64,
) -> u64 {
    let (lock, cvar) = &**shared;
    let mut st = lock.lock().unwrap();
    st.track_skew(local_vt);
    // Single-host delivery stays latency-only (send_vt = 0), preserving
    // same-seed outcomes; the windowed multi-host broker (see
    // docs/MULTI_HOST_CLOCK_PROTOCOL.md) anchors on the sender's `local_vt`.
    let send_vt = 0;
    let (chan_seq, result) = st.core.send(src, dst, payload, send_vt);
    // Publish the logical clock's high-water mark for the orchestrator.
    // fetch_max keeps it monotonic even if callers ever race here.
    global_time.fetch_max(st.core.vt(), Ordering::Relaxed);
    st.observe(Observed::Send {
        conn,
        src,
        dst,
        chan_seq,
        send_vt,
        payload,
        result: &result,
    });
    if matches!(result, SendResult::Enqueued { .. }) {
        cvar.notify_all();
    }
    st.core.vt()
}

/// Windowed send: buffer the op in the sequencer (ordered by virtual time,
/// not arrival), seal whatever windows can now close, and wake held receives.
/// A sequencer rejection is a loud protocol violation (design doc §8, F5) —
/// logged, never silently reordered.
fn admit_send(
    shared: &Arc<(Mutex<State>, Condvar)>,
    global_time: &Arc<AtomicU64>,
    conn: u64,
    src: VAddr,
    dst: VAddr,
    payload: Vec<u8>,
    local_vt: u64,
) -> u64 {
    let (lock, cvar) = &**shared;
    let mut st = lock.lock().unwrap();
    st.track_skew(local_vt);
    let res = st
        .seq
        .as_mut()
        .map(|seq| seq.admit_send(conn, local_vt, src, dst, payload));
    match res {
        Some(Ok(_)) => {
            st.seal_and_feed(global_time);
            cvar.notify_all();
        }
        Some(Err(e)) => {
            eprintln!("weft-net: sequencer rejected send on conn {conn}: {e:?}");
        }
        None => {}
    }
    st.core.vt()
}

/// Windowed frontier declaration: an idle guest promising it will emit nothing
/// below `local_vt`, so it stops stalling window sealing (design doc §4.2).
fn declare_frontier(
    shared: &Arc<(Mutex<State>, Condvar)>,
    global_time: &Arc<AtomicU64>,
    conn: u64,
    local_vt: u64,
) -> u64 {
    let (lock, cvar) = &**shared;
    let mut st = lock.lock().unwrap();
    st.track_skew(local_vt);
    let res = st.seq.as_mut().map(|seq| seq.declare_frontier(conn, local_vt));
    match res {
        Some(Ok(())) => {
            st.seal_and_feed(global_time);
            cvar.notify_all();
        }
        Some(Err(e)) => {
            eprintln!("weft-net: sequencer rejected frontier on conn {conn}: {e:?}");
        }
        None => {}
    }
    st.core.vt()
}

/// Windowed receive: deliver only datagrams whose delivery time has fallen
/// below the sealed horizon (arrival-gated, design doc §4.3), so visibility
/// never depends on how early a send was admitted in real time. While a
/// blocking receiver waits it leaves the sealing quorum (`block`), so idle
/// receivers never stall the windows that would eventually feed them; when a
/// datagram is popped it rejoins at that delivery time (`wake`).
fn recv_windowed(
    shared: &Arc<(Mutex<State>, Condvar)>,
    global_time: &Arc<AtomicU64>,
    id: u64,
    blocking: bool,
    local_vt: u64,
) -> FromBroker {
    let (lock, cvar) = &**shared;
    let mut st = lock.lock().unwrap();
    st.track_skew(local_vt);
    let mut parked = false;
    loop {
        let horizon = st.seq.as_ref().map_or(u64::MAX, WindowSequencer::horizon);
        let result = st.core.recv_before(id, horizon);
        match &result {
            RecvResult::Delivered {
                src,
                dst,
                deliv_ns,
                payload,
                ..
            } => {
                let (src, dst, deliv_ns) = (*src, *dst, *deliv_ns);
                let out = FromBroker::Deliver {
                    src,
                    dst,
                    payload: payload.clone(),
                    vt: st.core.vt(),
                };
                if let Some(seq) = st.seq.as_mut() {
                    seq.wake(id, deliv_ns);
                }
                st.observe(Observed::Recv {
                    conn: id,
                    blocking,
                    result: &result,
                });
                return out;
            }
            RecvResult::Empty => {
                if !blocking || !st.core.is_connected(id) {
                    st.observe(Observed::Recv {
                        conn: id,
                        blocking,
                        result: &result,
                    });
                    return FromBroker::Empty { vt: st.core.vt() };
                }
                if !parked {
                    if let Some(seq) = st.seq.as_mut() {
                        seq.block(id, local_vt);
                    }
                    parked = true;
                    // Leaving the quorum can unblock a window that will feed us.
                    st.seal_and_feed(global_time);
                    cvar.notify_all();
                }
            }
        }
        st = cvar.wait(st).unwrap();
    }
}

fn recv_next(
    shared: &Arc<(Mutex<State>, Condvar)>,
    id: u64,
    blocking: bool,
    local_vt: u64,
) -> FromBroker {
    let (lock, cvar) = &**shared;
    let mut st = lock.lock().unwrap();
    st.track_skew(local_vt);
    loop {
        let result = st.core.recv(id);
        match &result {
            RecvResult::Delivered {
                src, dst, payload, ..
            } => {
                let out = FromBroker::Deliver {
                    src: *src,
                    dst: *dst,
                    payload: payload.clone(),
                    vt: st.core.vt(),
                };
                st.observe(Observed::Recv {
                    conn: id,
                    blocking,
                    result: &result,
                });
                return out;
            }
            RecvResult::Empty => {
                if !blocking || !st.core.is_connected(id) {
                    st.observe(Observed::Recv {
                        conn: id,
                        blocking,
                        result: &result,
                    });
                    return FromBroker::Empty { vt: st.core.vt() };
                }
            }
        }
        st = cvar.wait(st).unwrap();
    }
}
