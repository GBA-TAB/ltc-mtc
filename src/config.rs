//! Settings and the event list, stored as JSON.

use crate::tc::{Fps, Tc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// ALSA PCM name, e.g. "default" or "plughw:CARD=USB,DEV=0".
    pub audio_device: String,
    /// Input channel carrying LTC, 1-based.
    pub audio_channel: u32,
    pub sample_rate: u32,
    /// Default output for the event commands: sequencer port name "Client:Port" (or
    /// "client:port"); empty = no connection (other programs can still connect to LTC-MTC:out).
    pub midi_output: String,
    pub mtc_enabled: bool,
    /// Output for MIDI Time Code; empty = no connection (LTC-MTC:mtc can still be connected).
    /// Missing in configs from before 0.2, which sent MTC to `midi_output`.
    pub mtc_output: Option<String>,
    /// "auto", "24", "25", "29.97df" or "30".
    pub fps_mode: String,
    /// Frames the reader may miss and still treat playback as continuous (events
    /// in the gap still fire).
    pub max_gap_frames: u32,
    pub events: Vec<CueEvent>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            audio_device: "default".into(),
            audio_channel: 1,
            sample_rate: 48000,
            midi_output: String::new(),
            mtc_enabled: true,
            mtc_output: None,
            fps_mode: "auto".into(),
            max_gap_frames: 4,
            events: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CueEvent {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// "HH:MM:SS:FF"
    pub time: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub commands: Vec<Command>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Command {
    /// Control Change; channel 1..16, controller and value 0..127.
    #[serde(rename = "cc")]
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
        /// Output port name; empty = the default output.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        output: String,
    },
    /// Program Change; channel 1..16, program 0..127.
    #[serde(rename = "pc")]
    ProgramChange {
        channel: u8,
        program: u8,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        output: String,
    },
}

impl Command {
    pub fn describe(&self) -> String {
        let (text, output) = match self {
            Command::ControlChange { channel, controller, value, output } => (format!("CC ch{channel} #{controller}={value}"), output),
            Command::ProgramChange { channel, program, output } => (format!("PC ch{channel} {program}"), output),
        };
        if output.is_empty() {
            text
        } else {
            format!("{text} -> {output}")
        }
    }

    pub fn output(&self) -> &str {
        match self {
            Command::ControlChange { output, .. } | Command::ProgramChange { output, .. } => output,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let mut c: Config = serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?;
                c.validate()?;
                Ok(c)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes through a temporary file, so a power cut never leaves half a file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_string_pretty(self).unwrap();
        std::fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
        if let Ok(f) = std::fs::File::open(&tmp) {
            let _ = f.sync_all();
        }
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn fixed_fps(&self) -> Option<Fps> {
        Fps::parse(&self.fps_mode)
    }

    /// Checks values and fills in missing event ids; events are kept sorted by time.
    pub fn validate(&mut self) -> Result<(), String> {
        if self.audio_channel == 0 || self.audio_channel > 64 {
            return Err("audio channel must be 1..64".into());
        }
        if !(8000..=192000).contains(&self.sample_rate) {
            return Err("sample rate must be 8000..192000".into());
        }
        if self.fps_mode != "auto" && Fps::parse(&self.fps_mode).is_none() {
            return Err(format!("unknown frame rate '{}'", self.fps_mode));
        }
        self.max_gap_frames = self.max_gap_frames.clamp(1, 30);
        if self.mtc_output.is_none() {
            self.mtc_output = Some(self.midi_output.clone());
        }
        // Existing ids are kept; only missing or duplicate ones get a new one.
        let mut taken: HashSet<String> = self.events.iter().map(|e| e.id.clone()).collect();
        let mut seen = HashSet::new();
        let mut n = 0u32;
        for e in self.events.iter_mut() {
            let tc = Tc::parse(&e.time).ok_or_else(|| format!("event '{}': bad time '{}' (HH:MM:SS:FF)", e.name, e.time))?;
            if let Some(fps) = Fps::parse(&self.fps_mode) {
                if u32::from(tc.f) >= fps.nominal() {
                    return Err(format!("event '{}': frame {} does not exist at {} fps", e.name, tc.f, fps.name()));
                }
            }
            e.time = tc.to_string();
            for c in &e.commands {
                let (ch, a, b) = match *c {
                    Command::ControlChange { channel, controller, value, .. } => (channel, controller, value),
                    Command::ProgramChange { channel, program, .. } => (channel, program, 0),
                };
                if !(1..=16).contains(&ch) || a > 127 || b > 127 {
                    return Err(format!("event '{}': {} out of range (channel 1-16, values 0-127)", e.name, c.describe()));
                }
            }
            if e.id.is_empty() || !seen.insert(e.id.clone()) {
                loop {
                    n += 1;
                    let id = format!("e{n}");
                    if taken.insert(id.clone()) {
                        seen.insert(id.clone());
                        e.id = id;
                        break;
                    }
                }
            }
        }
        self.events.sort_by_key(|e| Tc::parse(&e.time).unwrap());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str, time: &str) -> CueEvent {
        CueEvent { id: id.into(), name: String::new(), time: time.into(), enabled: true, commands: vec![] }
    }

    #[test]
    fn ids_kept_and_filled() {
        let mut c = Config { events: vec![ev("", "00:00:00:00"), ev("e1", "1:0:0:0"), ev("e1", "02:00:00:00")], ..Default::default() };
        c.validate().unwrap();
        let ids: Vec<&str> = c.events.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids[1], "e1");
        assert_eq!(c.events[1].time, "01:00:00:00");
        assert!(ids[0] != "e1" && ids[2] != "e1" && ids[0] != ids[2]);
    }

    #[test]
    fn old_config_keeps_mtc_on_its_port() {
        let mut c: Config = serde_json::from_str(r#"{"midi_output": "X:1", "events": [
            {"time": "01:00:00:00", "commands": [{"type": "cc", "channel": 1, "controller": 2, "value": 3}]}]}"#).unwrap();
        c.validate().unwrap();
        assert_eq!(c.mtc_output.as_deref(), Some("X:1"));
        assert_eq!(c.events[0].commands[0].output(), "");
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains(r#""output""#), "empty outputs are not written");
    }
}
