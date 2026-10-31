// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! 8253 programmable interval timer, ports 40h-43h. Channel 0 drives IRQ0 and
//! channel 2 the speaker. Counts are computed from the PIT tick count (master
//! ticks / 12) when read, not stepped.
//!
//! Gates (`Pit::set_gate`; on the PC only channel 2's is switchable, through
//! port 61h bit 0) act as the 8253 data sheet says: in modes 0 and 4 a low
//! gate suspends counting; in modes 2 and 3 it also forces OUT high, and the
//! rising edge restarts the count; in modes 1 and 5 the rising edge starts or
//! restarts it, and writing a count only arms the counter.

use serde::{Deserialize, Serialize};

pub const MASTER_PER_PIT: u64 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub mode: u8,
    /// 1 = low byte, 2 = high byte, 3 = low then high.
    access: u8,
    /// Initial count, 1..=65536.
    pub reload: u32,
    /// PIT tick the current count started at, moved on by any time a low
    /// gate suspended it.
    pub load_time: u64,
    /// A count was written after the control word (and, in modes 1 and 5,
    /// triggered).
    pub counting: bool,
    write_hi_next: bool,
    pending_lo: u8,
    latch: Option<u16>,
    read_hi_next: bool,
    /// Gate input; change it with `Pit::set_gate`.
    pub gate: bool,
    /// Ticks counted when a low gate suspended counting (modes 0, 2, 3, 4).
    held: Option<u64>,
}

impl Channel {
    fn new() -> Self {
        Channel {
            mode: 3,
            access: 3,
            reload: 0x10000,
            load_time: 0,
            counting: true,
            write_hi_next: false,
            pending_lo: 0,
            latch: None,
            read_hi_next: false,
            gate: true,
            held: None,
        }
    }

    /// Ticks counted since the count was loaded, not counting suspensions.
    fn elapsed(&self, now: u64) -> u64 {
        self.held.unwrap_or_else(|| now.saturating_sub(self.load_time))
    }

    pub fn count(&self, now: u64) -> u16 {
        if !self.counting {
            return self.reload as u16;
        }
        let n = self.reload as u64;
        let el = self.elapsed(now);
        let v = match self.mode {
            2 => n - el % n,
            3 => {
                // Counts down by two through each half period.
                let half = (n / 2).max(1);
                n - 2 * (el % half)
            }
            _ => {
                if el < n {
                    n - el
                } else {
                    0x10000 - (el - n) % 0x10000
                }
            }
        };
        v as u16
    }

    /// Output level at `now`.
    pub fn output(&self, now: u64) -> bool {
        if !self.counting {
            return true;
        }
        let n = self.reload as u64;
        let el = self.elapsed(now);
        match self.mode {
            0 | 1 => el >= n,
            2 => !self.gate || el % n != n - 1,
            3 => !self.gate || el % n < n.div_ceil(2),
            _ => true,
        }
    }

    /// Ticks between output events (IRQs) in the periodic modes.
    pub fn period(&self) -> Option<u64> {
        match self.mode {
            2 | 3 if self.counting && self.held.is_none() => Some(self.reload as u64),
            _ => None,
        }
    }

    /// Starts counting the loaded count at `now`, or holds it at zero ticks
    /// while the gate is low (modes 0, 2, 3, 4).
    fn start(&mut self, now: u64) {
        self.load_time = now;
        self.counting = true;
        self.held = (!self.gate && !matches!(self.mode, 1 | 5)).then_some(0);
    }

    fn set_gate(&mut self, level: bool, now: u64) {
        if level == self.gate {
            return;
        }
        self.gate = level;
        match (self.mode, level) {
            (0 | 2 | 3 | 4, false) if self.counting => self.held = Some(self.elapsed(now)),
            (0 | 4, true) => {
                if let Some(el) = self.held.take() {
                    self.load_time = now.saturating_sub(el);
                }
            }
            (2 | 3, true) if self.counting => self.start(now),
            // A trigger counts whatever was last loaded.
            (1 | 5, true) => self.start(now),
            _ => {}
        }
    }
}

/// Channel 2 and port 61h bits as one snapshot, for synthesising the speaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpeakerState {
    pub ch2: Channel,
    /// Port 61h bit 1: speaker data enable.
    pub enable: bool,
}

