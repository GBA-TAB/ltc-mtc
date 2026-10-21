//! Turns decoded LTC frames into MTC and fires the cue events.
//!
//! An LTC frame is complete when its last bit ends, i.e. when the next frame starts, so the
//! frame "now" is the decoded one plus one (minus one in reverse). MTC and the events use
//! that frame.
//!
//! Events fire when playback crosses their time going forward: every event after the
//! position already handled, up to the current frame. That position follows reverse play
//! too, so rolling back over a cue and forward again fires it again. After a locate (a jump
//! of more than `max_gap_frames`), the events of the last `max_gap_frames` frames up to the
//! current one fire, so a cue at the very start of a roll is not lost while the reader locks.

use crate::config::{Command, CueEvent};
use crate::ltc::LtcFrame;
use crate::midi::Midi;
use crate::tc::{labels_per_day, Fps, Tc};
use crate::{local_time, App, EngineMsg, LogEntry, NextEvent};
use std::collections::VecDeque;
use crate::osc::OscMsg;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Prev {
    idx: u32,
    fps: Fps,
    start: Instant,
    dur: Duration,
    reverse: bool,
}

/// The labels whose events fire on a forward frame: (lo, lo + span], modulo a day.
struct Window {
    lo: u32,
    span: u32,
    fps: Fps,
}

impl Window {
    fn new(fired_upto: Option<(u32, Fps)>, idx: u32, fps: Fps, max_gap: u32) -> Window {
        let day = labels_per_day(fps);
        let lo = match fired_upto {
            // Continuing (also after a pause or a few missed frames): from where we were.
            Some((u, ufps)) if ufps == fps && (idx + day - u) % day <= max_gap => u,
            // Start or locate: the last max_gap frames.
            _ => (idx + day - max_gap) % day,
        };
        Window { lo, span: (idx + day - lo) % day, fps }
    }

    fn contains(&self, tc: Tc) -> bool {
        let day = labels_per_day(self.fps);
        let d = (tc.label_index(self.fps) + day - self.lo) % day;
        (1..=self.span).contains(&d)
    }
}

struct Engine {
    app: Arc<App>,
    midi: Option<Midi>,
    events: Vec<(Tc, CueEvent)>,
    mtc_enabled: bool,
    max_gap: u32,
    fixed_fps: Option<Fps>,
    detected: Fps,
    prev: Option<Prev>,
    stopped: bool,
    /// Label index up to which events have been handled.
    fired_upto: Option<(u32, Fps)>,
    /// Next quarter frame block: false = pieces 0-3 (latch a new time), true = pieces 4-7.
    qf_second_half: bool,
    qf_tc: Tc,
    pending: VecDeque<(Instant, u8)>,
    last_midi_check: Option<Instant>,
    osc: Sender<OscMsg>,
    osc_targets: Vec<String>,
}

