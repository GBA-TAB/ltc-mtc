//! ALSA capture: reads the chosen input channel and decodes LTC.

use crate::ltc::{LtcDecoder, LtcFrame};
use crate::{App, EngineMsg};
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize)]
pub struct AudioDevice {
    pub name: String,
    pub desc: String,
}

/// Capture-capable PCMs from the ALSA name hints: per card plughw (converts format, rate and
/// channels), hw and dsnoop (shares the input with other programs), plus "default", sound
/// servers and PCMs defined in asoundrc. Aliases like front: or sysdefault: are left out.
pub fn list_inputs() -> Vec<AudioDevice> {
    const SKIP: [&str; 9] = ["sysdefault", "default:", "front:", "surround", "iec958", "spdif", "hdmi", "usbstream", "dmix"];
    let mut v = Vec::new();
    if let Ok(hints) = alsa::device_name::HintIter::new_str(None, "pcm") {
        for h in hints {
            if h.direction == Some(Direction::Playback) {
                continue;
            }
            let Some(name) = h.name else { continue };
            if name == "null" || SKIP.iter().any(|p| name.starts_with(p)) {
                continue;
            }
            let desc = h.desc.unwrap_or_default().replace('\n', " - ");
            v.push(AudioDevice { name, desc });
        }
    }
    v
}

/// For a first start: the first real card's plughw device (a USB interface on a Pi).
pub fn first_card_input() -> Option<String> {
    list_inputs()
        .into_iter()
        .map(|d| d.name)
        .find(|n| n.starts_with("plughw:CARD=") && !n.contains("Loopback"))
}

enum Buf {
    I16(Vec<i16>),
    I32(Vec<i32>),
    F32(Vec<f32>),
}

struct Input {
    pcm: PCM,
    rate: u32,
    channels: usize,
    buf: Buf,
}

fn open(device: &str, want_channel: u32, rate: u32) -> Result<Input, String> {
    let pcm = PCM::new(device, Direction::Capture, false).map_err(|e| format!("open {device}: {e}"))?;
    let (rate, channels, format) = {
        let hwp = HwParams::any(&pcm).map_err(|e| e.to_string())?;
        hwp.set_access(Access::RWInterleaved).map_err(|e| format!("access: {e}"))?;
        let format = [Format::s16(), Format::s32(), Format::float()]
            .into_iter()
            .find(|f| hwp.set_format(*f).is_ok())
            .ok_or("no supported sample format (use a plughw: device)")?;
        let chans = hwp.set_channels_near(want_channel.max(1)).map_err(|e| format!("channels: {e}"))?;
        if chans < want_channel {
            return Err(format!("{device} has {chans} channel(s), channel {want_channel} chosen"));
        }
        let rate = hwp.set_rate_near(rate, ValueOr::Nearest).map_err(|e| format!("rate: {e}"))?;
        // About 5 ms periods: low latency for MTC, still easy for a Pi 3.
        let period = (rate / 200).max(64) as i64;
        hwp.set_period_size_near(period, ValueOr::Nearest).map_err(|e| format!("period: {e}"))?;
        hwp.set_buffer_size_near(period * 8).map_err(|e| format!("buffer: {e}"))?;
        pcm.hw_params(&hwp).map_err(|e| format!("hw params: {e}"))?;
        (rate, chans as usize, format)
    };
    pcm.start().ok();
    let period = pcm.hw_params_current().and_then(|h| h.get_period_size()).unwrap_or(256) as usize;
    let n = period * channels;
    let buf = if format == Format::s16() {
        Buf::I16(vec![0; n])
    } else if format == Format::s32() {
        Buf::I32(vec![0; n])
    } else {
        Buf::F32(vec![0.0; n])
    };
    Ok(Input { pcm, rate, channels, buf })
}

pub fn run(app: Arc<App>) {
    crate::set_realtime(70);
    loop {
        let gen = app.audio_gen.load(Ordering::SeqCst);
        let (device, channel, rate) = {
            let c = app.config.lock().unwrap();
            (c.audio_device.clone(), c.audio_channel, c.sample_rate)
        };
        let mut input = match open(&device, channel, rate) {
            Ok(i) => i,
            Err(e) => {
                app.set_audio_status(false, Some(e), 0);
                // Retry, sooner when the config changes.
                for _ in 0..20 {
                    std::thread::sleep(Duration::from_millis(100));
                    if app.audio_gen.load(Ordering::SeqCst) != gen {
                        break;
                    }
                }
                continue;
            }
        };
        eprintln!("audio: {device} channel {channel}, {} Hz, {} channel(s)", input.rate, input.channels);
        app.set_audio_status(true, None, input.rate);
        let mut dec = LtcDecoder::new(input.rate);
        let mut mono: Vec<f32> = Vec::new();
        let mut frames: Vec<LtcFrame> = Vec::new();
        let mut pos: u64 = 0;
        let mut peak = 0f32;
        let mut peak_at = Instant::now();
        let ch = (channel - 1) as usize;
        let tx = app.engine_tx.lock().unwrap().clone();
        while app.audio_gen.load(Ordering::SeqCst) == gen {
            let res = match &mut input.buf {
                Buf::I16(b) => input.pcm.io_i16().and_then(|io| io.readi(b)),
                Buf::I32(b) => input.pcm.io_i32().and_then(|io| io.readi(b)),
                Buf::F32(b) => input.pcm.io_f32().and_then(|io| io.readi(b)),
            };
            let n = match res {
                Ok(n) => n,
                Err(e) => {
                    if input.pcm.try_recover(e, true).is_err() {
                        app.set_audio_status(false, Some(format!("read: {e}")), input.rate);
                        std::thread::sleep(Duration::from_millis(500));
                        break;
                    }
                    let _ = input.pcm.start();
                    continue;
                }
            };
            let now = Instant::now();
            mono.clear();
            let c = input.channels;
            match &input.buf {
                Buf::I16(b) => mono.extend((0..n).map(|i| f32::from(b[i * c + ch]) / 32768.0)),
                Buf::I32(b) => mono.extend((0..n).map(|i| b[i * c + ch] as f32 / 2147483648.0)),
                Buf::F32(b) => mono.extend((0..n).map(|i| b[i * c + ch])),
            }
            for s in &mono {
                peak = peak.max(s.abs());
            }
            frames.clear();
            dec.process(&mono, &mut frames);
            pos += n as u64;
            let end_pos = pos as f64;
            for f in frames.drain(..) {
                // The sample at end_pos was read at `now`.
                let ago = (end_pos - f.end_pos).max(0.0) / f64::from(input.rate);
                let at = now.checked_sub(Duration::from_secs_f64(ago)).unwrap_or(now);
                let _ = tx.send(EngineMsg::Frame { frame: f, at, rate: input.rate });
            }
            if now.duration_since(peak_at) >= Duration::from_millis(100) {
                let db = if peak > 0.0 { 20.0 * peak.log10() } else { -120.0 };
                app.status.lock().unwrap().level_db = db.max(-120.0);
                peak = 0.0;
                peak_at = now;
            }
        }
    }
}
