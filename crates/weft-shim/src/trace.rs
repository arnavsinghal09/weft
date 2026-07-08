//! Per-call tracing that is safe from inside any hook: formats into a stack
//! buffer and issues one raw `write(2)` to stderr. No allocation, no stdio
//! locks, no reentry into interposed functions.

use core::fmt::{self, Write as _};

struct StackBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> fmt::Write for StackBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let take = s.len().min(N - self.len);
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(()) // truncation is acceptable for trace lines
    }
}

fn write_fd2(bytes: &[u8]) {
    #[cfg(unix)]
    {
        let mut off = 0;
        while off < bytes.len() {
            // SAFETY: fd 2 write with an in-bounds pointer/length pair; we
            // handle short writes and ignore errors (tracing is best-effort).
            let n = unsafe { libc::write(2, bytes.as_ptr().add(off).cast(), bytes.len() - off) };
            if n <= 0 {
                return;
            }
            #[allow(clippy::cast_sign_loss)] // n > 0 checked above
            {
                off += n as usize;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = bytes;
    }
}

/// Emit one preformatted line to stderr (init-time diagnostics).
pub fn raw_stderr(s: &str) {
    write_fd2(s.as_bytes());
}

/// Emit a `[weft] ...` trace line built from `fmt::Arguments`, at most ~250
/// bytes, allocation-free.
pub fn trace_line(args: fmt::Arguments<'_>) {
    let mut b = StackBuf::<256> {
        buf: [0; 256],
        len: 0,
    };
    let _ = b.write_str("[weft] ");
    let _ = b.write_fmt(args);
    let _ = b.write_str("\n");
    write_fd2(&b.buf[..b.len]);
}

/// Trace helper for hooks: logs only when the shim is active with tracing on.
#[cfg_attr(not(target_os = "linux"), allow(unused_macros))] // hooks are Linux-only
macro_rules! shim_trace {
    ($shim:expr, $($arg:tt)*) => {
        if $shim.trace {
            $crate::trace::trace_line(format_args!($($arg)*));
        }
    };
}
#[cfg_attr(not(target_os = "linux"), allow(unused_imports))] // hooks are Linux-only
pub(crate) use shim_trace;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_buf_truncates_instead_of_overflowing() {
        let mut b = StackBuf::<8> {
            buf: [0; 8],
            len: 0,
        };
        b.write_str("0123456789").unwrap();
        assert_eq!(&b.buf[..b.len], b"01234567");
        b.write_str("more").unwrap(); // full buffer: still no panic
        assert_eq!(b.len, 8);
    }
}
