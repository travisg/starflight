// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! The XT keyboard interface (port 60h + IRQ1) and the BIOS translation of
//! set-1 scan codes into the words INT 16h returns.
//!
//! Scan codes queue up and are latched one at a time: the next one is
//! delivered only after the previous IRQ1 was acknowledged (by the BIOS INT 9
//! handler, or by pulsing port 61h bit 7) and a short gap has passed.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Gap between scan codes, about what an XT keyboard's serial link takes.
pub const CODE_GAP_MASTER: u64 = 14_318; // 1 ms

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KeyboardCtl {
    queue: VecDeque<u8>,
    pub latch: u8,
    busy: bool,
    next_time: u64,
}

impl KeyboardCtl {
    pub fn push(&mut self, code: u8) {
        self.queue.push_back(code);
    }

    /// Codes the BIOS has not handled yet: the latched one, then the queue.
    pub fn pending(&self) -> impl Iterator<Item = u8> + '_ {
        self.busy.then_some(self.latch).into_iter().chain(self.queue.iter().copied())
    }

    /// Nothing queued and nothing waiting to be handled.
    pub fn idle(&self) -> bool {
        self.queue.is_empty() && !self.busy
    }

    /// Master tick at which `poll` may deliver the next code.
    pub fn next_event(&self) -> Option<u64> {
        (!self.busy && !self.queue.is_empty()).then_some(self.next_time)
    }

    /// Latches the next code if one is due; true means raise IRQ1.
    pub fn poll(&mut self, now: u64) -> bool {
        if self.busy || now < self.next_time {
            return false;
        }
        match self.queue.pop_front() {
            Some(c) => {
                self.latch = c;
                self.busy = true;
                true
            }
            None => false,
        }
    }

    pub fn ack(&mut self, now: u64) {
        if self.busy {
            self.busy = false;
            self.next_time = now + CODE_GAP_MASTER;
        }
    }
}

/// BDA keyboard flag bits at 0040:0017.
pub mod shift {
    pub const RSHIFT: u8 = 0x01;
    pub const LSHIFT: u8 = 0x02;
    pub const CTRL: u8 = 0x04;
    pub const ALT: u8 = 0x08;
    pub const SCROLL: u8 = 0x10;
    pub const NUM: u8 = 0x20;
    pub const CAPS: u8 = 0x40;
    pub const INSERT: u8 = 0x80;
}

/// Unshifted and shifted ASCII for scan codes 00h-39h (0 = none).
const MAIN: [(u8, u8); 0x3a] = [
    (0, 0),
    (0x1b, 0x1b),
    (b'1', b'!'),
    (b'2', b'@'),
    (b'3', b'#'),
    (b'4', b'$'),
    (b'5', b'%'),
    (b'6', b'^'),
    (b'7', b'&'),
    (b'8', b'*'),
    (b'9', b'('),
    (b'0', b')'),
    (b'-', b'_'),
    (b'=', b'+'),
    (0x08, 0x08),
    (0x09, 0),
    (b'q', b'Q'),
    (b'w', b'W'),
    (b'e', b'E'),
    (b'r', b'R'),
    (b't', b'T'),
    (b'y', b'Y'),
    (b'u', b'U'),
    (b'i', b'I'),
    (b'o', b'O'),
    (b'p', b'P'),
    (b'[', b'{'),
    (b']', b'}'),
    (0x0d, 0x0d),
    (0, 0),
    (b'a', b'A'),
    (b's', b'S'),
    (b'd', b'D'),
    (b'f', b'F'),
    (b'g', b'G'),
    (b'h', b'H'),
    (b'j', b'J'),
    (b'k', b'K'),
    (b'l', b'L'),
    (b';', b':'),
    (b'\'', b'"'),
    (b'`', b'~'),
    (0, 0),
    (b'\\', b'|'),
    (b'z', b'Z'),
    (b'x', b'X'),
    (b'c', b'C'),
    (b'v', b'V'),
    (b'b', b'B'),
    (b'n', b'N'),
    (b'm', b'M'),
    (b',', b'<'),
    (b'.', b'>'),
    (b'/', b'?'),
    (0, 0),
    (b'*', 0),
    (0, 0),
    (b' ', b' '),
];

const KEYPAD_DIGITS: &[u8; 13] = b"789-456+1230.";

/// What a scan code does to the BIOS state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    /// A word for the INT 16h buffer (scan code << 8 | ASCII).
    Key(u16),
    /// Only the shift state changed (or the key produces nothing).
    None,
}

