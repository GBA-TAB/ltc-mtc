//! SMPTE timecode values and frame rates.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fps {
    F24,
    F25,
    F2997Df,
    F30,
}

impl Fps {
    /// Frames per second as counted in the labels (30 for 29.97 drop-frame).
    pub fn nominal(self) -> u32 {
        match self {
            Fps::F24 => 24,
            Fps::F25 => 25,
            Fps::F2997Df | Fps::F30 => 30,
        }
    }

    pub fn real(self) -> f64 {
        match self {
            Fps::F24 => 24.0,
            Fps::F25 => 25.0,
            Fps::F2997Df => 30000.0 / 1001.0,
            Fps::F30 => 30.0,
        }
    }

    pub fn drop_frame(self) -> bool {
        self == Fps::F2997Df
    }

    /// The rate code in MTC (quarter frame piece 7, full frame hour byte).
    pub fn mtc_code(self) -> u8 {
        match self {
            Fps::F24 => 0,
            Fps::F25 => 1,
            Fps::F2997Df => 2,
            Fps::F30 => 3,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Fps::F24 => "24",
            Fps::F25 => "25",
            Fps::F2997Df => "29.97df",
            Fps::F30 => "30",
        }
    }

    pub fn parse(s: &str) -> Option<Fps> {
        match s.trim().to_ascii_lowercase().as_str() {
            "24" => Some(Fps::F24),
            "25" => Some(Fps::F25),
            "29.97df" | "29.97" | "2997" | "df" => Some(Fps::F2997Df),
            "30" => Some(Fps::F30),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Tc {
    pub h: u8,
    pub m: u8,
    pub s: u8,
    pub f: u8,
}

/// Frame labels in a day at a nominal rate (labels skipped by drop-frame included).
pub fn labels_per_day(fps: Fps) -> u32 {
    24 * 3600 * fps.nominal()
}

impl Tc {
    pub fn new(h: u8, m: u8, s: u8, f: u8) -> Tc {
        Tc { h, m, s, f }
    }

    /// Parses "HH:MM:SS:FF" (also with ';' or '.' as separators).
    pub fn parse(s: &str) -> Option<Tc> {
        let parts: Vec<&str> = s.trim().split([':', ';', '.']).collect();
        if parts.len() != 4 {
            return None;
        }
        let mut v = [0u8; 4];
        for (i, p) in parts.iter().enumerate() {
            if p.is_empty() || p.len() > 2 || !p.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            v[i] = p.parse().ok()?;
        }
        let tc = Tc::new(v[0], v[1], v[2], v[3]);
        if tc.h > 23 || tc.m > 59 || tc.s > 59 || tc.f > 29 {
            return None;
        }
        Some(tc)
    }

    pub fn valid_for(self, fps: Fps) -> bool {
        if self.h > 23 || self.m > 59 || self.s > 59 || u32::from(self.f) >= fps.nominal() {
            return false;
        }
        !(fps.drop_frame() && self.s == 0 && self.f < 2 && self.m % 10 != 0)
    }

    /// Position of the label in the day, counting every label at the nominal rate.
    /// Monotonic in the timecode, which is all event matching needs.
    pub fn label_index(self, fps: Fps) -> u32 {
        let n = fps.nominal();
        ((u32::from(self.h) * 60 + u32::from(self.m)) * 60 + u32::from(self.s)) * n
            + u32::from(self.f)
    }

    pub fn next(self, fps: Fps) -> Tc {
        let n = fps.nominal() as u8;
        let mut t = self;
        t.f += 1;
        if t.f >= n {
            t.f = 0;
            t.s += 1;
            if t.s >= 60 {
                t.s = 0;
                t.m += 1;
                if t.m >= 60 {
                    t.m = 0;
                    t.h = (t.h + 1) % 24;
                }
            }
        }
        if fps.drop_frame() && t.s == 0 && t.f == 0 && t.m % 10 != 0 {
            t.f = 2;
        }
        t
    }

    pub fn prev(self, fps: Fps) -> Tc {
        let n = fps.nominal() as u8;
        let mut t = self;
        let first = if fps.drop_frame() && t.s == 0 && t.m % 10 != 0 { 2 } else { 0 };
        if t.f > first {
            t.f -= 1;
            return t;
        }
        t.f = n - 1;
        if t.s > 0 {
            t.s -= 1;
        } else {
            t.s = 59;
            if t.m > 0 {
                t.m -= 1;
            } else {
                t.m = 59;
                t.h = if t.h == 0 { 23 } else { t.h - 1 };
            }
        }
        t
    }

    pub fn display(self, fps: Fps) -> String {
        let sep = if fps.drop_frame() { ';' } else { ':' };
        format!("{:02}:{:02}:{:02}{}{:02}", self.h, self.m, self.s, sep, self.f)
    }
}

impl fmt::Display for Tc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}:{:02}:{:02}", self.h, self.m, self.s, self.f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display() {
        assert_eq!(Tc::parse("01:02:03:04"), Some(Tc::new(1, 2, 3, 4)));
        assert_eq!(Tc::parse("1:2:3;4"), Some(Tc::new(1, 2, 3, 4)));
        assert_eq!(Tc::parse("24:00:00:00"), None);
        assert_eq!(Tc::parse("01:02:03"), None);
        assert_eq!(Tc::new(1, 2, 3, 4).display(Fps::F2997Df), "01:02:03;04");
    }

    #[test]
    fn next_prev_roundtrip() {
        for fps in [Fps::F24, Fps::F25, Fps::F2997Df, Fps::F30] {
            let mut t = Tc::new(23, 58, 0, 0);
            while !t.valid_for(fps) {
                t.f += 1;
            }
            for _ in 0..(fps.nominal() * 200) {
                let n = t.next(fps);
                assert!(n.valid_for(fps), "{fps:?} {n}");
                assert_eq!(n.prev(fps), t, "{fps:?} {t}");
                t = n;
            }
        }
    }

    #[test]
    fn drop_frame_skips() {
        let fps = Fps::F2997Df;
        assert_eq!(Tc::new(0, 0, 59, 29).next(fps), Tc::new(0, 1, 0, 2));
        assert_eq!(Tc::new(0, 9, 59, 29).next(fps), Tc::new(0, 10, 0, 0));
        assert_eq!(Tc::new(23, 59, 59, 29).next(Fps::F25), Tc::new(0, 0, 0, 0));
    }
}
