//! UDP socket interposition: datagrams go to the Weft broker, not the kernel.
//!
//! Scope (see docs/network-model.md): `AF_INET` + `SOCK_DGRAM` sockets used
//! via `bind`/`sendto`/`recvfrom`. When `WEFT_BROKER` names a broker socket,
//! `socket()` returns a Unix-stream connection to the broker instead of a real
//! UDP socket; `sendto`/`recvfrom` speak the [`weft_net::wire`] protocol over
//! it. Everything else (TCP, `AF_INET6`, `connect`+`send`) passes through and
//! is out of simulation scope for now.
//!
//! # Scheduler integration (the determinism-critical part)
//!
//! A broker round-trip (request + reply) is performed while *holding* the
//! Phase 2 scheduler token, so it is one atomic step in the deterministic
//! schedule. Managed threads therefore never park inside the broker: a
//! `recvfrom` with nothing pending gets `Empty` back and then calls the
//! scheduler's `yield_now` — waiting for a datagram is exactly a Phase 2
//! yield point, and *which* thread gets to produce that datagram next is
//! chosen deterministically from the seed. Unmanaged threads (single-threaded
//! processes, cross-process nodes) fall back to a blocking broker `Recv`;
//! message *content and per-channel fate* stay deterministic, but cross-
//! process interleaving is not unified — the documented Phase 3 limit.

// The only panic reachable from these hooks is `Mutex::lock().unwrap()` on
// poisoning, which cannot happen: no critical section here performs a
// panicking operation. Stated once instead of on every hook.
#![allow(clippy::missing_panics_doc)]

use core::ffi::c_void;
use core::sync::atomic::{AtomicU16, Ordering};
use std::io::{self, Read, Write};
use std::os::unix::io::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, OnceLock};

use libc::{c_int, size_t, sockaddr, socklen_t, ssize_t};

use weft_net::wire::{read_from_broker, write_to_broker, FromBroker, ToBroker, VAddr};

use crate::real::real;
use crate::sched::{current_tid, is_reentrant, Reentrancy};
use crate::state::{shim, Shim};
use crate::trace::shim_trace;

/// Borrowed raw-fd I/O for the broker connection. Deliberately does **not**
/// own the fd: the target closes it through the interposed `close(2)` like
/// any other descriptor, so ownership must stay with the target (a `UnixStream`
/// in the table would close it again on drop — a reused-fd hazard).
struct RawSock(c_int);

impl Read for RawSock {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: valid fd and buffer; recv on a stream socket.
        let n = unsafe { libc::recv(self.0, buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            #[allow(clippy::cast_sign_loss)] // negative handled above
            Ok(n as usize)
        }
    }
}

impl Write for RawSock {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: valid fd and buffer; send on a stream socket.
        let n = unsafe { libc::send(self.0, buf.as_ptr().cast(), buf.len(), libc::MSG_NOSIGNAL) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            #[allow(clippy::cast_sign_loss)] // negative handled above
            Ok(n as usize)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One simulated socket: the broker-connection fd plus its bound address.
/// The `Mutex` keeps a request/reply round-trip atomic per socket even if the
/// target shares the fd across threads.
struct Sock {
    fd: c_int,
    local: Option<VAddr>,
}

type SockRef = Arc<Mutex<Sock>>;
type SockTable = Mutex<Vec<(c_int, SockRef)>>;

/// fd → simulated-socket table. Linear scan under one small lock: socket
/// counts are tiny next to datagram counts, and lookups clone the `Arc` out so
/// the table lock is never held across broker I/O.
fn table() -> &'static SockTable {
    static TABLE: OnceLock<SockTable> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(Vec::new()))
}

fn lookup(fd: c_int) -> Option<SockRef> {
    let _g = Reentrancy::enter();
    let t = table().lock().unwrap();
    t.iter().find(|(f, _)| *f == fd).map(|(_, s)| Arc::clone(s))
}

/// Forget `fd` if it was a simulated socket (called from the `close` hook).
pub fn untrack(fd: c_int) {
    if is_reentrant() {
        return;
    }
    let _g = Reentrancy::enter();
    if let Ok(mut t) = table().lock() {
        t.retain(|(f, _)| *f != fd);
    }
}

