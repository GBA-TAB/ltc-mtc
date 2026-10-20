//! MIDI output through the ALSA sequencer.
//!
//! The program is the sequencer client "LTC-MTC" with two ports:
//! - "out" carries the event commands. It is connected to the default commands output, and
//!   whatever is connected to it with `aconnect` receives them too. A command with its own
//!   output goes straight to that port instead.
//! - "mtc" carries MIDI Time Code, connected to the MTC output (and any `aconnect` connection).

use alsa::seq::{Addr, ClientIter, EvCtrl, Event, EventType, PortCap, PortIter, PortSubscribe, PortType, Seq};
use serde::Serialize;
use std::ffi::CString;

#[derive(Clone, Debug, Serialize)]
pub struct MidiPort {
    /// "client:port", valid until the device is unplugged or the client restarts.
    pub addr: String,
    /// "Client name:Port name", what the config stores.
    pub name: String,
}

impl MidiPort {
    fn address(&self) -> Addr {
        let (c, p) = self.addr.split_once(':').unwrap();
        Addr { client: c.parse().unwrap(), port: p.parse().unwrap() }
    }
}

/// One of our ports and the destination it is subscribed to.
struct Link {
    port: i32,
    dest: Option<(Addr, String)>,
}

pub struct Midi {
    seq: Seq,
    client: i32,
    out: Link,
    mtc: Link,
    /// The destinations as of the last refresh.
    ports: Vec<MidiPort>,
}

fn find<'a>(ports: &'a [MidiPort], want: &str) -> Option<&'a MidiPort> {
    ports.iter().find(|p| p.name == want).or_else(|| ports.iter().find(|p| p.addr == want))
}

impl Midi {
    pub fn open() -> alsa::Result<Midi> {
        let seq = Seq::open(None, Some(alsa::Direction::Playback), false)?;
        seq.set_client_name(&CString::new("LTC-MTC").unwrap())?;
        let caps = PortCap::READ | PortCap::SUBS_READ;
        let kind = PortType::MIDI_GENERIC | PortType::APPLICATION;
        let out = seq.create_simple_port(&CString::new("out").unwrap(), caps, kind)?;
        let mtc = seq.create_simple_port(&CString::new("mtc").unwrap(), caps, kind)?;
        let client = seq.client_id()?;
        let ports = list_ports(&seq, client);
        Ok(Midi { seq, client, out: Link { port: out, dest: None }, mtc: Link { port: mtc, dest: None }, ports })
    }

    pub fn ports(&self) -> &[MidiPort] {
        &self.ports
    }

    /// Re-reads the destinations (USB devices come and go, and change address).
    pub fn refresh(&mut self) {
        self.ports = list_ports(&self.seq, self.client);
    }

    pub fn has_port(&self, want: &str) -> bool {
        find(&self.ports, want).is_some()
    }

    pub fn out_connected(&self) -> Option<&str> {
        self.out.dest.as_ref().map(|(_, n)| n.as_str())
    }

    pub fn mtc_connected(&self) -> Option<&str> {
        self.mtc.dest.as_ref().map(|(_, n)| n.as_str())
    }

    /// Connects the commands port to its default output ("" = none).
    pub fn connect_out(&mut self, want: &str) -> Result<(), String> {
        connect(&self.seq, self.client, &self.ports, &mut self.out, want)
    }

    /// Connects the MTC port to its output ("" = none).
    pub fn connect_mtc(&mut self, want: &str) -> Result<(), String> {
        connect(&self.seq, self.client, &self.ports, &mut self.mtc, want)
    }

