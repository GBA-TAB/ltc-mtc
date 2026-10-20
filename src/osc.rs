//! OSC output: message encoding and a sender thread.
//!
//! Targets are defined once in the settings (name, host, port, protocol) and chosen per
//! command by name. Sending runs on its own thread, so a slow DNS lookup never holds up the
//! timecode engine.

use crate::App;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OscTarget {
    pub name: String,
    /// Host name or IP address.
    pub host: String,
    pub port: u16,
    /// "udp", "tcp" (SLIP framing, OSC 1.1, e.g. QLab) or "tcp-len" (int32 length
    /// prefix, OSC 1.0).
    #[serde(default = "udp")]
    pub protocol: String,
}

fn udp() -> String {
    "udp".into()
}

pub const PROTOCOLS: [&str; 3] = ["udp", "tcp", "tcp-len"];

/// A packet framed for a stream: SLIP with an END byte on both sides (OSC 1.1), or an
/// int32 big-endian length prefix (OSC 1.0).
pub fn frame(protocol: &str, packet: &[u8]) -> Vec<u8> {
    const END: u8 = 0xC0;
    const ESC: u8 = 0xDB;
    if protocol == "tcp-len" {
        let mut v = (packet.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(packet);
        return v;
    }
    let mut v = Vec::with_capacity(packet.len() + 4);
    v.push(END);
    for &b in packet {
        match b {
            END => v.extend_from_slice(&[ESC, 0xDC]),
            ESC => v.extend_from_slice(&[ESC, 0xDD]),
            _ => v.push(b),
        }
    }
    v.push(END);
    v
}

/// An OSC argument; JSON {"type": "int", "value": 1}.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum OscArg {
    #[serde(rename = "int")]
    Int(i32),
    #[serde(rename = "float")]
    Float(f32),
    #[serde(rename = "string")]
    Str(String),
    /// Sent as the argument-less types T and F.
    #[serde(rename = "bool")]
    Bool(bool),
}

impl OscArg {
    pub fn describe(&self) -> String {
        match self {
            OscArg::Int(v) => v.to_string(),
            OscArg::Float(v) => format!("{v:?}"),
            OscArg::Str(s) => format!("\"{s}\""),
            OscArg::Bool(b) => b.to_string(),
        }
    }
}

/// Checks an OSC address pattern for sending: starts with '/', printable ASCII, no spaces
/// and none of the characters reserved for matching.
pub fn valid_address(a: &str) -> bool {
    a.starts_with('/') && a.bytes().all(|b| b.is_ascii_graphic() && !b"#*,?[]{}".contains(&b))
}

fn pad_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
    // At least one NUL, then up to a multiple of 4.
    buf.push(0);
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

/// Encodes one OSC message.
pub fn encode(address: &str, args: &[OscArg]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    pad_str(&mut buf, address);
    let mut tags = String::from(",");
    for a in args {
        tags.push(match a {
            OscArg::Int(_) => 'i',
            OscArg::Float(_) => 'f',
            OscArg::Str(_) => 's',
            OscArg::Bool(true) => 'T',
            OscArg::Bool(false) => 'F',
        });
    }
    pad_str(&mut buf, &tags);
    for a in args {
        match a {
            OscArg::Int(v) => buf.extend_from_slice(&v.to_be_bytes()),
            OscArg::Float(v) => buf.extend_from_slice(&v.to_be_bytes()),
            OscArg::Str(s) => pad_str(&mut buf, s),
            OscArg::Bool(_) => {}
        }
    }
    buf
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct TargetStatus {
    pub name: String,
    /// The resolved address, once known.
    pub addr: Option<String>,
    pub ok: bool,
    pub error: Option<String>,
    pub sent: u64,
}

pub enum OscMsg {
    Targets(Vec<OscTarget>),
    Send { target: String, packet: Vec<u8> },
}

pub fn spawn(app: Arc<App>) -> Sender<OscMsg> {
    let (tx, rx) = channel();
    std::thread::Builder::new().name("osc".into()).spawn(move || run(app, rx)).unwrap();
    tx
}

type Resolved = Arc<Mutex<HashMap<String, Result<SocketAddr, String>>>>;

/// Looks host names up on its own thread (a lookup can take seconds), at start, on every
/// change and every 30 s (DHCP leases, mDNS names). Senders use the latest result.
fn resolver(rx: Receiver<Vec<OscTarget>>, out: Resolved) {
    let mut targets: Vec<OscTarget> = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(t) => targets = t,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        for t in &targets {
            let r = lookup(t);
            out.lock().unwrap().insert(t.name.clone(), r);
        }
    }
}

