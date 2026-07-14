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
    /// First windowed protocol violation seen (a rejected [`SeqError`] op:
    /// non-monotone clock, late op, reconnect splice). Latched so the
    /// orchestrator can abort the run as invalid — continuing past a rejected
    /// op silently corrupts the linearization (design doc §8, F4/F5).
    violation: Option<String>,
    /// Distinct node ids that have said `Hello` (transport-level, tracked for
    /// windowed and plain brokers alike) — with `live_conns`, lets a hosting
    /// orchestrator (`--listen`) hold the broker open until every expected
    /// node has joined *and* finished, instead of dying with its local
    /// children while remote nodes still depend on it.
    nodes_hello: std::collections::HashSet<u32>,
    /// Connections accepted and not yet disconnected.
    live_conns: u64,
    /// Connections that said `Hello` (joined as a node, vs. a bare probe).
    hello_conns: std::collections::HashSet<u64>,
    /// Connections that sent a clean `Goodbye`. A windowed node connection
    /// whose stream ends without one crashed (F1) — latched as a violation.
    said_goodbye: std::collections::HashSet<u64>,
}

impl State {
    fn new(model: FaultModel, observer: Option<Observer>, window_ns: u64) -> Self {
        let lookahead = model.min_delay_ns();
        Self {
            core: Core::new(model),
            observer,
            max_skew_ns: 0,
            seq: (window_ns > 0).then(|| WindowSequencer::new(window_ns, lookahead)),
            violation: None,
            nodes_hello: std::collections::HashSet::new(),
            live_conns: 0,
            hello_conns: std::collections::HashSet::new(),
            said_goodbye: std::collections::HashSet::new(),
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
    fn from_listener(
        listener: Listener,
        model: FaultModel,
        observer: Option<Observer>,
        window_ns: u64,
    ) -> Self {
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
                st.live_conns += 1;
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

    /// Arm the windowed join barrier: no window seals until `n` distinct node
    /// ids have said `Hello` (see [`WindowSequencer::expect_nodes`]). No-op on
    /// the non-windowed broker.
    ///
    /// # Panics
    ///
    /// Same non-condition as [`Self::run`].
    pub fn expect_nodes(&self, n: u32) {
        if let Some(seq) = self.shared.0.lock().unwrap().seq.as_mut() {
            seq.expect_nodes(n);
        }
    }

    /// Whether every one of `expected` distinct node ids has said `Hello` and
    /// every accepted connection has since closed — the whole cluster joined
    /// and finished. A hosting orchestrator (`--listen`) uses this to keep the
    /// broker alive for remote nodes after its own children exit.
    ///
    /// # Panics
    ///
    /// Same non-condition as [`Self::run`].
    #[must_use]
    pub fn cluster_drained(&self, expected: u32) -> bool {
        let st = self.shared.0.lock().unwrap();
        st.nodes_hello.len() >= expected as usize && st.live_conns == 0
    }

    /// The first windowed protocol violation seen (a rejected op: non-monotone
    /// clock F5, late op, reconnect splice F4), or `None`. A violated run's
    /// linearization is corrupt; the orchestrator aborts it as invalid.
    ///
    /// # Panics
    ///
    /// Same non-condition as [`Self::run`].
    #[must_use]
    pub fn violation(&self) -> Option<String> {
        self.shared.0.lock().unwrap().violation.clone()
    }

    /// Whether the windowed cluster has reached a terminal deadlock: every live
    /// guest is blocked with no message that can wake it, nothing is buffered
    /// or in flight, yet at least one guest is still connected (a hang, not a
    /// clean shutdown). A pure function of sequencer + core state — the
    /// deterministic F6 quiescence report (docs/MULTI_HOST_CLOCK_PROTOCOL.md
    /// §8). Always `false` for the non-windowed broker.
    ///
    /// # Panics
    ///
    /// Same non-condition as [`Self::run`].
    #[must_use]
    pub fn deadlock_check(&self) -> bool {
        let st = self.shared.0.lock().unwrap();
        st.seq.as_ref().is_some_and(WindowSequencer::deadlocked) && !st.core.any_queued()
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
                    st.nodes_hello.insert(node_id);
                    st.hello_conns.insert(id);
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
                addr,
                blocking,
                local_vt,
            } => {
                let out = if windowed {
                    recv_windowed(shared, global_time, id, addr, blocking, local_vt)
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
            ToBroker::Goodbye => {
                // Fire-and-forget clean farewell (no reply): the coming EOF is
                // a normal exit, not a crash.
                shared.0.lock().unwrap().said_goodbye.insert(id);
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
    st.live_conns = st.live_conns.saturating_sub(1);
    // F1: a windowed *node* connection (said Hello) that ends without a clean
    // Goodbye crashed mid-run. What the survivors then see depends on when, in
    // real time, the crash landed — the run is invalid.
    if windowed && st.hello_conns.contains(&id) && !st.said_goodbye.contains(&id) {
        st.violation
            .get_or_insert_with(|| format!("conn {id} closed without goodbye (node crash)"));
    }
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
            st.violation
                .get_or_insert_with(|| format!("rejected send on conn {conn}: {e:?}"));
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
    let res = st
        .seq
        .as_mut()
        .map(|seq| seq.declare_frontier(conn, local_vt));
    match res {
        Some(Ok(())) => {
            st.seal_and_feed(global_time);
            cvar.notify_all();
        }
        Some(Err(e)) => {
            eprintln!("weft-net: sequencer rejected frontier on conn {conn}: {e:?}");
            st.violation
                .get_or_insert_with(|| format!("rejected frontier on conn {conn}: {e:?}"));
        }
        None => {}
    }
    st.core.vt()
}

/// Windowed receive — single-shot, never holds. The shim polls this (parking
/// as `BlockedNet` between polls) so the wait costs no scheduler entropy and a
/// sibling thread can keep running (docs/MULTI_HOST_CLOCK_PROTOCOL.md §4.2).
/// Two models, keyed on the guest's true intent:
///
/// - **Blocking recv** (`blocking`): the guest waits for a *message*. It leaves
///   the sealing quorum (`block`, contributing its reactivation bound) and the
///   broker returns the earliest sealed datagram, or `Empty` (the shim re-polls
///   until one arrives).
/// - **Non-blocking recv** (`MSG_DONTWAIT`): the guest polls at virtual time
///   `local_vt = T`. It advances its frontier to `T` (`touch_frontier`, so the
///   horizon can rise to meet it) and the broker returns the earliest datagram
///   with `deliv_vt < T` — the messages that have virtually arrived by `T`. If
///   none, it returns `Empty { vt = pop_horizon }`; the shim reads `EAGAIN`
///   once `pop_horizon >= T` (the virtual-time answer is now final) and re-polls
///   otherwise. This makes the visible set a pure function of `T`, not of how
///   far windows have sealed in real time.
fn recv_windowed(
    shared: &Arc<(Mutex<State>, Condvar)>,
    global_time: &Arc<AtomicU64>,
    id: u64,
    addr: VAddr,
    blocking: bool,
    local_vt: u64,
) -> FromBroker {
    let (lock, _cvar) = &**shared;
    let mut st = lock.lock().unwrap();
    st.track_skew(local_vt);
    // Declare the connection's frontier so sealing can advance, then seal.
    if let Some(seq) = st.seq.as_mut() {
        if blocking {
            seq.block(id, local_vt, addr);
        } else {
            seq.touch_frontier(id, local_vt);
        }
    }
    st.seal_and_feed(global_time);
    let horizon = st
        .seq
        .as_ref()
        .map_or(u64::MAX, WindowSequencer::pop_horizon);
    // Blocking: any sealed datagram. Non-blocking: only those with deliv < T.
    let bound = if blocking {
        horizon
    } else {
        horizon.min(local_vt)
    };
    let result = st.core.recv_before(id, bound);
    match &result {
        RecvResult::Delivered {
            src,
            dst,
            deliv_ns,
            payload,
            ..
        } => {
            let out = FromBroker::Deliver {
                src: *src,
                dst: *dst,
                payload: payload.clone(),
                // Windowed deliveries carry their own seed-derived delivery
                // time so the shim can merge it into the receiver's clock
                // (docs/MULTI_HOST_CLOCK_PROTOCOL.md §3).
                vt: *deliv_ns,
            };
            let deliv_ns = *deliv_ns;
            if let Some(seq) = st.seq.as_mut() {
                seq.wake(id, deliv_ns);
            }
            st.observe(Observed::Recv {
                conn: id,
                blocking,
                result: &result,
            });
            out
        }
        RecvResult::Empty => {
            st.observe(Observed::Recv {
                conn: id,
                blocking,
                result: &result,
            });
            // Carry the pop-horizon so a non-blocking poller knows when its
            // virtual-time answer is final (pop_horizon >= T).
            FromBroker::Empty { vt: horizon }
        }
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