/// This process's node id (`WEFT_NODE_ID`), if network simulation is on.
fn node_id() -> Option<u32> {
    static NODE: OnceLock<Option<u32>> = OnceLock::new();
    *NODE.get_or_init(|| {
        let _g = Reentrancy::enter();
        std::env::var(weft_abi::ENV_NODE_ID).ok()?.parse().ok()
    })
}

/// The broker socket path (`WEFT_BROKER`), if network simulation is on.
fn broker_path() -> Option<&'static str> {
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    PATH.get_or_init(|| {
        let _g = Reentrancy::enter();
        std::env::var(weft_abi::ENV_BROKER).ok()
    })
    .as_deref()
}

/// The IPv4 address of node `n`, by convention `127.0.0.(n+1)`.
fn node_ip(n: u32) -> u32 {
    0x7f00_0001 + n
}

/// Round-trip one request to the broker on this socket. Called with the
/// scheduler token held (if the thread is managed), making the exchange one
/// atomic step in the deterministic schedule.
fn broker_call(sock: &mut Sock, req: &ToBroker) -> Option<FromBroker> {
    let _g = Reentrancy::enter();
    let mut io = RawSock(sock.fd);
    write_to_broker(&mut io, req).ok()?;
    read_from_broker(&mut io).ok()
}

/// Parse a `sockaddr_in` into a [`VAddr`]. `None` for null/short/non-INET.
fn parse_addr(addr: *const sockaddr, len: socklen_t) -> Option<VAddr> {
    if addr.is_null() || (len as usize) < core::mem::size_of::<libc::sockaddr_in>() {
        return None;
    }
    // SAFETY: non-null and long enough for sockaddr_in, checked above;
    // read_unaligned because the caller's buffer has no alignment guarantee.
    let sin = unsafe { addr.cast::<libc::sockaddr_in>().read_unaligned() };
    if i32::from(sin.sin_family) != libc::AF_INET {
        return None;
    }
    Some(VAddr::new(
        u32::from_be(sin.sin_addr.s_addr),
        u16::from_be(sin.sin_port),
    ))
}

/// Write a [`VAddr`] out as a `sockaddr_in` (for `recvfrom`'s source).
fn write_addr(v: VAddr, addr: *mut sockaddr, len: *mut socklen_t) {
    if addr.is_null() || len.is_null() {
        return;
    }
    // SAFETY: sockaddr_in is a plain-old-data struct, valid all-zeroes.
    let mut sin: libc::sockaddr_in = unsafe { core::mem::zeroed() };
    #[allow(clippy::cast_possible_truncation)] // AF_INET == 2 fits sa_family_t
    {
        sin.sin_family = libc::AF_INET as libc::sa_family_t;
    }
    sin.sin_port = v.port.to_be();
    sin.sin_addr.s_addr = v.ip.to_be();
    #[allow(clippy::cast_possible_truncation)] // sizeof(sockaddr_in) == 16
    let out_len = core::mem::size_of::<libc::sockaddr_in>() as socklen_t;
    // SAFETY: caller supplied the out-pointers per the recvfrom contract;
    // we copy at most min(*len, sizeof(sockaddr_in)) bytes.
    unsafe {
        let copy = (*len).min(out_len) as usize;
        core::ptr::copy_nonoverlapping(
            core::ptr::addr_of!(sin).cast::<u8>(),
            addr.cast::<u8>(),
            copy,
        );
        *len = out_len;
    }
}

/// Ephemeral-port counter for sockets that `sendto` without a prior `bind`.
static EPHEMERAL: AtomicU16 = AtomicU16::new(50_000);

/// Auto-assign a source address on first unbound `sendto`, and register it
/// with the broker so replies can route back.
fn ensure_local(s: &Shim, sock: &mut Sock, node: u32) -> Option<VAddr> {
    if let Some(l) = sock.local {
        return Some(l);
    }
    let addr = VAddr::new(node_ip(node), EPHEMERAL.fetch_add(1, Ordering::Relaxed));
    match broker_call(sock, &ToBroker::Bind { addr })? {
        FromBroker::Ack { .. } => {
            shim_trace!(s, "socket auto-bound {addr}");
            sock.local = Some(addr);
            Some(addr)
        }
        _ => None,
    }
}

fn set_errno(e: c_int) {
    // SAFETY: writing through libc's thread-local errno location.
    unsafe { *libc::__errno_location() = e };
}

