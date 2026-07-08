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
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::core::{Core, RecvResult, SendResult};
use crate::fault::FaultModel;
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
}

impl State {
    fn observe(&mut self, ev: Observed<'_>) {
        if let Some(obs) = &mut self.observer {
            obs(self.core.vt(), ev);
        }
    }
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
        let listener = UnixListener::bind(path)?;
        Ok(Self {
            listener,
            shared: Arc::new((
                Mutex::new(State {
                    core: Core::new(model),
                    observer,
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
            let id = id as u64;
            {
                let mut st = self.shared.0.lock().unwrap();
                st.core.connect(id);
                st.observe(Observed::Connect { conn: id });
            }
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
        self.shared.0.lock().unwrap().core.stats()
    }
}

fn handle_conn(
    id: u64,
    stream: UnixStream,
    shared: &Arc<(Mutex<State>, Condvar)>,
    global_time: &Arc<AtomicU64>,
) {
    let mut reader = io::BufReader::new(stream.try_clone().expect("dup unix stream"));
    let mut writer = stream;

    // Serve until EOF or a protocol error ends the connection.
    while let Ok(msg) = read_to_broker(&mut reader) {
        match msg {
            ToBroker::Hello { node_id } => {
                // No state change, but the identity is recorded in linear
                // order so a log names its participants.
                shared.0.lock().unwrap().observe(Observed::Hello {
                    conn: id,
                    node: node_id,
                });
                let _ = write_from_broker(&mut writer, &FromBroker::Ack);
            }
            ToBroker::Bind { addr } => {
                let mut st = shared.0.lock().unwrap();
                st.core.bind(id, addr);
                st.observe(Observed::Bind { conn: id, addr });
                drop(st);
                let _ = write_from_broker(&mut writer, &FromBroker::Ack);
            }
            ToBroker::Send { src, dst, payload } => {
                route_send(shared, global_time, id, src, dst, &payload);
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
    st.core.disconnect(id);
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
) {
    let (lock, cvar) = &**shared;
    let mut st = lock.lock().unwrap();
    let (chan_seq, result) = st.core.send(src, dst, payload);
    // Publish the logical clock's high-water mark for the orchestrator.
    // fetch_max keeps it monotonic even if callers ever race here.
    global_time.fetch_max(st.core.vt(), Ordering::Relaxed);
    st.observe(Observed::Send {
        conn,
        src,
        dst,
        chan_seq,
        payload,
        result: &result,
    });
    if matches!(result, SendResult::Enqueued { .. }) {
        cvar.notify_all();
    }
}

fn recv_next(shared: &Arc<(Mutex<State>, Condvar)>, id: u64, blocking: bool) -> FromBroker {
    let (lock, cvar) = &**shared;
    let mut st = lock.lock().unwrap();
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
                    return FromBroker::Empty;
                }
            }
        }
        st = cvar.wait(st).unwrap();
    }
}