    fn send(&self, port: i32, dest: Option<Addr>, mut ev: Event) -> Result<(), String> {
        ev.set_source(port);
        match dest {
            Some(a) => ev.set_dest(a),
            None => ev.set_subs(),
        }
        ev.set_direct();
        self.seq.event_output_direct(&mut ev).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Where a command goes: `output` "" (or the default output itself) = the "out" port's
    /// connections; another name = straight to that port.
    fn command_dest(&self, output: &str) -> Result<Option<Addr>, String> {
        if output.is_empty() || self.out_connected() == Some(output) {
            return Ok(None);
        }
        find(&self.ports, output).map(|p| Some(p.address())).ok_or_else(|| format!("MIDI output '{output}' not found"))
    }

    /// `channel` 1..=16.
    pub fn control_change(&self, output: &str, channel: u8, controller: u8, value: u8) -> Result<(), String> {
        let c = EvCtrl { channel: channel.clamp(1, 16) - 1, param: u32::from(controller & 0x7F), value: i32::from(value & 0x7F) };
        self.send(self.out.port, self.command_dest(output)?, Event::new(EventType::Controller, &c))
    }

    pub fn program_change(&self, output: &str, channel: u8, program: u8) -> Result<(), String> {
        let c = EvCtrl { channel: channel.clamp(1, 16) - 1, param: 0, value: i32::from(program & 0x7F) };
        self.send(self.out.port, self.command_dest(output)?, Event::new(EventType::Pgmchange, &c))
    }

    pub fn quarter_frame(&self, data: u8) -> Result<(), String> {
        let c = EvCtrl { channel: 0, param: 0, value: i32::from(data) };
        self.send(self.mtc.port, None, Event::new(EventType::Qframe, &c))
    }

    pub fn sysex(&self, data: &[u8]) -> Result<(), String> {
        self.send(self.mtc.port, None, Event::new_ext(EventType::Sysex, data.to_vec()))
    }
}

/// Subscribes `link`'s port to the port named `want` (or "client:port"), replacing the previous
/// subscription; "" disconnects. Called again periodically, so a USB device that comes back
/// under a new address is reconnected.
fn connect(seq: &Seq, client: i32, ports: &[MidiPort], link: &mut Link, want: &str) -> Result<(), String> {
    let me = Addr { client, port: link.port };
    let target = if want.is_empty() {
        None
    } else {
        match find(ports, want) {
            Some(p) => Some((p.address(), p.name.clone())),
            None => {
                // Gone: its subscription went with it.
                link.dest = None;
                return Err(format!("MIDI output '{want}' not found"));
            }
        }
    };
    let same = match (&link.dest, &target) {
        (Some((a, _)), Some((b, _))) => a.client == b.client && a.port == b.port,
        (None, None) => true,
        _ => false,
    };
    if same {
        return Ok(());
    }
    if let Some((old, _)) = link.dest.take() {
        let _ = seq.unsubscribe_port(me, old);
    }
    if let Some((addr, name)) = target {
        let sub = PortSubscribe::empty().map_err(|e| e.to_string())?;
        sub.set_sender(me);
        sub.set_dest(addr);
        match seq.subscribe_port(&sub) {
            Ok(()) => {}
            // Already connected (e.g. with aconnect): fine.
            Err(e) if e.errno() == libc::EBUSY => {}
            Err(e) => return Err(format!("connect to {name}: {e}")),
        }
        link.dest = Some((addr, name));
    }
    Ok(())
}

fn list_ports(seq: &Seq, me: i32) -> Vec<MidiPort> {
    let mut v = Vec::new();
    for c in ClientIter::new(seq) {
        let cid = c.get_client();
        if cid == 0 || cid == me {
            continue; // System (timer, announce) and ourselves
        }
        let cname = c.get_name().unwrap_or("?").to_string();
        for p in PortIter::new(seq, cid) {
            let caps = p.get_capability();
            if !caps.contains(PortCap::WRITE | PortCap::SUBS_WRITE) || caps.contains(PortCap::NO_EXPORT) {
                continue;
            }
            let pname = p.get_name().unwrap_or("?");
            v.push(MidiPort { addr: format!("{}:{}", cid, p.get_port()), name: format!("{cname}:{pname}") });
        }
    }
    v
}