/// True when network simulation should engage for this call.
fn net_active() -> Option<(&'static Shim, u32)> {
    if is_reentrant() {
        return None;
    }
    let s = shim()?;
    Some((s, node_id()?))
}

/// `socket(2)`: an `AF_INET`/`SOCK_DGRAM` socket becomes a broker connection.
///
/// # Safety
///
/// Arguments per the libc `socket(2)` contract.
#[no_mangle]
pub unsafe extern "C" fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int {
    if let Some((s, node)) = net_active() {
        // SOCK_DGRAM may carry SOCK_CLOEXEC/SOCK_NONBLOCK bits.
        if domain == libc::AF_INET && (ty & 0xf) == libc::SOCK_DGRAM {
            if let Some(path) = broker_path() {
                let _g = Reentrancy::enter();
                if let Ok(stream) = UnixStream::connect(path) {
                    // Hand fd ownership to the target: it will close it via
                    // the interposed close(2) like any other descriptor.
                    let fd = stream.into_raw_fd();
                    let mut io = RawSock(fd);
                    let hello = ToBroker::Hello { node_id: node };
                    let ok = write_to_broker(&mut io, &hello).is_ok()
                        && matches!(read_from_broker(&mut io), Ok(FromBroker::Ack { .. }));
                    if ok {
                        table()
                            .lock()
                            .unwrap()
                            .push((fd, Arc::new(Mutex::new(Sock { fd, local: None }))));
                        shim_trace!(s, "socket(AF_INET, SOCK_DGRAM) -> simulated fd {fd}");
                        return fd;
                    }
                    // SAFETY: fd came from into_raw_fd above; handshake
                    // failed, so nobody else references it.
                    unsafe { libc::close(fd) };
                }
                shim_trace!(s, "broker unreachable; real socket");
            }
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe { real!(socket: fn(c_int, c_int, c_int) -> c_int)(domain, ty, protocol) }
}

/// `bind(2)` on a simulated socket claims the address with the broker.
///
/// # Safety
///
/// Arguments per the libc `bind(2)` contract.
#[no_mangle]
pub unsafe extern "C" fn bind(fd: c_int, addr: *const sockaddr, len: socklen_t) -> c_int {
    if let Some((s, _)) = net_active() {
        if let Some(sock) = lookup(fd) {
            let Some(vaddr) = parse_addr(addr, len) else {
                set_errno(libc::EINVAL);
                return -1;
            };
            let mut sock = sock.lock().unwrap();
            if let Some(FromBroker::Ack { .. }) =
                broker_call(&mut sock, &ToBroker::Bind { addr: vaddr })
            {
                sock.local = Some(vaddr);
                shim_trace!(s, "bind(fd {fd}) -> {vaddr}");
                return 0;
            }
            set_errno(libc::EIO);
            return -1;
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe { real!(bind: fn(c_int, *const sockaddr, socklen_t) -> c_int)(fd, addr, len) }
}

/// `sendto(2)` on a simulated socket routes the datagram through the broker.
///
/// # Safety
///
/// Arguments per the libc `sendto(2)` contract.
#[allow(clippy::similar_names)] // dest/dst: POSIX parameter naming
#[no_mangle]
pub unsafe extern "C" fn sendto(
    fd: c_int,
    buf: *const c_void,
    len: size_t,
    flags: c_int,
    dest: *const sockaddr,
    dest_len: socklen_t,
) -> ssize_t {
    if let Some((s, node)) = net_active() {
        if let Some(sock) = lookup(fd) {
            let Some(dst) = parse_addr(dest, dest_len) else {
                set_errno(libc::EINVAL);
                return -1;
            };
            if buf.is_null() {
                set_errno(libc::EFAULT);
                return -1;
            }
            // SAFETY: caller guarantees buf is valid for len reads.
            let payload = unsafe { core::slice::from_raw_parts(buf.cast::<u8>(), len) };
            let mut guard = sock.lock().unwrap();
            let Some(src) = ensure_local(s, &mut guard, node) else {
                set_errno(libc::EIO);
                return -1;
            };
            let req = ToBroker::Send {
                src,
                dst,
                payload: payload.to_vec(),
                local_vt: s.clock.now_mono_ns(),
            };
            let reply = broker_call(&mut guard, &req);
            drop(guard);
            // The Ack's `vt` is deliberately NOT merged into the guest clock:
            // broker logical time is a function of cross-process arrival
            // order (OS-scheduled, re-rolled per live run), so folding it in
            // would leak that nondeterminism into guest-visible time and
            // break the same-seed guarantee. A future multi-host shim
            // transport revisits this (docs/MULTI_HOST_ARCHITECTURE.md).
            let Some(FromBroker::Ack { .. }) = reply else {
                set_errno(libc::EIO);
                return -1;
            };
            shim_trace!(s, "sendto({src} -> {dst}, {len}B)");
            // A send is a yield point: give the scheduler a chance to run the
            // receiver (or anyone else) next, deterministically.
            if s.sched_enabled && current_tid().is_some() {
                s.sched.yield_now("net_send");
            }
            #[allow(clippy::cast_possible_wrap)] // datagram sizes are tiny
            return len as ssize_t;
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe {
        real!(sendto: fn(c_int, *const c_void, size_t, c_int, *const sockaddr, socklen_t) -> ssize_t)(
            fd, buf, len, flags, dest, dest_len,
        )
    }
}

/// `recvfrom(2)` on a simulated socket asks the broker for the next delivery.
///
/// Managed threads poll and yield (see the module docs); unmanaged threads do
/// a blocking broker `Recv`. `MSG_DONTWAIT` returns `EAGAIN` when empty.
///
/// # Safety
///
/// Arguments per the libc `recvfrom(2)` contract.
#[no_mangle]
pub unsafe extern "C" fn recvfrom(
    fd: c_int,
    buf: *mut c_void,
    len: size_t,
    flags: c_int,
    src_addr: *mut sockaddr,
    src_len: *mut socklen_t,
) -> ssize_t {
    if let Some((s, _)) = net_active() {
        if let Some(sock) = lookup(fd) {
            if buf.is_null() {
                set_errno(libc::EFAULT);
                return -1;
            }
            let nonblock = flags & libc::MSG_DONTWAIT != 0;
            let managed = s.sched_enabled && current_tid().is_some();
            loop {
                let addr = sock.lock().unwrap().local;
                let Some(addr) = addr else {
                    set_errno(libc::EINVAL); // recv on an unbound socket
                    return -1;
                };
                // Managed threads must not park inside the broker (the token
                // would be lost to real-time races); they poll instead.
                let blocking = !nonblock && !managed;
                let req = ToBroker::Recv {
                    addr,
                    blocking,
                    local_vt: s.clock.now_mono_ns(),
                };
                let reply = {
                    let mut guard = sock.lock().unwrap();
                    broker_call(&mut guard, &req)
                };
                // Reply `vt` is intentionally ignored here, as in sendto:
                // merging broker logical time (linearization-order-dependent)
                // into the guest clock would break same-seed determinism.
                match reply {
                    Some(FromBroker::Deliver { src, payload, .. }) => {
                        let n = payload.len().min(len);
                        // SAFETY: caller guarantees buf valid for len writes;
                        // n <= len.
                        unsafe {
                            core::ptr::copy_nonoverlapping(payload.as_ptr(), buf.cast::<u8>(), n);
                        }
                        write_addr(src, src_addr, src_len);
                        shim_trace!(s, "recvfrom({addr}) <- {src}, {n}B");
                        #[allow(clippy::cast_possible_wrap)] // datagram sizes are tiny
                        return n as ssize_t;
                    }
                    Some(FromBroker::Empty { .. }) => {
                        if nonblock {
                            set_errno(libc::EAGAIN);
                            return -1;
                        }
                        if managed {
                            // The Phase 2 yield point: let the sender run.
                            s.sched.yield_now("net_recv_wait");
                            continue;
                        }
                        // Unmanaged and blocking=true returned Empty: broker
                        // is shutting down.
                        set_errno(libc::ECONNRESET);
                        return -1;
                    }
                    _ => {
                        set_errno(libc::EIO);
                        return -1;
                    }
                }
            }
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe {
        real!(recvfrom: fn(c_int, *mut c_void, size_t, c_int, *mut sockaddr, *mut socklen_t) -> ssize_t)(
            fd, buf, len, flags, src_addr, src_len,
        )
    }
}
