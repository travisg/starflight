// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! 8253 programmable interval timer, ports 40h-43h. Channel 0 drives IRQ0 and
//! channel 2 the speaker. Counts are computed from the PIT tick count (master
//! ticks / 12) when read, not stepped.

pub const MASTER_PER_PIT: u64 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Channel {
    pub mode: u8,
    /// 1 = low byte, 2 = high byte, 3 = low then high.
    access: u8,
    /// Initial count, 1..=65536.
    pub reload: u32,
    /// PIT tick at which the current count was loaded.
    pub load_time: u64,
    pub counting: bool,
    write_hi_next: bool,
    pending_lo: u8,
    latch: Option<u16>,
    read_hi_next: bool,
    /// Gate input; only channel 2's is switchable (port 61h bit 0).
    pub gate: bool,
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
        }
    }

    pub fn count(&self, now: u64) -> u16 {
        if !self.counting {
            return self.reload as u16;
        }
        let n = self.reload as u64;
        let el = now.saturating_sub(self.load_time);
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
        let el = now.saturating_sub(self.load_time);
        match self.mode {
            0 | 1 => el >= n,
            2 => el % n != n - 1,
            3 => {
                if !self.gate {
                    return true;
                }
                el % n < n.div_ceil(2)
            }
            _ => true,
        }
    }

    /// Ticks between output events (IRQs) in the periodic modes.
    pub fn period(&self) -> Option<u64> {
        match self.mode {
            2 | 3 if self.counting => Some(self.reload as u64),
            _ => None,
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

#[derive(Clone, Debug)]
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
        p.ch[2].gate = false;
        p.schedule_irq0(0);
        p
    }

    fn schedule_irq0(&mut self, now: u64) {
        let c = &self.ch[0];
        self.next_irq0 = match (c.counting, c.period()) {
            (false, _) => None,
            (true, Some(p)) => {
                let el = now.saturating_sub(c.load_time);
                Some(c.load_time + (el / p + 1) * p)
            }
            (true, None) if c.mode == 0 && now < c.load_time + c.reload as u64 => Some(c.load_time + c.reload as u64),
            _ => None,
        };
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
            c.load_time = now;
            c.counting = true;
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

    #[test]
    fn square_wave_for_speaker() {
        let mut p = Pit::new();
        p.write(0x43, 0xb6, 0);
        p.write(0x42, 100, 0);
        p.write(0x42, 0, 0);
        let mut c = p.ch[2];
        c.gate = true;
        assert!(c.output(10) && !c.output(60) && c.output(110));
    }
}
