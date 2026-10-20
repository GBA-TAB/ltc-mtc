//! ltc-mtc: reads SMPTE LTC from an ALSA input, sends MIDI Time Code and fires MIDI
//! Control/Program Changes at given timecodes. Configured from a web page.

mod audio;
mod config;
mod engine;
mod ltc;
mod midi;
mod mtc;
mod osc;
mod tc;
mod web;

use config::{Command, Config};
use serde::Serialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tc::{Fps, Tc};

pub enum EngineMsg {
    Frame { frame: ltc::LtcFrame, at: Instant, rate: u32 },
    ConfigChanged,
    /// Fire an event now (the UI's test button).
    Fire(String),
    /// Send commands now (the UI's test button on an unsaved event).
    Send(Vec<Command>),
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct NextEvent {
    pub id: String,
    pub name: String,
    pub time: String,
    pub seconds: f64,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct LogEntry {
    pub wall: String,
    pub tc: String,
    pub event_id: String,
    pub name: String,
    pub commands: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct Status {
    pub tc: String,
    pub running: bool,
    pub reverse: bool,
    pub fps: String,
    pub fps_detected: String,
    pub speed: f64,
    pub user_bits: String,
    pub level_db: f32,
    pub frames_decoded: u64,
    pub audio_ok: bool,
    pub audio_error: Option<String>,
    pub sample_rate: u32,
    pub midi_ok: bool,
    /// Where the commands port is connected (the default output).
    pub midi_port: Option<String>,
    /// Where the MTC port is connected.
    pub mtc_port: Option<String>,
    pub midi_error: Option<String>,
    /// OSC targets: resolved address, errors, messages sent.
    pub osc: Vec<osc::TargetStatus>,
    pub next_event: Option<NextEvent>,
    pub last_fired: Option<String>,
    pub fired_seq: u64,
    pub log: VecDeque<LogEntry>,
}

pub struct App {
    pub config: Mutex<Config>,
    pub config_path: PathBuf,
    pub status: Mutex<Status>,
    /// Bumped when the audio input settings change; the capture thread reopens.
    pub audio_gen: AtomicU64,
    pub engine_tx: Mutex<Sender<EngineMsg>>,
}

impl App {
    pub fn set_audio_status(&self, ok: bool, err: Option<String>, rate: u32) {
        if let Some(e) = &err {
            eprintln!("audio: {e}");
        }
        let mut s = self.status.lock().unwrap();
        s.audio_ok = ok;
        s.audio_error = err;
        s.sample_rate = rate;
        if !ok {
            s.level_db = -120.0;
        }
    }
}

/// Real-time scheduling for the calling thread, if allowed (systemd unit or rtprio limit).
pub fn set_realtime(prio: i32) {
    unsafe {
        let p = libc::sched_param { sched_priority: prio };
        if libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &p) != 0 {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| eprintln!("note: no real-time priority (run as the systemd unit, or allow rtprio)"));
        }
    }
}

pub fn local_time() -> String {
    unsafe {
        let mut tv: libc::timeval = std::mem::zeroed();
        libc::gettimeofday(&mut tv, std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&tv.tv_sec, &mut tm);
        format!("{:02}:{:02}:{:02}.{:03}", tm.tm_hour, tm.tm_min, tm.tm_sec, tv.tv_usec / 1000)
    }
}

const USAGE: &str = "\
ltc-mtc: SMPTE LTC reader -> MIDI Time Code + MIDI events on timecode

usage:
  ltc-mtc [--config FILE] [--listen ADDR:PORT]
      run, with the web interface (default 0.0.0.0:8088,
      config ~/.config/ltc-mtc/config.json)
  ltc-mtc devices
      list ALSA capture devices and MIDI (sequencer) outputs
  ltc-mtc gen FILE.wav [--start HH:MM:SS:FF] [--fps 24|25|29.97df|30]
                       [--seconds N] [--rate HZ] [--level DBFS]
      write an LTC test signal (16-bit mono WAV)
  ltc-mtc decode FILE.wav
      print the timecode in a 16-bit PCM WAV file
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("gen") => gen(&args[1..]),
        Some("decode") => decode(&args[1..]),
        Some("devices") => devices(),
        Some("-h") | Some("--help") | Some("help") => {
            print!("{USAGE}");
            Ok(())
        }
        _ => serve(&args),
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn serve(args: &[String]) -> Result<(), String> {
    for (i, a) in args.iter().enumerate() {
        if a.starts_with("--") && !["--config", "--listen"].contains(&a.as_str()) {
            return Err(format!("unknown option {a}\n\n{USAGE}"));
        }
        if !a.starts_with("--") && (i == 0 || !args[i - 1].starts_with("--")) {
            return Err(format!("unexpected argument {a}\n\n{USAGE}"));
        }
    }
    let config_path = match opt(args, "--config") {
        Some(p) => PathBuf::from(p),
        None => {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .unwrap_or_else(|| PathBuf::from("."));
            base.join("ltc-mtc").join("config.json")
        }
    };
    let listen = opt(args, "--listen").unwrap_or("0.0.0.0:8088").to_string();
    let mut config = Config::load(&config_path)?;
    if !config_path.exists() {
        // "default" often has no capture under a system service (desktop sound servers run
        // per user), so start on the first card.
        if let Some(dev) = audio::first_card_input() {
            config.audio_device = dev;
        }
    }
    eprintln!("config: {} ({} events)", config_path.display(), config.events.len());
    let (tx, rx) = channel();
    let app = Arc::new(App {
        config: Mutex::new(config),
        config_path,
        status: Mutex::new(Status { tc: "--:--:--:--".into(), level_db: -120.0, ..Default::default() }),
        audio_gen: AtomicU64::new(0),
        engine_tx: Mutex::new(tx),
    });
    {
        let a = app.clone();
        std::thread::Builder::new().name("engine".into()).spawn(move || engine::run(a, rx)).unwrap();
    }
    {
        let a = app.clone();
        std::thread::Builder::new().name("audio".into()).spawn(move || audio::run(a)).unwrap();
    }
    web::run(app, &listen)
}

fn devices() -> Result<(), String> {
    println!("Audio inputs (ALSA PCM):");
    for d in audio::list_inputs() {
        println!("  {:<40} {}", d.name, d.desc);
    }
    println!("\nMIDI outputs (ALSA sequencer; ltc-mtc's own ports are LTC-MTC:out and LTC-MTC:mtc):");
    let m = midi::Midi::open().map_err(|e| format!("ALSA sequencer: {e}"))?;
    for p in m.ports() {
        println!("  {:<8} {}", p.addr, p.name);
    }
    Ok(())
}

fn gen(args: &[String]) -> Result<(), String> {
    let path = args.first().filter(|a| !a.starts_with("--")).ok_or(USAGE)?;
    let fps = Fps::parse(opt(args, "--fps").unwrap_or("25")).ok_or("bad --fps")?;
    let mut tc = Tc::parse(opt(args, "--start").unwrap_or("00:59:50:00")).ok_or("bad --start")?;
    if !tc.valid_for(fps) {
        return Err(format!("{tc} does not exist at {} fps", fps.name()));
    }
    let seconds: f64 = opt(args, "--seconds").unwrap_or("30").parse().map_err(|_| "bad --seconds")?;
    let rate: u32 = opt(args, "--rate").unwrap_or("48000").parse().map_err(|_| "bad --rate")?;
    let level_db: f32 = opt(args, "--level").unwrap_or("-10").parse().map_err(|_| "bad --level")?;
    let mut enc = ltc::LtcEncoder::new(rate, fps, 10f32.powf(level_db / 20.0));
    let mut buf = Vec::new();
    let frames = (seconds * fps.real()).round() as u64;
    for _ in 0..frames {
        enc.frame(tc, 1.0, false, &mut buf);
        tc = tc.next(fps);
    }
    ltc::write_wav(path, rate, &buf).map_err(|e| format!("{path}: {e}"))?;
    println!("{path}: {frames} frames at {} fps, {rate} Hz, {level_db} dBFS", fps.name());
    Ok(())
}

fn decode(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or(USAGE)?;
    let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    // Minimal RIFF reader: 16-bit PCM, any channel count, first channel decoded.
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let (mut rate, mut chans, mut bits, mut pcm) = (0u32, 0usize, 0u16, None);
    let mut i = 12;
    while i + 8 <= data.len() {
        let id = &data[i..i + 4];
        let len = u32::from_le_bytes(data[i + 4..i + 8].try_into().unwrap()) as usize;
        let body = &data[i + 8..(i + 8 + len).min(data.len())];
        if id == b"fmt " && body.len() >= 16 {
            chans = u16::from_le_bytes([body[2], body[3]]) as usize;
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            pcm = Some(body);
        }
        i += 8 + len + (len & 1);
    }
    let pcm = pcm.ok_or("no data chunk")?;
    if bits != 16 || chans == 0 {
        return Err(format!("only 16-bit PCM is supported ({bits} bit)"));
    }
    let samples: Vec<f32> = pcm
        .chunks_exact(2 * chans)
        .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0)
        .collect();
    let mut dec = ltc::LtcDecoder::new(rate);
    let mut out = Vec::new();
    dec.process(&samples, &mut out);
    for f in &out {
        let fps = if f.drop_frame { Fps::F2997Df } else { Fps::F30 };
        println!(
            "{:>10.4} s  {}{}  {:.1} fps  user {:08X}",
            f.end_pos / f64::from(rate),
            f.tc.display(fps),
            if f.reverse { "  reverse" } else { "" },
            f64::from(rate) / f.frame_samples,
            f.user_bits
        );
    }
    eprintln!("{} frames", out.len());
    Ok(())
}
