// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! 8259A interrupt controller (single, as on the PC/XT), ports 20h-21h.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Init {
    Ready,
    Icw2,
    Icw3,
    Icw4,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pic {
    irr: u8,
    isr: u8,
    pub imr: u8,
    base: u8,
    init: Init,
    single: bool,
    icw4: bool,
    read_isr: bool,
}

impl Default for Pic {
    fn default() -> Self {
        Self::new()
    }
}

impl Pic {
    /// As the BIOS leaves it: vectors 08h-0Fh, IRQ0, IRQ1 and IRQ6 unmasked.
    pub fn new() -> Self {
        Pic { irr: 0, isr: 0, imr: 0xbc, base: 8, init: Init::Ready, single: true, icw4: true, read_isr: false }
    }

    pub fn raise(&mut self, irq: u8) {
        self.irr |= 1 << irq;
    }

    /// Highest-priority request that is unmasked and not blocked by one in service.
    fn best(&self) -> Option<u8> {
        let req = self.irr & !self.imr;
        (0..8u8).take_while(|&i| self.isr & (1 << i) == 0).find(|&i| req & (1 << i) != 0)
    }

    pub fn has_pending(&self) -> bool {
        self.best().is_some()
    }

    /// Acknowledges the best pending request and returns its vector.
    pub fn acknowledge(&mut self) -> Option<u8> {
        let i = self.best()?;
        self.irr &= !(1 << i);
        self.isr |= 1 << i;
        Some(self.base + i)
    }

    /// Non-specific end of interrupt: clears the highest-priority in-service bit.
    pub fn eoi(&mut self) {
        self.isr &= self.isr.wrapping_sub(1);
    }

    pub fn write(&mut self, port: u16, v: u8) {
        if port & 1 == 0 {
            if v & 0x10 != 0 {
                self.single = v & 2 != 0;
                self.icw4 = v & 1 != 0;
                self.init = Init::Icw2;
                self.imr = 0;
                self.isr = 0;
                self.irr = 0;
            } else if v & 0x08 != 0 {
                if v & 2 != 0 {
                    self.read_isr = v & 1 != 0;
                }
            } else if v & 0x20 != 0 {
                if v & 0x40 != 0 {
                    self.isr &= !(1 << (v & 7));
                } else {
                    self.eoi();
                }
            }
            return;
        }
        self.init = match self.init {
            Init::Ready => {
                self.imr = v;
                Init::Ready
            }
            Init::Icw2 => {
                self.base = v & 0xf8;
                if !self.single {
                    Init::Icw3
                } else if self.icw4 {
                    Init::Icw4
                } else {
                    Init::Ready
                }
            }
            Init::Icw3 if self.icw4 => Init::Icw4,
            Init::Icw3 | Init::Icw4 => Init::Ready,
        };
    }

    pub fn read(&self, port: u16) -> u8 {
        if port & 1 == 0 {
            if self.read_isr { self.isr } else { self.irr }
        } else {
            self.imr
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_and_eoi() {
        let mut p = Pic::new();
        p.imr = 0;
        p.raise(1);
        p.raise(0);
        assert_eq!(p.acknowledge(), Some(8));
        assert_eq!(p.acknowledge(), None, "IRQ1 waits while IRQ0 is in service");
        p.eoi();
        assert_eq!(p.acknowledge(), Some(9));
        p.write(0x20, 0x20);
        assert!(!p.has_pending());
    }

    #[test]
    fn reinitialise() {
        let mut p = Pic::new();
        for (port, v) in [(0x20, 0x13), (0x21, 0x08), (0x21, 0x09), (0x21, 0xfe)] {
            p.write(port, v);
        }
        assert_eq!(p.imr, 0xfe);
        p.raise(1);
        assert_eq!(p.acknowledge(), None, "IRQ1 is masked");
        p.raise(0);
        assert_eq!(p.acknowledge(), Some(8));
    }
}
