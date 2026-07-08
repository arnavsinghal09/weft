//! The length-prefixed wire protocol spoken between a node's shim and the
//! broker over a Unix-domain stream socket.
//!
//! Hand-rolled binary encoding (no `serde`): a 4-byte little-endian length
//! prefix, then a 1-byte tag, then the message fields. Keeping it dependency
//! free matters because this crate is linked into the `LD_PRELOAD` shim.

use std::io::{self, Read, Write};

/// A virtual network address: an IPv4 address (host byte order) and a port.
/// Mirrors the `sockaddr_in` a target program uses, so unmodified UDP code
/// addresses peers exactly as it would on a real network.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct VAddr {
    pub ip: u32,
    pub port: u16,
}

impl VAddr {
    #[must_use]
    pub fn new(ip: u32, port: u16) -> Self {
        Self { ip, port }
    }

    /// The node index this address belongs to, by convention `127.0.0.(n+1)`.
    #[must_use]
    pub fn node_of(self) -> u32 {
        (self.ip & 0xff).wrapping_sub(1)
    }
}

impl std::fmt::Display for VAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let octets = self.ip.to_be_bytes();
        write!(
            f,
            "{}.{}.{}.{}:{}",
            octets[0], octets[1], octets[2], octets[3], self.port
        )
    }
}

/// Messages a node's shim sends to the broker.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ToBroker {
    /// First message on a connection: identify the owning node.
    Hello { node_id: u32 },
    /// Claim `addr` as this connection's receive address (a `bind`).
    Bind { addr: VAddr },
    /// Send a datagram.
    Send {
        src: VAddr,
        dst: VAddr,
        payload: Vec<u8>,
    },
    /// Ask for the next datagram delivered to `addr`. `blocking` requests that
    /// the broker hold the request until one is available (vs. answer `Empty`).
    Recv { addr: VAddr, blocking: bool },
}

/// Messages the broker sends back to a node's shim.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FromBroker {
    /// Acknowledges `Hello`/`Bind`/`Send`.
    Ack,
    /// A delivered datagram.
    Deliver {
        src: VAddr,
        dst: VAddr,
        payload: Vec<u8>,
    },
    /// No datagram was available for a non-blocking `Recv`.
    Empty,
}

// --- encoding primitives -------------------------------------------------

fn put_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn put_addr(buf: &mut Vec<u8>, a: VAddr) {
    put_u32(buf, a.ip);
    put_u16(buf, a.port);
}
fn put_bytes(buf: &mut Vec<u8>, b: &[u8]) {
    put_u32(buf, u32::try_from(b.len()).unwrap_or(u32::MAX));
    buf.extend_from_slice(b);
}

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}
impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i + n)?;
        self.i += n;
        Some(s)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn addr(&mut self) -> Option<VAddr> {
        Some(VAddr::new(self.u32()?, self.u16()?))
    }
    fn bytes(&mut self) -> Option<Vec<u8>> {
        let n = self.u32()? as usize;
        Some(self.take(n)?.to_vec())
    }
}

impl ToBroker {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        match self {
            Self::Hello { node_id } => {
                b.push(1);
                put_u32(&mut b, *node_id);
            }
            Self::Bind { addr } => {
                b.push(2);
                put_addr(&mut b, *addr);
            }
            Self::Send { src, dst, payload } => {
                b.push(3);
                put_addr(&mut b, *src);
                put_addr(&mut b, *dst);
                put_bytes(&mut b, payload);
            }
            Self::Recv { addr, blocking } => {
                b.push(4);
                put_addr(&mut b, *addr);
                b.push(u8::from(*blocking));
            }
        }
        b
    }

    #[must_use]
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let mut c = Cur { b: buf, i: 0 };
        Some(match c.take(1)?[0] {
            1 => Self::Hello { node_id: c.u32()? },
            2 => Self::Bind { addr: c.addr()? },
            3 => Self::Send {
                src: c.addr()?,
                dst: c.addr()?,
                payload: c.bytes()?,
            },
            4 => Self::Recv {
                addr: c.addr()?,
                blocking: c.take(1)?[0] != 0,
            },
            _ => return None,
        })
    }
}

impl FromBroker {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        match self {
            Self::Ack => b.push(1),
            Self::Deliver { src, dst, payload } => {
                b.push(2);
                put_addr(&mut b, *src);
                put_addr(&mut b, *dst);
                put_bytes(&mut b, payload);
            }
            Self::Empty => b.push(3),
        }
        b
    }

    #[must_use]
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let mut c = Cur { b: buf, i: 0 };
        Some(match c.take(1)?[0] {
            1 => Self::Ack,
            2 => Self::Deliver {
                src: c.addr()?,
                dst: c.addr()?,
                payload: c.bytes()?,
            },
            3 => Self::Empty,
            _ => return None,
        })
    }
}

// --- framing -------------------------------------------------------------

/// Cap on a single frame so a corrupt length can't trigger a huge allocation.
const MAX_FRAME: u32 = 16 * 1024 * 1024;

fn write_frame(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

fn read_frame(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Write a node→broker message.
///
/// # Errors
/// Propagates any underlying write error.
pub fn write_to_broker(w: &mut impl Write, m: &ToBroker) -> io::Result<()> {
    write_frame(w, &m.encode())
}

/// Read a node→broker message (broker side).
///
/// # Errors
/// Propagates I/O errors and reports malformed frames as `InvalidData`.
pub fn read_to_broker(r: &mut impl Read) -> io::Result<ToBroker> {
    let f = read_frame(r)?;
    ToBroker::decode(&f).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad ToBroker"))
}

/// Write a broker→node message.
///
/// # Errors
/// Propagates any underlying write error.
pub fn write_from_broker(w: &mut impl Write, m: &FromBroker) -> io::Result<()> {
    write_frame(w, &m.encode())
}

/// Read a broker→node message (node side).
///
/// # Errors
/// Propagates I/O errors and reports malformed frames as `InvalidData`.
pub fn read_from_broker(r: &mut impl Read) -> io::Result<FromBroker> {
    let f = read_frame(r)?;
    FromBroker::decode(&f)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad FromBroker"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_all_messages() {
        let a = VAddr::new(0x7f00_0001, 5000);
        let b = VAddr::new(0x7f00_0002, 6000);
        let tb = [
            ToBroker::Hello { node_id: 3 },
            ToBroker::Bind { addr: a },
            ToBroker::Send {
                src: a,
                dst: b,
                payload: vec![1, 2, 3, 255],
            },
            ToBroker::Recv {
                addr: a,
                blocking: true,
            },
        ];
        for m in tb {
            assert_eq!(ToBroker::decode(&m.encode()), Some(m));
        }
        let fb = [
            FromBroker::Ack,
            FromBroker::Deliver {
                src: a,
                dst: b,
                payload: vec![9, 8, 7],
            },
            FromBroker::Empty,
        ];
        for m in fb {
            assert_eq!(FromBroker::decode(&m.encode()), Some(m));
        }
    }

    #[test]
    fn framing_round_trip() {
        let mut buf = Vec::new();
        let m = ToBroker::Send {
            src: VAddr::new(1, 2),
            dst: VAddr::new(3, 4),
            payload: b"hello world".to_vec(),
        };
        write_to_broker(&mut buf, &m).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        assert_eq!(read_to_broker(&mut cur).unwrap(), m);
    }

    #[test]
    fn node_of_address() {
        assert_eq!(VAddr::new(0x7f00_0001, 0).node_of(), 0);
        assert_eq!(VAddr::new(0x7f00_0003, 0).node_of(), 2);
    }
}
