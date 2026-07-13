//! The live recorder: adapts the broker's [`Observed`] operations into log
//! [`Event`]s, runs in-process invariants, and appends everything (including
//! violations, as they happen) to a [`LogWriter`].
//!
//! The observer closure runs under the broker's state lock, so events land in
//! the log in exactly the linearization order — the property replay depends
//! on. Every record is flushed as written: a crashed run leaves a readable,
//! chain-verifiable prefix.

use std::path::Path;
use std::sync::{Arc, Mutex};

use weft_net::broker::{Observed, Observer};
use weft_net::core::{RecvResult, SendResult};

use crate::hash::to_hex;
use crate::invariant::{Invariant, Monitor, ViolationRecord};
use crate::log::{Event, FileSink, Header, LogError, LogWriter, RecvOutcome, SendOutcome};

struct Inner {
    writer: LogWriter<FileSink>,
    monitor: Monitor,
    /// First write error, if any (surfaced by [`Recorder::finish`]).
    error: Option<String>,
    /// Virtual time of the last observed op, for the End record.
    last_vt: u64,
}

/// A recording session: create it, install [`Recorder::observer`] on the
/// broker, run the scenario, then call [`Recorder::finish`].
pub struct Recorder {
    inner: Arc<Mutex<Inner>>,
}

fn to_event(ev: &Observed<'_>) -> Event {
    match ev {
        Observed::Connect { conn } => Event::Connect { conn: *conn },
        Observed::Hello { conn, node } => Event::Hello {
            conn: *conn,
            node: *node,
        },
        Observed::Bind { conn, addr } => Event::Bind {
            conn: *conn,
            addr: (*addr).into(),
        },
        Observed::Send {
            conn,
            src,
            dst,
            chan_seq,
            send_vt,
            payload,
            result,
        } => Event::Send {
            conn: *conn,
            src: (*src).into(),
            dst: (*dst).into(),
            chan_seq: *chan_seq,
            send_vt: *send_vt,
            payload: to_hex(payload),
            outcome: match result {
                SendResult::Dropped => SendOutcome::Dropped,
                SendResult::NoReceiver => SendOutcome::NoReceiver,
                SendResult::Enqueued {
                    to_conn,
                    deliv_ns,
                    tie,
                } => SendOutcome::Enqueued {
                    to_conn: *to_conn,
                    deliv_ns: *deliv_ns,
                    tie: *tie,
                },
            },
        },
        Observed::Recv {
            conn,
            blocking,
            result,
        } => Event::Recv {
            conn: *conn,
            blocking: *blocking,
            outcome: match result {
                RecvResult::Empty => RecvOutcome::Empty,
                RecvResult::Delivered {
                    src,
                    dst,
                    deliv_ns,
                    tie,
                    payload,
                } => RecvOutcome::Delivered {
                    src: (*src).into(),
                    dst: (*dst).into(),
                    deliv_ns: *deliv_ns,
                    tie: *tie,
                    payload: to_hex(payload),
                },
            },
        },
        Observed::Disconnect { conn } => Event::Disconnect { conn: *conn },
    }
}

impl Recorder {
    /// Open `path` for recording and register `invariants` to check live.
    ///
    /// # Errors
    /// Propagates log-creation errors.
    pub fn create(
        path: &Path,
        header: &Header,
        invariants: Vec<Box<dyn Invariant>>,
    ) -> Result<Self, LogError> {
        let writer = LogWriter::create(path, header)?;
        let mut monitor = Monitor::new();
        for inv in invariants {
            monitor.register(inv);
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                writer,
                monitor,
                error: None,
                last_vt: 0,
            })),
        })
    }

    /// The observer to install via `Broker::bind_with`. Runs under the
    /// broker's state lock; write errors are remembered and surfaced by
    /// [`Recorder::finish`] rather than panicking mid-run.
    ///
    /// # Panics
    /// The returned closure panics if the recorder lock is poisoned, which
    /// cannot happen: no holder performs a panicking operation.
    #[must_use]
    pub fn observer(&self) -> Observer {
        let inner = Arc::clone(&self.inner);
        Box::new(move |vt, ev| {
            let mut g = inner.lock().unwrap();
            if g.error.is_some() {
                return;
            }
            g.last_vt = vt;
            let event = to_event(&ev);
            let op = match g.writer.append(vt, event.clone()) {
                Ok(op) => op,
                Err(e) => {
                    g.error = Some(e.to_string());
                    return;
                }
            };
            // Run invariants and log any violation right after its trigger,
            // so the log carries the finding at its point on the timeline.
            let raised = g.monitor.observe(op, vt, &event);
            if raised > 0 {
                let start = g.monitor.violations().len() - raised;
                let new: Vec<ViolationRecord> = g.monitor.violations()[start..].to_vec();
                for v in new {
                    let ve = Event::Violation {
                        invariant: v.invariant,
                        message: v.message,
                    };
                    if let Err(e) = g.writer.append(vt, ve) {
                        g.error = Some(e.to_string());
                        return;
                    }
                }
            }
        })
    }

    /// Write the `end` record and return every violation observed.
    ///
    /// # Errors
    /// The first write error hit during recording, or one from the final
    /// flush.
    ///
    /// # Panics
    /// If the recorder lock is poisoned, which cannot happen: no holder
    /// performs a panicking operation.
    pub fn finish(self) -> Result<Vec<ViolationRecord>, String> {
        let mut g = self.inner.lock().unwrap();
        if let Some(e) = g.error.take() {
            return Err(e);
        }
        let vt = g.last_vt;
        g.writer.finish(vt).map_err(|e| e.to_string())?;
        // Complete the file here rather than relying on Drop: the broker's
        // accept thread may hold the observer's Arc past process teardown.
        g.writer.finalize().map_err(|e| e.to_string())?;
        Ok(g.monitor.violations().to_vec())
    }
}
