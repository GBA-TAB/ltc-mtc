//! SMPTE linear timecode (LTC): a biphase-mark decoder for audio samples, and an encoder
//! for test signals.
//!
//! The decoder tracks the bit period adaptively, so it follows varispeed and shuttle,
//! and reads frames played forward and in reverse.

use crate::tc::{Fps, Tc};

const SYNC_FORWARD: u16 = 0x3FFD; // bits 64..79 as received: 0011 1111 1111 1101
const SYNC_REVERSE: u16 = 0xBFFC; // the same word read backwards
const MASK80: u128 = (1u128 << 80) - 1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LtcFrame {
    pub tc: Tc,
    pub drop_frame: bool,
    pub color_frame: bool,
    pub user_bits: u32,
    pub reverse: bool,
    /// Absolute sample position (fractional) at which the frame's last bit ended.
    pub end_pos: f64,
    /// Measured length of the frame in samples (from the previous frame, or from the bit period).
    pub frame_samples: f64,
}

pub struct LtcDecoder {
    pos: u64,
    prev_x: f32,
    dc: f32,
    env: f32,
    high: bool,
    last_zc: f64,
    last_edge: Option<f64>,
    bit_period: f64,
    init: Vec<f64>,
    half_pending: bool,
    reg: u128,
    bits_since_sync: u32,
    /// Bits received since the lock (a frame needs 80).
    filled: u32,
    last_frame_end: Option<f64>,
    min_period: f64,
    max_period: f64,
}

impl LtcDecoder {
    pub fn new(rate: u32) -> LtcDecoder {
        let rate = f64::from(rate);
        LtcDecoder {
            pos: 0,
            prev_x: 0.0,
            dc: 0.0,
            env: 0.0,
            high: false,
            last_zc: 0.0,
            last_edge: None,
            bit_period: 0.0,
            init: Vec::with_capacity(128),
            half_pending: false,
            reg: 0,
            bits_since_sync: 0,
            filled: 0,
            last_frame_end: None,
            // From 30 fps at 4x speed to 24 fps at 1/8 speed.
            min_period: rate / (30.0 * 80.0 * 4.0),
            max_period: rate / (24.0 * 80.0 / 8.0),
        }
    }

    /// Feeds samples (full scale +-1.0); decoded frames are pushed to `out`.
    pub fn process(&mut self, samples: &[f32], out: &mut Vec<LtcFrame>) {
        for &raw in samples {
            self.dc += (raw - self.dc) * 0.0005;
            let x = raw - self.dc;
            let ax = x.abs();
            self.env = if ax > self.env { ax } else { self.env * 0.9995 };
            let p = self.pos as f64;
            // Zero crossing position, interpolated between the previous and this sample.
            if (x >= 0.0) != (self.prev_x >= 0.0) {
                let d = x - self.prev_x;
                let frac = if d != 0.0 { f64::from(-self.prev_x / d) } else { 0.0 };
                self.last_zc = p - 1.0 + frac.clamp(0.0, 1.0);
            }
            self.prev_x = x;
            self.pos += 1;
            if self.env < 0.002 {
                // Below about -54 dBFS: no signal.
                if self.last_edge.is_some() {
                    self.lose();
                }
                continue;
            }
            let thr = self.env * 0.15;
            let edge = if self.high && x < -thr {
                self.high = false;
                true
            } else if !self.high && x > thr {
                self.high = true;
                true
            } else {
                false
            };
            if edge {
                let at = self.last_zc;
                if let Some(prev) = self.last_edge {
                    self.interval(at - prev, at, out);
                }
                self.last_edge = Some(at);
            }
        }
    }

    fn lose(&mut self) {
        self.last_edge = None;
        self.half_pending = false;
        self.reg = 0;
        self.bits_since_sync = 0;
        self.filled = 0;
        self.last_frame_end = None;
    }

    fn relock(&mut self) {
        self.bit_period = 0.0;
        self.init.clear();
        self.half_pending = false;
        self.reg = 0;
        self.bits_since_sync = 0;
        self.filled = 0;
        self.last_frame_end = None;
    }

    fn interval(&mut self, iv: f64, at: f64, out: &mut Vec<LtcFrame>) {
        if self.bit_period == 0.0 {
            if iv < self.min_period * 0.4 || iv > self.max_period * 1.2 {
                self.init.clear();
                return;
            }
            self.init.push(iv);
            if self.init.len() >= 100 {
                // Every frame holds zeros (the sync word starts with two), so the longest
                // intervals in a frame's worth are full bits.
                let max = self.init.iter().cloned().fold(0.0, f64::max);
                let long: Vec<f64> = self.init.iter().cloned().filter(|&v| v > max * 0.75).collect();
                self.bit_period = long.iter().sum::<f64>() / long.len() as f64;
                self.init.clear();
            }
            return;
        }
        let t = self.bit_period;
        if iv > t * 1.6 || iv < t * 0.3 {
            // Dropout, glitch or a big speed change.
            self.half_pending = false;
            self.filled = 0;
            self.bits_since_sync += 8;
            if self.bits_since_sync > 320 {
                self.relock();
            }
            return;
        }
        if iv >= t * 0.75 {
            self.bit_period += (iv - t) * 0.05;
            self.half_pending = false;
            self.push(false, at, out);
        } else {
            self.bit_period += (iv * 2.0 - t) * 0.05;
            if self.half_pending {
                self.half_pending = false;
                self.push(true, at, out);
            } else {
                self.half_pending = true;
            }
        }
        self.bit_period = self.bit_period.clamp(self.min_period, self.max_period);
    }