fn lookup(t: &OscTarget) -> Result<SocketAddr, String> {
    (t.host.as_str(), t.port)
        .to_socket_addrs()
        .map_err(|e| format!("{}: {e}", t.host))?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| format!("{}: no IPv4 address", t.host))
}

struct Conn {
    target: OscTarget,
    sent: u64,
    send_error: Option<String>,
    /// TCP: the connection thread and what it reports.
    tcp: Option<(Sender<(Instant, Vec<u8>)>, Arc<Mutex<TcpState>>)>,
}

#[derive(Default)]
struct TcpState {
    connected: bool,
    sent: u64,
    error: Option<String>,
}

struct Worker {
    app: Arc<App>,
    udp: Option<UdpSocket>,
    conns: HashMap<String, Conn>,
    resolved: Resolved,
    resolver: Sender<Vec<OscTarget>>,
}

fn run(app: Arc<App>, rx: Receiver<OscMsg>) {
    let resolved: Resolved = Arc::default();
    let (rtx, rrx) = channel();
    {
        let r = resolved.clone();
        std::thread::Builder::new().name("osc-resolve".into()).spawn(move || resolver(rrx, r)).unwrap();
    }
    let mut w = Worker { app, udp: None, conns: HashMap::new(), resolved, resolver: rtx };
    loop {
        // Short, so TCP link changes reach the interface quickly.
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(OscMsg::Targets(t)) => w.set_targets(t),
            Ok(OscMsg::Send { target, packet }) => w.send(&target, &packet),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        w.publish();
    }
}

impl Worker {
    fn set_targets(&mut self, targets: Vec<OscTarget>) {
        let mut old = std::mem::take(&mut self.conns);
        {
            let mut r = self.resolved.lock().unwrap();
            r.retain(|name, _| targets.iter().any(|t| &t.name == name));
            for t in &targets {
                let same = old.get(&t.name).is_some_and(|c| c.target == *t);
                if !same {
                    r.remove(&t.name);
                }
                // An IP address needs no lookup: usable at once.
                if let Ok(ip) = t.host.parse::<IpAddr>() {
                    r.insert(t.name.clone(), Ok(SocketAddr::new(ip, t.port)));
                }
            }
        }
        for t in &targets {
            let conn = match old.remove(&t.name) {
                Some(c) if c.target == *t => c,
                _ => {
                    // Dropping the old Conn ends its TCP thread, which closes the connection.
                    let tcp = (t.protocol != "udp").then(|| {
                        let (tx, rx) = channel();
                        let state = Arc::new(Mutex::new(TcpState::default()));
                        let (tt, st, rs) = (t.clone(), state.clone(), self.resolved.clone());
                        std::thread::Builder::new()
                            .name(format!("osc-tcp-{}", t.name))
                            .spawn(move || tcp_link(tt, rx, st, rs))
                            .unwrap();
                        (tx, state)
                    });
                    Conn { target: t.clone(), sent: 0, send_error: None, tcp }
                }
            };
            self.conns.insert(t.name.clone(), conn);
        }
        let _ = self.resolver.send(targets);
    }

    fn send(&mut self, target: &str, packet: &[u8]) {
        let Some(c) = self.conns.get_mut(target) else {
            eprintln!("osc: unknown target '{target}'");
            return;
        };
        if let Some((tx, _)) = &c.tcp {
            let _ = tx.send((Instant::now(), frame(&c.target.protocol, packet)));
            return;
        }
        let addr = self.resolved.lock().unwrap().get(target).cloned();
        let res = match addr {
            None => Err(format!("{}: still looking up the name", c.target.host)),
            Some(Err(e)) => Err(e),
            Some(Ok(addr)) => {
                if self.udp.is_none() {
                    match UdpSocket::bind("0.0.0.0:0") {
                        Ok(u) => self.udp = Some(u),
                        Err(e) => return c.fail(format!("UDP socket: {e}")),
                    }
                }
                self.udp.as_ref().unwrap().send_to(packet, addr).map(|_| ()).map_err(|e| format!("send to {addr}: {e}"))
            }
        };
        match res {
            Ok(()) => {
                c.sent += 1;
                c.send_error = None;
            }
            Err(e) => c.fail(e),
        }
    }

