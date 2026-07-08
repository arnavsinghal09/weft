//! Violation reports: everything needed to go from "the report" to
//! "reproducing and debugging the failure" with no further archaeology —
//! the run's exact inputs (seed, network spec, log file), the violation's
//! anchor on the logical timeline, the surrounding event window, and the
//! literal replay command.

use std::path::Path;

use crate::invariant::ViolationRecord;
use crate::log::{Event, Log, RecvOutcome, SendOutcome};

/// How many events of context to show on each side of the violation.
const WINDOW: u64 = 4;

/// Render one event as a single human-readable line.
fn event_line(e: &Event) -> String {
    match e {
        Event::Connect { conn } => format!("conn {conn} connected"),
        Event::Hello { conn, node } => format!("conn {conn} is node {node}"),
        Event::Bind { conn, addr } => format!("conn {conn} bound {addr}"),
        Event::Send {
            conn,
            src,
            dst,
            chan_seq,
            outcome,
            ..
        } => {
            let fate = match outcome {
                SendOutcome::Dropped => "DROPPED by fault model".to_string(),
                SendOutcome::NoReceiver => "discarded (no receiver)".to_string(),
                SendOutcome::Enqueued { deliv_ns, tie, .. } => {
                    format!("enqueued, delivery at vt {deliv_ns}ns (tie {tie})")
                }
            };
            format!("conn {conn} send {src} → {dst} seq {chan_seq}: {fate}")
        }
        Event::Recv {
            conn,
            blocking,
            outcome,
        } => match outcome {
            RecvOutcome::Empty => format!("conn {conn} recv: empty"),
            RecvOutcome::Delivered {
                src,
                dst,
                deliv_ns,
                tie,
                ..
            } => format!(
                "conn {conn} recv{}: delivered {src} → {dst} (sent-at-vt {deliv_ns}ns, tie {tie})",
                if *blocking { " (blocking)" } else { "" }
            ),
        },
        Event::Disconnect { conn } => format!("conn {conn} disconnected"),
        Event::Violation { invariant, message } => {
            format!("VIOLATION [{invariant}]: {message}")
        }
        Event::End {
            events,
            sent,
            dropped,
        } => {
            format!("end of run: {events} events, {sent} sent, {dropped} dropped")
        }
    }
}

/// Render a full report for one violation against its log.
///
/// `log_path` is where the log lives on disk (shown in the reproduce
/// section); pass the path the reader will actually have.
#[must_use]
pub fn render(v: &ViolationRecord, log: &Log, log_path: &Path) -> String {
    let mut out = String::new();
    let push = |out: &mut String, s: &str| {
        out.push_str(s);
        out.push('\n');
    };

    push(
        &mut out,
        "================ WEFT INVARIANT VIOLATION ================",
    );
    push(&mut out, &format!("invariant : {}", v.invariant));
    push(
        &mut out,
        &format!(
            "where     : op {} of {}, virtual time {} ns",
            v.op,
            log.records.len(),
            v.vt
        ),
    );
    push(&mut out, &format!("what      : {}", v.message));
    push(&mut out, &format!("event     : {}", event_line(&v.event)));
    push(&mut out, "");
    push(&mut out, "run inputs (sufficient for exact reproduction):");
    push(&mut out, &format!("  seed        : {}", log.header.seed));
    push(
        &mut out,
        &format!(
            "  net spec    : {}",
            if log.header.net.is_empty() {
                "(reliable)"
            } else {
                &log.header.net
            }
        ),
    );
    push(&mut out, &format!("  log file    : {}", log_path.display()));
    push(
        &mut out,
        &format!(
            "  log format  : {} v{}",
            log.header.format, log.header.version
        ),
    );
    push(
        &mut out,
        &format!("  stream hash : {:016x}", log.stream_digest()),
    );
    if let Some(label) = &log.header.meta.label {
        push(&mut out, &format!("  label       : {label}"));
    }
    push(&mut out, "");
    push(&mut out, "reproduce:");
    push(
        &mut out,
        &format!(
            "  weft replay {}            # re-executes to the identical stream hash",
            log_path.display()
        ),
    );
    push(
        &mut out,
        &format!(
            "  weft replay {} --until {}  # stops right after the violating op",
            log_path.display(),
            v.op
        ),
    );
    push(&mut out, "");

    let lo = v.op.saturating_sub(WINDOW);
    let hi = (v.op + WINDOW).min(log.records.len().saturating_sub(1) as u64);
    push(&mut out, &format!("event window (op {lo}..={hi}):"));
    for r in &log.records {
        if r.op < lo || r.op > hi {
            continue;
        }
        let marker = if r.op == v.op { ">>" } else { "  " };
        push(
            &mut out,
            &format!(
                "{marker} op {:>4}  vt {:>12}ns  {}",
                r.op,
                r.vt,
                event_line(&r.e)
            ),
        );
    }
    push(
        &mut out,
        "===========================================================",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invariant::{Monitor, PerChannelFifo};
    use crate::log::{Addr, Header, LogWriter, Meta, FORMAT, VERSION};

    #[test]
    fn report_contains_everything_needed_to_reproduce() {
        let header = Header {
            format: FORMAT.into(),
            version: VERSION,
            seed: 3,
            net: "latency=uniform:1000-100000".into(),
            meta: Meta {
                label: Some("demo".into()),
                ..Meta::default()
            },
        };
        let a = Addr {
            ip: 0x7f00_0001,
            port: 100,
        };
        let b = Addr {
            ip: 0x7f00_0002,
            port: 200,
        };
        let events = [
            Event::Send {
                conn: 1,
                src: b,
                dst: a,
                chan_seq: 0,
                payload: "00".into(),
                outcome: SendOutcome::Enqueued {
                    to_conn: 0,
                    deliv_ns: 90_000,
                    tie: 0,
                },
            },
            Event::Send {
                conn: 1,
                src: b,
                dst: a,
                chan_seq: 1,
                payload: "01".into(),
                outcome: SendOutcome::Enqueued {
                    to_conn: 0,
                    deliv_ns: 2_000,
                    tie: 1,
                },
            },
            Event::Recv {
                conn: 0,
                blocking: false,
                outcome: RecvOutcome::Delivered {
                    src: b,
                    dst: a,
                    deliv_ns: 2_000,
                    tie: 1,
                    payload: "01".into(),
                },
            },
            Event::Recv {
                conn: 0,
                blocking: false,
                outcome: RecvOutcome::Delivered {
                    src: b,
                    dst: a,
                    deliv_ns: 90_000,
                    tie: 0,
                    payload: "00".into(),
                },
            },
        ];
        let mut buf = Vec::new();
        let mut w = LogWriter::new(&mut buf, &header).unwrap();
        let mut mon = Monitor::new();
        mon.register(Box::new(PerChannelFifo::new()));
        for (i, e) in events.iter().enumerate() {
            let op = w.append(i as u64, e.clone()).unwrap();
            mon.observe(op, i as u64, e);
        }
        w.finish(4).unwrap();

        let log = crate::log::Log::from_reader(buf.as_slice()).unwrap();
        let v = &mon.violations()[0];
        let text = render(v, &log, Path::new("/tmp/demo.weftlog"));

        // The reader must find: what, where on the timeline, and how to rerun.
        assert!(text.contains("per-channel-fifo"));
        assert!(text.contains("op 3"));
        assert!(text.contains("seed        : 3"));
        assert!(text.contains("latency=uniform:1000-100000"));
        assert!(text.contains("weft replay /tmp/demo.weftlog"));
        assert!(text.contains("--until 3"));
        assert!(text.contains(">> op    3"));
    }
}
