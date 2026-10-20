//! MIDI Time Code messages.

use crate::tc::{Fps, Tc};

/// Data byte of quarter-frame piece 0..7 (status 0xF1 is sent separately).
pub fn quarter_frame(tc: Tc, fps: Fps, piece: u8) -> u8 {
    let v = match piece {
        0 => tc.f & 0x0F,
        1 => tc.f >> 4,
        2 => tc.s & 0x0F,
        3 => tc.s >> 4,
        4 => tc.m & 0x0F,
        5 => tc.m >> 4,
        6 => tc.h & 0x0F,
        _ => ((tc.h >> 4) & 0x01) | (fps.mtc_code() << 1),
    };
    (piece << 4) | (v & 0x0F)
}

/// Full Frame SysEx: F0 7F 7F 01 01 hr mn sc fr F7 (device id 7F = all).
pub fn full_frame(tc: Tc, fps: Fps) -> [u8; 10] {
    [0xF0, 0x7F, 0x7F, 0x01, 0x01, (fps.mtc_code() << 5) | tc.h, tc.m, tc.s, tc.f, 0xF7]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces() {
        let tc = Tc::new(23, 59, 58, 24);
        let q: Vec<u8> = (0..8).map(|p| quarter_frame(tc, Fps::F25, p)).collect();
        assert_eq!(q, vec![0x08, 0x11, 0x2A, 0x33, 0x4B, 0x53, 0x67, 0x73]);
        assert_eq!(full_frame(tc, Fps::F30)[5], 0x60 | 23);
    }
}