    fn publish(&self) {
        let r = self.resolved.lock().unwrap();
        let mut list: Vec<TargetStatus> = self
            .conns
            .values()
            .map(|c| {
                let (addr, lookup_err) = match r.get(&c.target.name) {
                    Some(Ok(a)) => (Some(a.to_string()), None),
                    Some(Err(e)) => (None, Some(e.clone())),
                    None => (None, None),
                };
                if let Some((_, st)) = &c.tcp {
                    let st = st.lock().unwrap();
                    let error = st.error.clone().or(lookup_err).or_else(|| (!st.connected).then(|| "not connected".to_string()));
                    return TargetStatus { name: c.target.name.clone(), addr, ok: error.is_none(), error, sent: st.sent };
                }
                let error = c.send_error.clone().or(lookup_err);
                TargetStatus { name: c.target.name.clone(), addr, ok: error.is_none(), error, sent: c.sent }
            })
            .collect();
        drop(r);
        list.sort_by(|a, b| a.name.cmp(&b.name));
        self.app.status.lock().unwrap().osc = list;
    }
}

/// One TCP target: keeps a connection open (reconnecting every 2 s while the target is
/// down), writes the framed packets, and discards what the target sends back. A packet more
/// than a second old (the connection was down) is dropped rather than sent late.
fn tcp_link(t: OscTarget, rx: Receiver<(Instant, Vec<u8>)>, state: Arc<Mutex<TcpState>>, resolved: Resolved) {
    let mut stream: Option<TcpStream> = None;
    let mut next_try = Instant::now();
    let set = |connected: bool, error: Option<String>| {
        let mut s = state.lock().unwrap();
        s.connected = connected;
        s.error = error;
    };
    let connect = |next_try: &mut Instant| -> Option<TcpStream> {
        *next_try = Instant::now() + Duration::from_secs(2);
        let addr = match resolved.lock().unwrap().get(&t.name).cloned() {
            Some(Ok(a)) => a,
            _ => return None, // the status shows the lookup error
        };
        match TcpStream::connect_timeout(&addr, Duration::from_millis(1000)) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                let _ = s.set_write_timeout(Some(Duration::from_millis(500)));
                eprintln!("osc {}: connected to {addr}", t.name);
                set(true, None);
                Some(s)
            }
            Err(e) => {
                set(false, Some(format!("connect to {addr}: {e}")));
                None
            }
        }
    };
    let mut junk = [0u8; 2048];
    loop {
        if stream.is_none() && Instant::now() >= next_try {
            stream = connect(&mut next_try);
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok((at, bytes)) => {
                if stream.is_none() {
                    stream = connect(&mut next_try);
                }
                let Some(s) = stream.as_mut() else { continue };
                if at.elapsed() > Duration::from_secs(1) {
                    set(true, Some("a cue was dropped: more than 1 s late".into()));
                    continue;
                }
                match s.write_all(&bytes) {
                    Ok(()) => {
                        let mut st = state.lock().unwrap();
                        st.sent += 1;
                        st.error = None;
                    }
                    Err(e) => {
                        // Typically the target restarted: one new connection, one retry.
                        stream = connect(&mut next_try);
                        let retry = stream.as_mut().map(|s| s.write_all(&bytes));
                        match retry {
                            Some(Ok(())) => state.lock().unwrap().sent += 1,
                            _ => {
                                stream = None;
                                set(false, Some(format!("send: {e}")));
                            }
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // Read what the target sent (replies), and notice when it closed the connection.
        if let Some(s) = stream.as_mut() {
            let _ = s.set_nonblocking(true);
            let closed = loop {
                match s.read(&mut junk) {
                    Ok(0) => break true,
                    Ok(_) => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break false,
                    Err(_) => break true,
                }
            };
            let _ = s.set_nonblocking(false);
            if closed {
                eprintln!("osc {}: connection closed by the target", t.name);
                stream = None;
                set(false, Some("connection closed by the target".into()));
                next_try = Instant::now() + Duration::from_millis(500);
            }
        }
    }
}

impl Conn {
    fn fail(&mut self, e: String) {
        eprintln!("osc {}: {e}", self.target.name);
        self.send_error = Some(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_the_spec() {
        // The example from the OSC 1.0 specification.
        let b = encode("/oscillator/4/frequency", &[OscArg::Float(440.0)]);
        let expect: Vec<u8> = [
            &b"/oscillator/4/frequency\0"[..],
            &b",f\0\0"[..],
            &[0x43, 0xdc, 0x00, 0x00][..],
        ]
        .concat();
        assert_eq!(b, expect);
        let b = encode("/foo", &[OscArg::Int(1000), OscArg::Int(-1), OscArg::Str("hello".into()), OscArg::Float(1.234), OscArg::Float(5.678)]);
        assert_eq!(&b[..8], b"/foo\0\0\0\0");
        assert_eq!(&b[8..16], b",iisff\0\0");
        assert_eq!(&b[16..20], &[0, 0, 0x03, 0xe8]);
        assert_eq!(&b[20..24], &[0xff, 0xff, 0xff, 0xff]);
        assert_eq!(&b[24..32], b"hello\0\0\0");
        assert_eq!(&b[32..36], &[0x3f, 0x9d, 0xf3, 0xb6]);
        assert_eq!(b.len(), 40);
        // Booleans have a type tag and no data; no arguments still send the "," tag.
        assert_eq!(encode("/go", &[OscArg::Bool(true), OscArg::Bool(false)]), b"/go\0,TF\0".to_vec());
        assert_eq!(encode("/go", &[]), b"/go\0,\0\0\0".to_vec());
    }

    #[test]
    fn framing() {
        assert_eq!(frame("tcp-len", b"/a\0\0"), vec![0, 0, 0, 4, b'/', b'a', 0, 0]);
        assert_eq!(frame("tcp", &[1, 0xC0, 2, 0xDB, 3]), vec![0xC0, 1, 0xDB, 0xDC, 2, 0xDB, 0xDD, 3, 0xC0]);
    }

    /// A real TCP round trip through the connection thread, for both framings.
    #[test]
    fn tcp_link_sends() {
        for proto in ["tcp", "tcp-len"] {
            let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = srv.local_addr().unwrap();
            let t = OscTarget { name: "t".into(), host: "127.0.0.1".into(), port: addr.port(), protocol: proto.into() };
            let resolved: Resolved = Arc::default();
            resolved.lock().unwrap().insert("t".into(), Ok(addr));
            let state = Arc::new(Mutex::new(TcpState::default()));
            let (tx, rx) = channel();
            let (st, rs, tt) = (state.clone(), resolved.clone(), t.clone());
            let h = std::thread::spawn(move || tcp_link(tt, rx, st, rs));
            let (mut conn, _) = srv.accept().unwrap();
            let packet = encode("/cue/1/go", &[OscArg::Int(5)]);
            tx.send((Instant::now(), frame(proto, &packet))).unwrap();
            let want = frame(proto, &packet);
            let mut got = vec![0u8; want.len()];
            conn.read_exact(&mut got).unwrap();
            assert_eq!(got, want, "{proto}");
            // Replies are read and discarded without disturbing the link.
            conn.write_all(b"reply").unwrap();
            tx.send((Instant::now(), frame(proto, &packet))).unwrap();
            conn.read_exact(&mut got).unwrap();
            assert_eq!(got, want);
            std::thread::sleep(Duration::from_millis(300));
            assert!(state.lock().unwrap().connected);
            assert_eq!(state.lock().unwrap().sent, 2);
            drop(tx);
            h.join().unwrap();
        }
    }

    #[test]
    fn addresses() {
        assert!(valid_address("/cue/12/go"));
        assert!(!valid_address("cue/12"));
        assert!(!valid_address("/cue 12"));
        assert!(!valid_address("/cue/*"));
    }

    #[test]
    fn args_json() {
        let a: Vec<OscArg> = serde_json::from_str(r#"[{"type":"int","value":3},{"type":"float","value":0.5},
            {"type":"string","value":"x"},{"type":"bool","value":true}]"#).unwrap();
        assert_eq!(a, vec![OscArg::Int(3), OscArg::Float(0.5), OscArg::Str("x".into()), OscArg::Bool(true)]);
    }
}