/// Translates one set-1 scan code like the XT BIOS INT 9 handler, updating
/// the shift flags at 0040:0017.
pub fn translate(code: u8, flags: &mut u8) -> KeyEvent {
    use shift::*;
    let release = code & 0x80 != 0;
    let scan = code & 0x7f;
    let modifier = match scan {
        0x36 => Some(RSHIFT),
        0x2a => Some(LSHIFT),
        0x1d => Some(CTRL),
        0x38 => Some(ALT),
        _ => None,
    };
    if let Some(m) = modifier {
        if release {
            *flags &= !m;
        } else {
            *flags |= m;
        }
        return KeyEvent::None;
    }
    if release {
        return KeyEvent::None;
    }
    let toggle = match scan {
        0x3a => Some(CAPS),
        0x45 => Some(NUM),
        0x46 => Some(SCROLL),
        _ => None,
    };
    if let Some(t) = toggle {
        *flags ^= t;
        return KeyEvent::None;
    }
    let shifted = *flags & (LSHIFT | RSHIFT) != 0;
    let ctrl = *flags & CTRL != 0;
    let alt = *flags & ALT != 0;
    let w = |ascii: u8| KeyEvent::Key((scan as u16) << 8 | ascii as u16);
    let ext = |s: u16| KeyEvent::Key(s << 8);

    // Function keys F1-F10.
    if (0x3b..=0x44).contains(&scan) {
        let i = (scan - 0x3b) as u16;
        return ext(if alt {
            0x68 + i
        } else if ctrl {
            0x5e + i
        } else if shifted {
            0x54 + i
        } else {
            scan as u16
        });
    }
    // Keypad.
    if (0x47..=0x53).contains(&scan) {
        let i = (scan - 0x47) as usize;
        if ctrl {
            return match scan {
                0x47 => ext(0x77),
                0x49 => ext(0x84),
                0x4b => ext(0x73),
                0x4d => ext(0x74),
                0x4f => ext(0x75),
                0x51 => ext(0x76),
                _ => KeyEvent::None,
            };
        }
        if alt {
            return KeyEvent::None;
        }
        if scan == 0x4a || scan == 0x4e {
            return w(KEYPAD_DIGITS[i]);
        }
        let digits = (*flags & NUM != 0) != shifted;
        if digits {
            return w(KEYPAD_DIGITS[i]);
        }
        return match scan {
            0x4c => KeyEvent::None,
            0x52 => {
                *flags ^= INSERT;
                ext(0x52)
            }
            _ => ext(scan as u16),
        };
    }
    let Some(&(lo, hi)) = MAIN.get(scan as usize) else {
        return KeyEvent::None;
    };
    if alt {
        return match scan {
            0x02..=0x0d => ext(0x76 + scan as u16),
            _ if lo.is_ascii_alphabetic() => ext(scan as u16),
            _ => KeyEvent::None,
        };
    }
    if ctrl {
        return match lo {
            b'a'..=b'z' => w(lo - b'a' + 1),
            b'[' => w(0x1b),
            b'\\' => w(0x1c),
            b']' => w(0x1d),
            b'6' => w(0x1e),
            b'-' => w(0x1f),
            b'2' => w(0),
            0x0d => w(0x0a),
            0x08 => w(0x7f),
            0x1b => w(0x1b),
            _ => KeyEvent::None,
        };
    }
    if scan == 0x0f && shifted {
        return ext(0x0f);
    }
    let letter = lo.is_ascii_alphabetic();
    let upper = shifted != (letter && *flags & CAPS != 0);
    let ascii = if upper { hi } else { lo };
    if ascii == 0 { KeyEvent::None } else { w(ascii) }
}

/// Set-1 make code and whether Shift is needed, for typing `c` in scripts.
pub fn scan_for_char(c: char) -> Option<(u8, bool)> {
    let b = u8::try_from(c).ok()?;
    match b {
        b'\n' | b'\r' => return Some((0x1c, false)),
        0x1b => return Some((0x01, false)),
        0x08 => return Some((0x0e, false)),
        b'\t' => return Some((0x0f, false)),
        _ => {}
    }
    MAIN.iter().enumerate().find_map(|(scan, &(lo, hi))| {
        if lo == b && b != 0 {
            Some((scan as u8, false))
        } else if hi == b && b != 0 {
            Some((scan as u8, true))
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_shift_caps() {
        let mut f = 0;
        assert_eq!(translate(0x1e, &mut f), KeyEvent::Key(0x1e61));
        translate(0x2a, &mut f);
        assert_eq!(translate(0x1e, &mut f), KeyEvent::Key(0x1e41));
        translate(0xaa, &mut f);
        assert_eq!(f, 0);
        translate(0x3a, &mut f);
        assert_eq!(translate(0x1e, &mut f), KeyEvent::Key(0x1e41));
    }

    #[test]
    fn keypad_follows_numlock() {
        let mut f = 0;
        assert_eq!(translate(0x48, &mut f), KeyEvent::Key(0x4800));
        translate(0x45, &mut f);
        assert_eq!(translate(0x48, &mut f), KeyEvent::Key(0x4838));
    }

    #[test]
    fn controller_paces_codes() {
        let mut k = KeyboardCtl::default();
        k.push(0x1e);
        k.push(0x9e);
        assert!(k.poll(0));
        assert!(!k.poll(1), "waits for the first code to be handled");
        k.ack(10);
        assert!(!k.poll(10));
        assert!(k.poll(10 + CODE_GAP_MASTER));
        assert_eq!(k.latch, 0x9e);
    }

    #[test]
    fn script_chars() {
        assert_eq!(scan_for_char('a'), Some((0x1e, false)));
        assert_eq!(scan_for_char('A'), Some((0x1e, true)));
        assert_eq!(scan_for_char('\n'), Some((0x1c, false)));
    }
}
