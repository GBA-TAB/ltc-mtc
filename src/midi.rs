//! MIDI output through the ALSA sequencer.
//!
//! The program has one output port, "LTC-MTC:out". Events go to every subscriber, so the port
//! chosen in the UI and any connection made with `aconnect` both receive them.

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

pub struct Midi {
    seq: Seq,
    port: i32,
    me: Addr,
    dest: Option<(Addr, String)>,
}

impl Midi {
    pub fn open() -> alsa::Result<Midi> {
        let seq = Seq::open(None, Some(alsa::Direction::Playback), false)?;
        seq.set_client_name(&CString::new("LTC-MTC").unwrap())?;
        let port = seq.create_simple_port(
            &CString::new("out").unwrap(),
            PortCap::READ | PortCap::SUBS_READ,
            PortType::MIDI_GENERIC | PortType::APPLICATION,
        )?;
        let client = seq.client_id()?;
        Ok(Midi { seq, port, me: Addr { client, port }, dest: None })
    }

    pub fn connected(&self) -> Option<&str> {
        self.dest.as_ref().map(|(_, n)| n.as_str())
    }

    /// Connects the output to the port with this name (or "client:port"), replacing the
    /// previous connection; `None` disconnects. Called again periodically, so a USB device
    /// that comes back (under a new address) is reconnected.
    pub fn connect(&mut self, want: Option<&str>) -> Result<(), String> {
        let target = match want {
            Some(w) if !w.is_empty() => {
                let ports = list_ports(&self.seq, self.me.client);
                match ports.iter().find(|p| p.name == w).or_else(|| ports.iter().find(|p| p.addr == w)) {
                    Some(p) => {
                        let (c, pt) = p.addr.split_once(':').unwrap();
                        Some((Addr { client: c.parse().unwrap(), port: pt.parse().unwrap() }, p.name.clone()))
                    }
                    None => {
                        // Gone: its subscription went with it.
                        self.dest = None;
                        return Err(format!("MIDI output '{w}' not found"));
                    }
                }
            }
            _ => None,
        };
        let same = match (&self.dest, &target) {
            (Some((a, _)), Some((b, _))) => a.client == b.client && a.port == b.port,
            (None, None) => true,
            _ => false,
        };
        if same {
            return Ok(());
        }
        if let Some((old, _)) = self.dest.take() {
            let _ = self.seq.unsubscribe_port(self.me, old);
        }
        if let Some((addr, name)) = target {
            let sub = PortSubscribe::empty().map_err(|e| e.to_string())?;
            sub.set_sender(self.me);
            sub.set_dest(addr);
            match self.seq.subscribe_port(&sub) {
                Ok(()) => {}
                // Already connected (e.g. with aconnect): fine.
                Err(e) if e.errno() == libc::EBUSY => {}
                Err(e) => return Err(format!("connect to {name}: {e}")),
            }
            self.dest = Some((addr, name));
        }
        Ok(())
    }

    pub fn ports(&self) -> Vec<MidiPort> {
        list_ports(&self.seq, self.me.client)
    }

    fn out(&self, mut ev: Event) -> Result<(), String> {
        ev.set_source(self.port);
        ev.set_subs();
        ev.set_direct();
        self.seq.event_output_direct(&mut ev).map(|_| ()).map_err(|e| e.to_string())
    }

    /// `channel` 1..=16.
    pub fn control_change(&self, channel: u8, controller: u8, value: u8) -> Result<(), String> {
        let c = EvCtrl { channel: channel.clamp(1, 16) - 1, param: u32::from(controller & 0x7F), value: i32::from(value & 0x7F) };
        self.out(Event::new(EventType::Controller, &c))
    }

    pub fn program_change(&self, channel: u8, program: u8) -> Result<(), String> {
        let c = EvCtrl { channel: channel.clamp(1, 16) - 1, param: 0, value: i32::from(program & 0x7F) };
        self.out(Event::new(EventType::Pgmchange, &c))
    }

    pub fn quarter_frame(&self, data: u8) -> Result<(), String> {
        let c = EvCtrl { channel: 0, param: 0, value: i32::from(data) };
        self.out(Event::new(EventType::Qframe, &c))
    }

    pub fn sysex(&self, data: &[u8]) -> Result<(), String> {
        self.out(Event::new_ext(EventType::Sysex, data.to_vec()))
    }
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