pub fn run(app: Arc<App>, rx: Receiver<EngineMsg>) {
    crate::set_realtime(60);
    let mut e = Engine {
        app: app.clone(),
        midi: None,
        events: Vec::new(),
        mtc_enabled: true,
        max_gap: 4,
        fixed_fps: None,
        detected: Fps::F25,
        prev: None,
        stopped: true,
        fired_upto: None,
        qf_second_half: false,
        qf_tc: Tc::default(),
        pending: VecDeque::new(),
        last_midi_check: None,
        osc: crate::osc::spawn(app.clone()),
        osc_targets: Vec::new(),
    };
    e.reload();
    loop {
        let now = Instant::now();
        let wait = e
            .pending
            .front()
            .map(|(t, _)| t.saturating_duration_since(now))
            .unwrap_or(Duration::from_millis(50))
            .min(Duration::from_millis(50));
        match rx.recv_timeout(wait) {
            Ok(EngineMsg::Frame { frame, at, rate }) => e.frame(frame, at, rate),
            Ok(EngineMsg::ConfigChanged) => e.reload(),
            Ok(EngineMsg::Fire(id)) => {
                if let Some((_, ev)) = e.events.iter().find(|(_, ev)| ev.id == id).cloned() {
                    e.fire(&ev, "test");
                }
            }
            Ok(EngineMsg::Send(cmds)) => {
                for c in &cmds {
                    let _ = e.send(c);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        e.tick();
    }
}

impl Engine {
    fn reload(&mut self) {
        {
            let c = self.app.config.lock().unwrap();
            self.events = c
                .events
                .iter()
                .filter_map(|ev| Tc::parse(&ev.time).map(|tc| (tc, ev.clone())))
                .collect();
            self.mtc_enabled = c.mtc_enabled;
            self.max_gap = c.max_gap_frames.max(1);
            self.fixed_fps = c.fixed_fps();
            self.osc_targets = c.osc_targets.iter().map(|t| t.name.clone()).collect();
            let _ = self.osc.send(OscMsg::Targets(c.osc_targets.clone()));
        }
        self.last_midi_check = None;
        self.tick();
        self.update_next();
    }

    fn fps(&self) -> Fps {
        self.fixed_fps.unwrap_or(self.detected)
    }

    fn tick(&mut self) {
        let now = Instant::now();
        while let Some(&(t, data)) = self.pending.front() {
            if t > now {
                break;
            }
            self.pending.pop_front();
            if let Some(m) = &self.midi {
                let _ = m.quarter_frame(data);
            }
        }
        if let Some(p) = &self.prev {
            let timeout = (p.dur * 4).max(Duration::from_millis(150));
            if !self.stopped && now.duration_since(p.start) > timeout {
                self.stopped = true;
                self.pending.clear();
                self.qf_second_half = false;
                self.app.status.lock().unwrap().running = false;
            }
        }
        if self.last_midi_check.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(2)) {
            self.last_midi_check = Some(now);
            self.check_midi();
        }
    }

    fn check_midi(&mut self) {
        if self.midi.is_none() {
            match Midi::open() {
                Ok(m) => self.midi = Some(m),
                Err(e) => {
                    let mut s = self.app.status.lock().unwrap();
                    s.midi_ok = false;
                    s.midi_error = Some(format!("ALSA sequencer: {e}"));
                    return;
                }
            }
        }
        let (out, mtc, mtc_on, mut used) = {
            let c = self.app.config.lock().unwrap();
            let used: Vec<String> = c.events.iter().flat_map(|e| e.commands.iter().map(|c| c.output().to_string())).collect();
            (c.midi_output.clone(), c.mtc_output.clone().unwrap_or_default(), c.mtc_enabled, used)
        };
        let m = self.midi.as_mut().unwrap();
        m.refresh();
        let mut errors = Vec::new();
        if let Err(e) = m.connect_out(&out) {
            errors.push(format!("commands: {e}"));
        }
        // With MTC off, nothing is sent on the MTC port, and it is disconnected too.
        if let Err(e) = m.connect_mtc(if mtc_on { &mtc } else { "" }) {
            errors.push(format!("MTC: {e}"));
        }
        used.sort();
        used.dedup();
        for o in used.iter().filter(|o| !o.is_empty() && **o != out && !m.has_port(o)) {
            errors.push(format!("event output '{o}' not found"));
        }
        let mut s = self.app.status.lock().unwrap();
        s.midi_ok = errors.is_empty();
        s.midi_port = m.out_connected().map(str::to_string);
        s.mtc_port = m.mtc_connected().map(str::to_string);
        s.midi_error = if errors.is_empty() { None } else { Some(errors.join("; ")) };
    }

    fn detect_fps(&mut self, f: &LtcFrame, rate: u32) {
        if f.drop_frame {
            self.detected = Fps::F2997Df;
            return;
        }
        let measured = f64::from(rate) / f.frame_samples;
        let mut guess = [Fps::F24, Fps::F25, Fps::F30]
            .into_iter()
            .find(|c| (measured / c.real() - 1.0).abs() < 0.02)
            .unwrap_or(if self.detected == Fps::F2997Df { Fps::F30 } else { self.detected });
        // Labels that cannot exist at the guessed rate (varispeed fools the measurement).
        if u32::from(f.tc.f) >= guess.nominal() {
            guess = if f.tc.f >= 25 { Fps::F30 } else { Fps::F25 };
        }
        self.detected = guess;
    }

    fn frame(&mut self, f: LtcFrame, at: Instant, rate: u32) {
        self.detect_fps(&f, rate);
        let fps = self.fps();
        let day = labels_per_day(fps);
        let now_tc = if f.reverse { f.tc.prev(fps) } else { f.tc.next(fps) };
        let idx = now_tc.label_index(fps);
        let dur = Duration::from_secs_f64(f.frame_samples / f64::from(rate));

        let mut diff = 0;
        let mut contiguous = false;
        let mut start = at;
        if let Some(p) = &self.prev {
            if p.fps == fps && !p.reverse && !f.reverse && !self.stopped {
                diff = (idx + day - p.idx) % day;
                let elapsed = at.saturating_duration_since(p.start);
                contiguous = (1..=self.max_gap).contains(&diff) && elapsed < dur * (diff + 2);
                if contiguous {
                    // Smooth the frame clock: the audio blocks make the arrival time jitter.
                    let predicted = p.start + p.dur * diff;
                    let err = at.duration_since(predicted.min(at)).as_secs_f64()
                        - predicted.duration_since(at.min(predicted)).as_secs_f64();
                    if err.abs() < dur.as_secs_f64() / 2.0 {
                        let corr = err * 0.1;
                        start = if corr >= 0.0 {
                            predicted + Duration::from_secs_f64(corr)
                        } else {
                            predicted.checked_sub(Duration::from_secs_f64(-corr)).unwrap_or(predicted)
                        };
                    }
                }
            }
        }

        // Events (only while cues are active; the position is followed either way, so starting
        // again does not fire what passed while stopped).
        if !f.reverse && self.app.armed.load(std::sync::atomic::Ordering::SeqCst) {
            let w = Window::new(self.fired_upto, idx, fps, self.max_gap);
            let due: Vec<CueEvent> = self
                .events
                .iter()
                .filter(|(tc, ev)| ev.enabled && w.contains(*tc))
                .map(|(_, ev)| ev.clone())
                .collect();
            let shown = now_tc.display(fps);
            for ev in &due {
                self.fire(ev, &shown);
            }
        }

        // MTC.
        if self.mtc_enabled && self.midi.is_some() {
            let m = self.midi.as_ref().unwrap();
            // Whatever is left of the last frame goes out now, in order.
            while let Some((_, data)) = self.pending.pop_front() {
                let _ = m.quarter_frame(data);
            }
            if f.reverse {
                let _ = m.sysex(&crate::mtc::full_frame(now_tc, fps));
                self.qf_second_half = false;
            } else {
                if !(contiguous && diff == 1) {
                    // Start, locate or a missed frame: tell the receiver where we are, then
                    // start a new quarter frame cycle.
                    let _ = m.sysex(&crate::mtc::full_frame(now_tc, fps));
                    self.qf_second_half = false;
                }
                let first = if self.qf_second_half { 4 } else { 0 };
                if !self.qf_second_half {
                    self.qf_tc = now_tc;
                }
                let q = dur / 4;
                for k in 0..4u8 {
                    let data = crate::mtc::quarter_frame(self.qf_tc, fps, first + k);
                    self.pending.push_back((start + q * u32::from(k), data));
                }
                self.qf_second_half = !self.qf_second_half;
            }
        }

        self.fired_upto = Some((idx, fps));
        self.prev = Some(Prev { idx, fps, start, dur, reverse: f.reverse });
        self.stopped = false;
        {
            let mut s = self.app.status.lock().unwrap();
            s.tc = now_tc.display(fps);
            s.running = true;
            s.reverse = f.reverse;
            s.fps = fps.name().to_string();
            s.fps_detected = self.detected.name().to_string();
            s.speed = (f64::from(rate) / f.frame_samples) / fps.real();
            s.user_bits = format!("{:08X}", f.user_bits);
            s.frames_decoded += 1;
        }
        self.update_next();
        self.tick();
    }

    fn update_next(&self) {
        let Some(p) = &self.prev else {
            self.app.status.lock().unwrap().next_event = None;
            return;
        };
        let fps = p.fps;
        let day = labels_per_day(fps);
        let next = self
            .events
            .iter()
            .filter(|(_, ev)| ev.enabled)
            .map(|(tc, ev)| ((tc.label_index(fps) + day - p.idx) % day, ev))
            .filter(|(d, _)| *d > 0)
            .min_by_key(|(d, _)| *d);
        let mut s = self.app.status.lock().unwrap();
        s.next_event = next.map(|(d, ev)| NextEvent {
            id: ev.id.clone(),
            name: ev.name.clone(),
            time: ev.time.clone(),
            seconds: f64::from(d) / fps.real(),
        });
    }

    fn send(&self, c: &Command) -> Result<(), String> {
        if let Command::Osc { target, address, args } = c {
            if !self.osc_targets.iter().any(|t| t == target) {
                return Err(format!("OSC target '{target}' unknown (saved?)"));
            }
            // Delivery happens on the OSC thread; its errors show per target in the status.
            let packet = crate::osc::encode(address, args);
            return self.osc.send(OscMsg::Send { target: target.clone(), packet }).map_err(|e| e.to_string());
        }
        let m = self.midi.as_ref().ok_or("no ALSA sequencer")?;
        match c {
            Command::ControlChange { channel, controller, value, output } => m.control_change(output, *channel, *controller, *value),
            Command::ProgramChange { channel, program, output } => m.program_change(output, *channel, *program),
            Command::Osc { .. } => unreachable!(),
        }
    }

    fn fire(&self, ev: &CueEvent, at_tc: &str) {
        let mut errors = Vec::new();
        for c in &ev.commands {
            if let Err(e) = self.send(c) {
                errors.push(e);
            }
        }
        let summary = ev.commands.iter().map(Command::describe).collect::<Vec<_>>().join(", ");
        eprintln!("fire {} '{}' at {}: {}", ev.time, ev.name, at_tc, summary);
        let mut s = self.app.status.lock().unwrap();
        s.fired_seq += 1;
        s.last_fired = Some(ev.id.clone());
        s.log.push_front(LogEntry {
            wall: local_time(),
            tc: at_tc.to_string(),
            event_id: ev.id.clone(),
            name: ev.name.clone(),
            commands: summary,
            error: if errors.is_empty() { None } else { Some(errors.join("; ")) },
        });
        s.log.truncate(50);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays a sequence of (frame, reverse) and returns the labels of the events fired.
    fn play(events: &[Tc], frames: &[(Tc, bool)]) -> Vec<Tc> {
        let fps = Fps::F25;
        let mut upto = None;
        let mut fired = Vec::new();
        for &(tc, reverse) in frames {
            let idx = tc.label_index(fps);
            if !reverse {
                let w = Window::new(upto, idx, fps, 4);
                fired.extend(events.iter().filter(|e| w.contains(**e)));
            }
            upto = Some((idx, fps));
        }
        fired
    }

    fn run(from: Tc, n: usize, reverse: bool) -> Vec<(Tc, bool)> {
        let mut v = Vec::new();
        let mut t = from;
        for _ in 0..n {
            v.push((t, reverse));
            t = if reverse { t.prev(Fps::F25) } else { t.next(Fps::F25) };
        }
        v
    }

    #[test]
    fn crossing() {
        let cue = Tc::new(1, 0, 0, 10);
        // Plain playback across the cue: once.
        assert_eq!(play(&[cue], &run(Tc::new(1, 0, 0, 0), 50, false)), vec![cue]);
        // A missed decode on the cue frame itself still fires it.
        let mut v = run(Tc::new(1, 0, 0, 0), 30, false);
        v.retain(|(t, _)| t.f != 10 && t.f != 11);
        assert_eq!(play(&[cue], &v), vec![cue]);
        // Pause on the cue, resume: no second fire.
        let mut v = run(Tc::new(1, 0, 0, 0), 11, false);
        v.extend(run(Tc::new(1, 0, 0, 10), 10, false));
        assert_eq!(play(&[cue], &v), vec![cue]);
        // Roll back over the cue in reverse, then forward again: fires again.
        let mut v = run(Tc::new(1, 0, 0, 0), 20, false);
        v.extend(run(Tc::new(1, 0, 0, 18), 15, true));
        v.extend(run(Tc::new(1, 0, 0, 4), 15, false));
        assert_eq!(play(&[cue], &v), vec![cue, cue]);
        // Locate far past the cue: nothing; locate onto it (or just after): fires.
        assert_eq!(play(&[cue], &run(Tc::new(1, 0, 5, 0), 10, false)), vec![]);
        assert_eq!(play(&[cue], &run(Tc::new(1, 0, 0, 12), 10, false)), vec![cue]);
        // Midnight.
        let mid = Tc::new(0, 0, 0, 1);
        assert_eq!(play(&[mid], &run(Tc::new(23, 59, 59, 20), 20, false)), vec![mid]);
    }
}
