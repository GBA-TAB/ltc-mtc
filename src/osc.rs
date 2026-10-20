//! OSC output: message encoding and a sender thread.
//!
//! Targets are defined once in the settings (name, host, port, protocol) and chosen per
//! command by name. Sending runs on its own thread, so a slow DNS lookup never holds up the
//! timecode engine.

use crate::App;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OscTarget {
    pub name: String,
    /// Host name or IP address.
    pub host: String,
    pub port: u16,
    /// "udp".
    #[serde(default = "udp")]
    pub protocol: String,
}

fn udp() -> String {
    "udp".into()
}

pub const PROTOCOLS: [&str; 1] = ["udp"];

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
        match rx.recv_timeout(Duration::from_secs(1)) {
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
                _ => Conn { target: t.clone(), sent: 0, send_error: None },
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
                let error = c.send_error.clone().or(lookup_err);
                TargetStatus { name: c.target.name.clone(), addr, ok: error.is_none(), error, sent: c.sent }
            })
            .collect();
        drop(r);
        list.sort_by(|a, b| a.name.cmp(&b.name));
        self.app.status.lock().unwrap().osc = list;
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