impl SpeakerState {
    /// Speaker cone position at PIT tick `now`: channel 2 output gated by bit 1.
    pub fn level(&self, now: u64) -> bool {
        self.enable && self.ch2.output(now)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pit {
    pub ch: [Channel; 3],
    /// PIT tick of the next IRQ0, if channel 0 is periodic or armed.
    pub next_irq0: Option<u64>,
}

impl Default for Pit {
    fn default() -> Self {
        Self::new()
    }
}

impl Pit {
    /// As the BIOS leaves it: channel 0 in mode 3 with count 65536 (18.2 Hz).
    pub fn new() -> Self {
        let mut p = Pit { ch: [Channel::new(); 3], next_irq0: None };
        p.set_gate(2, false, 0);
        p.schedule_irq0(0);
        p
    }

    /// Sets channel `i`'s gate input at PIT tick `now`.
    pub fn set_gate(&mut self, i: usize, level: bool, now: u64) {
        self.ch[i].set_gate(level, now);
        if i == 0 {
            self.schedule_irq0(now);
        }
    }

    fn schedule_irq0(&mut self, now: u64) {
        let c = &self.ch[0];
        let end = c.load_time + c.reload as u64;
        self.next_irq0 = match c.period() {
            _ if !c.counting || c.held.is_some() => None,
            Some(p) => {
                let el = now.saturating_sub(c.load_time);
                Some(c.load_time + (el / p + 1) * p)
            }
            None if matches!(c.mode, 0 | 1) && now < end => Some(end),
            None => None,
        };
    }

    /// Checks restored state at PIT tick `now`: counts in 1..=65536, modes
    /// and access modes the control word can set, load times not in the
    /// future and the next IRQ0 within one count of `now`.
    pub(crate) fn check(&self, now: u64) -> Result<(), String> {
        for (i, c) in self.ch.iter().enumerate() {
            if !(1..=0x10000).contains(&c.reload) || c.mode > 5 || !(1..=3).contains(&c.access) || c.load_time > now {
                return Err(format!(
                    "PIT channel {i}: mode {}, access {}, count {}, loaded at tick {} of {now}",
                    c.mode, c.access, c.reload, c.load_time
                ));
            }
        }
        match self.next_irq0 {
            Some(t) if t > now + 0x10000 => Err(format!("PIT: next IRQ0 at tick {t} of {now}")),
            _ => Ok(()),
        }
    }

    /// Called when `now` has reached `next_irq0`; returns true to raise IRQ0.
    pub fn irq0_due(&mut self, now: u64) -> bool {
        match self.next_irq0 {
            Some(t) if now >= t => {
                self.schedule_irq0(now);
                true
            }
            _ => false,
        }
    }

    pub fn write(&mut self, port: u16, v: u8, now: u64) {
        if port == 0x43 {
            let i = (v >> 6) as usize;
            if i == 3 {
                return;
            }
            let c = &mut self.ch[i];
            let access = (v >> 4) & 3;
            if access == 0 {
                if c.latch.is_none() {
                    c.latch = Some(c.count(now));
                    c.read_hi_next = false;
                }
                return;
            }
            c.access = access;
            c.mode = match (v >> 1) & 7 {
                6 => 2,
                7 => 3,
                m => m,
            };
            c.counting = false;
            c.held = None;
            c.write_hi_next = false;
            c.latch = None;
            c.read_hi_next = false;
            if i == 0 {
                self.next_irq0 = None;
            }
            return;
        }
        let i = (port & 3) as usize;
        let c = &mut self.ch[i];
        let complete = match c.access {
            1 => {
                c.reload = v as u32;
                true
            }
            2 => {
                c.reload = (v as u32) << 8;
                true
            }
            _ => {
                if c.write_hi_next {
                    c.reload = c.pending_lo as u32 | (v as u32) << 8;
                    c.write_hi_next = false;
                    true
                } else {
                    c.pending_lo = v;
                    c.write_hi_next = true;
                    false
                }
            }
        };
        if complete {
            if c.reload == 0 {
                c.reload = 0x10000;
            }
            // Modes 1 and 5 wait for a gate trigger.
            if !matches!(c.mode, 1 | 5) {
                c.start(now);
            }
            if i == 0 {
                self.schedule_irq0(now);
            }
        }
    }

    pub fn read(&mut self, port: u16, now: u64) -> u8 {
        let c = &mut self.ch[(port & 3) as usize];
        let value = c.latch.unwrap_or_else(|| c.count(now));
        let hi = match c.access {
            1 => false,
            2 => true,
            _ => {
                let hi = c.read_hi_next;
                c.read_hi_next = !hi;
                hi
            }
        };
        if hi || c.access == 1 {
            c.latch = None;
        }
        if hi { (value >> 8) as u8 } else { value as u8 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bios_rate_and_latch() {
        let mut p = Pit::new();
        assert_eq!(p.next_irq0, Some(65536));
        assert!(!p.irq0_due(65535));
        assert!(p.irq0_due(65536));
        assert_eq!(p.next_irq0, Some(131072));
        // Latch then read low, high, as the game's TIME word does.
        p.write(0x43, 0x00, 1000);
        let lo = p.read(0x40, 5000) as u16;
        let hi = p.read(0x40, 5000) as u16;
        assert_eq!(lo | hi << 8, 65536u32.wrapping_sub(2000) as u16);
    }

    /// Channel 2 in `mode` with count `n` written at tick 0; the gate stays
    /// low, as the BIOS leaves it.
    fn ch2(mode: u8, n: u16) -> Pit {
        let mut p = Pit::new();
        p.write(0x43, 0xb0 | mode << 1, 0);
        p.write(0x42, n as u8, 0);
        p.write(0x42, (n >> 8) as u8, 0);
        p
    }

    #[test]
    fn square_wave_for_speaker() {
        let mut p = ch2(3, 100);
        p.set_gate(2, true, 0);
        let c = p.ch[2];
        assert!(c.output(10) && !c.output(60) && c.output(110));
    }

    #[test]
    fn square_wave_gate() {
        let mut p = ch2(3, 100);
        // Gate low from the start: OUT high, the count waits.
        assert!((0..60).all(|t| p.ch[2].output(t)));
        assert_eq!(p.ch[2].count(59), 100);
        // The rising edge starts a fresh high half-period.
        p.set_gate(2, true, 60);
        assert!((60..110).all(|t| p.ch[2].output(t)));
        assert!((110..160).all(|t| !p.ch[2].output(t)));
        assert!(p.ch[2].output(160));
        // Low in the low half: OUT goes high and the count holds.
        let held = p.ch[2].count(130);
        p.set_gate(2, false, 130);
        assert!((130..300).all(|t| p.ch[2].output(t)));
        assert_eq!(p.ch[2].count(299), held);
        // High again: a fresh period, not the rest of the old one.
        p.set_gate(2, true, 300);
        assert_eq!(p.ch[2].count(300), 100);
        assert!((300..350).all(|t| p.ch[2].output(t)));
        assert!(!p.ch[2].output(350));
    }

    #[test]
    fn rate_generator_gate() {
        let mut p = ch2(2, 10);
        p.set_gate(2, true, 5);
        assert!(!p.ch[2].output(14) && p.ch[2].output(15));
        p.set_gate(2, false, 23);
        assert!(p.ch[2].output(24), "a low gate forces OUT high");
        assert_eq!(p.ch[2].count(40), 2);
        p.set_gate(2, true, 40);
        assert_eq!(p.ch[2].count(40), 10);
        assert!(!p.ch[2].output(49));
    }

    #[test]
    fn interrupt_on_terminal_count_pauses_with_the_gate() {
        let mut p = ch2(0, 100);
        assert_eq!(p.ch[2].count(50), 100, "waits for the gate");
        p.set_gate(2, true, 50);
        p.set_gate(2, false, 80);
        assert_eq!(p.ch[2].count(200), 70);
        assert!(!p.ch[2].output(200));
        p.set_gate(2, true, 200);
        assert!(!p.ch[2].output(269) && p.ch[2].output(270));
    }

    #[test]
    fn one_shot_is_triggered_by_the_gate() {
        let mut p = ch2(1, 20);
        assert!(p.ch[2].output(5), "armed, not started, by the count");
        p.set_gate(2, true, 10);
        assert!(!p.ch[2].output(10) && !p.ch[2].output(29) && p.ch[2].output(30));
        // Low then high again retriggers.
        p.set_gate(2, false, 40);
        assert!(p.ch[2].output(45));
        p.set_gate(2, true, 50);
        assert!(!p.ch[2].output(50) && p.ch[2].output(70));
        // Channel 0's gate stays high, so a one-shot there never fires.
        p.write(0x43, 0x32, 100);
        p.write(0x40, 10, 100);
        p.write(0x40, 0, 100);
        assert_eq!(p.next_irq0, None);
    }
}