    fn push(&mut self, bit: bool, at: f64, out: &mut Vec<LtcFrame>) {
        self.reg = ((self.reg << 1) | u128::from(bit)) & MASK80;
        self.bits_since_sync += 1;
        self.filled = self.filled.saturating_add(1);
        if self.bits_since_sync > 400 {
            // Five frames without a valid one: the period is probably wrong.
            self.relock();
            return;
        }
        if self.filled < 80 || (self.bits_since_sync < 80 && self.last_frame_end.is_some()) {
            return;
        }
        let low = (self.reg & 0xFFFF) as u16;
        let frame = match low {
            // Forward: frame bit i is received bit i, i.e. reg bit 79 - i.
            SYNC_FORWARD => decode_bits(|i| (self.reg >> (79 - i)) & 1 == 1, false),
            // Reverse: the 64 bits before the reversed sync are bits 63..0 of the frame.
            SYNC_REVERSE => decode_bits(|i| (self.reg >> (16 + i)) & 1 == 1, true),
            _ => None,
        };
        if let Some(mut f) = frame {
            let nominal = self.bit_period * 80.0;
            f.end_pos = at;
            f.frame_samples = match self.last_frame_end {
                Some(prev) if (at - prev - nominal).abs() < nominal * 0.25 => at - prev,
                _ => nominal,
            };
            self.last_frame_end = Some(at);
            self.bits_since_sync = 0;
            out.push(f);
        }
    }
}

fn decode_bits(bit: impl Fn(usize) -> bool, reverse: bool) -> Option<LtcFrame> {
    let field = |start: usize, len: usize| -> u8 {
        (0..len).fold(0u8, |acc, k| acc | (u8::from(bit(start + k)) << k))
    };
    let fu = field(0, 4);
    let ft = field(8, 2);
    let su = field(16, 4);
    let st = field(24, 3);
    let mu = field(32, 4);
    let mt = field(40, 3);
    let hu = field(48, 4);
    let ht = field(56, 2);
    if fu > 9 || su > 9 || mu > 9 || hu > 9 {
        return None;
    }
    let tc = Tc::new(ht * 10 + hu, mt * 10 + mu, st * 10 + su, ft * 10 + fu);
    if tc.h > 23 || tc.m > 59 || tc.s > 59 || tc.f > 29 {
        return None;
    }
    let mut user_bits = 0u32;
    for (g, start) in [4usize, 12, 20, 28, 36, 44, 52, 60].iter().enumerate() {
        user_bits |= u32::from(field(*start, 4)) << (4 * g);
    }
    Some(LtcFrame {
        tc,
        drop_frame: bit(10),
        color_frame: bit(11),
        user_bits,
        reverse,
        end_pos: 0.0,
        frame_samples: 0.0,
    })
}

/// The 80 bits of an LTC frame, in transmission order.
pub fn encode_bits(tc: Tc, fps: Fps, user_bits: u32) -> [bool; 80] {
    let mut b = [false; 80];
    let mut put = |start: usize, len: usize, v: u32| {
        for k in 0..len {
            b[start + k] = (v >> k) & 1 == 1;
        }
    };
    put(0, 4, u32::from(tc.f % 10));
    put(8, 2, u32::from(tc.f / 10));
    put(10, 1, u32::from(fps.drop_frame()));
    put(16, 4, u32::from(tc.s % 10));
    put(24, 3, u32::from(tc.s / 10));
    put(32, 4, u32::from(tc.m % 10));
    put(40, 3, u32::from(tc.m / 10));
    put(48, 4, u32::from(tc.h % 10));
    put(56, 2, u32::from(tc.h / 10));
    for (g, start) in [4usize, 12, 20, 28, 36, 44, 52, 60].iter().enumerate() {
        put(*start, 4, (user_bits >> (4 * g)) & 0xF);
    }
    put(64, 16, 0b1011_1111_1111_1100); // sync, LSB first: 0011 1111 1111 1101
    // Biphase-mark polarity correction: an even number of zeros per frame
    // (bit 27 at 25 fps, bit 59 otherwise).
    let zeros = b.iter().filter(|&&x| !x).count();
    if zeros % 2 == 1 {
        let pc = if fps == Fps::F25 { 59 } else { 27 };
        b[pc] = true;
    }
    b
}

/// Generates an LTC signal. `speed` is 1.0 for real time; a negative speed plays backwards.
pub struct LtcEncoder {
    rate: f64,
    fps: Fps,
    level: f32,
    phase: f64,
    out: f32,
    state: f32,
    smooth: f32,
}

impl LtcEncoder {
    pub fn new(rate: u32, fps: Fps, level: f32) -> LtcEncoder {
        // About 25 us rise time, as in SMPTE 12M.
        let smooth = 1.0 - (-1.0 / (25e-6 * rate as f32 / 2.2)).exp();
        LtcEncoder { rate: f64::from(rate), fps, level, phase: 0.0, out: 0.0, state: 1.0, smooth }
    }

    /// Appends one frame's samples at the given speed (magnitude; `reverse` sends the bits backwards).
    pub fn frame(&mut self, tc: Tc, speed: f64, reverse: bool, buf: &mut Vec<f32>) {
        let bits = encode_bits(tc, self.fps, 0);
        let per_bit = self.rate / (self.fps.real() * 80.0 * speed);
        let order: Vec<usize> = if reverse { (0..80).rev().collect() } else { (0..80).collect() };
        for i in order {
            // Transition at the start of every bit, plus mid-bit for a one.
            self.state = -self.state;
            let mut t = self.phase;
            let mid = per_bit / 2.0;
            let mut flipped = false;
            while t < per_bit {
                if bits[i] && !flipped && t >= mid {
                    self.state = -self.state;
                    flipped = true;
                }
                self.out += (self.state * self.level - self.out) * self.smooth;
                buf.push(self.out);
                t += 1.0;
            }
            self.phase = t - per_bit;
        }
    }
}

/// Writes 16-bit mono PCM WAV.
pub fn write_wav(path: &str, rate: u32, samples: &[f32]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data_len = (samples.len() * 2) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        f.write_all(&v.to_le_bytes())?;
    }
    f.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(rate: u32, fps: Fps, speed: f64, reverse: bool, noise: f32, level: f32) -> Vec<LtcFrame> {
        let mut enc = LtcEncoder::new(rate, fps, level);
        let mut buf = Vec::new();
        let mut tc = Tc::new(0, 59, 58, 0);
        if !tc.valid_for(fps) {
            tc = tc.next(fps);
        }
        let mut sent = Vec::new();
        for _ in 0..(fps.nominal() * 4) {
            enc.frame(tc, speed, reverse, &mut buf);
            sent.push(tc);
            tc = if reverse { tc.prev(fps) } else { tc.next(fps) };
        }
        // Deterministic pseudo noise.
        let mut seed = 12345u32;
        for s in buf.iter_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *s += ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 2.0 * noise;
        }
        let mut dec = LtcDecoder::new(rate);
        let mut out = Vec::new();
        for chunk in buf.chunks(256) {
            dec.process(chunk, &mut out);
        }
        assert!(out.len() as u32 >= fps.nominal() * 4 - 6, "{fps:?} {rate} speed {speed}: only {} frames", out.len());
        // Decoded frames must be consecutive.
        for w in out.windows(2) {
            let expect = if reverse { w[0].tc.prev(fps) } else { w[0].tc.next(fps) };
            assert_eq!(w[1].tc, expect, "{fps:?} {rate} speed {speed} reverse {reverse}");
            assert_eq!(w[1].reverse, reverse);
            assert_eq!(w[1].drop_frame, fps.drop_frame());
        }
        out
    }

    #[test]
    fn decodes_all_rates() {
        for rate in [44100, 48000, 96000] {
            for fps in [Fps::F24, Fps::F25, Fps::F2997Df, Fps::F30] {
                let out = run(rate, fps, 1.0, false, 0.0, 0.5);
                let fs = out.last().unwrap().frame_samples;
                let expect = f64::from(rate) / fps.real();
                assert!((fs - expect).abs() < 2.0, "{fps:?} {rate}: frame {fs} vs {expect}");
            }
        }
    }

    #[test]
    fn decodes_reverse_varispeed_noise() {
        run(48000, Fps::F25, 1.0, true, 0.0, 0.5);
        run(48000, Fps::F30, 0.5, false, 0.0, 0.5);
        run(48000, Fps::F24, 2.0, false, 0.0, 0.5);
        run(48000, Fps::F2997Df, 1.0, true, 0.0, 0.5);
        run(44100, Fps::F25, 1.0, false, 0.05, 0.3);
        run(48000, Fps::F25, 1.0, false, 0.0, 0.01); // -40 dBFS
    }

    #[test]
    fn inverted_polarity() {
        let mut enc = LtcEncoder::new(48000, Fps::F25, 0.5);
        let mut buf = Vec::new();
        let mut tc = Tc::new(10, 0, 0, 0);
        for _ in 0..50 {
            enc.frame(tc, 1.0, false, &mut buf);
            tc = tc.next(Fps::F25);
        }
        for s in buf.iter_mut() {
            *s = -*s;
        }
        let mut dec = LtcDecoder::new(48000);
        let mut out = Vec::new();
        dec.process(&buf, &mut out);
        assert!(out.len() >= 45);
        // The last frame never completes: its final bit needs the next frame's first edge.
        assert_eq!(out.last().unwrap().tc, Tc::new(10, 0, 1, 23));
    }
}
