// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Arithmetic and flag semantics. Values are carried in u32 with an explicit
//! operand width so byte and word forms share one implementation.

use crate::flags::*;
use crate::{AX, Cpu, DX};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum W {
    B,
    W,
}

impl W {
    #[inline]
    pub(crate) fn mask(self) -> u32 {
        match self {
            W::B => 0xff,
            W::W => 0xffff,
        }
    }

    #[inline]
    pub(crate) fn msb(self) -> u32 {
        match self {
            W::B => 0x80,
            W::W => 0x8000,
        }
    }

    #[inline]
    pub(crate) fn bits(self) -> u32 {
        match self {
            W::B => 8,
            W::W => 16,
        }
    }
}

#[inline]
fn parity(v: u32) -> bool {
    (v as u8).count_ones().is_multiple_of(2)
}

impl Cpu {
    #[inline]
    pub(crate) fn szp(&mut self, res: u32, w: W) {
        self.set_flag(ZF, res & w.mask() == 0);
        self.set_flag(SF, res & w.msb() != 0);
        self.set_flag(PF, parity(res));
    }

    pub(crate) fn add(&mut self, a: u32, b: u32, c: u32, w: W) -> u32 {
        let r = a + b + c;
        let res = r & w.mask();
        self.set_flag(CF, r > w.mask());
        self.set_flag(AF, (a ^ b ^ res) & 0x10 != 0);
        self.set_flag(OF, (a ^ res) & (b ^ res) & w.msb() != 0);
        self.szp(res, w);
        res
    }

    pub(crate) fn sub(&mut self, a: u32, b: u32, c: u32, w: W) -> u32 {
        let r = a.wrapping_sub(b).wrapping_sub(c);
        let res = r & w.mask();
        self.set_flag(CF, r > w.mask());
        self.set_flag(AF, (a ^ b ^ res) & 0x10 != 0);
        self.set_flag(OF, (a ^ b) & (a ^ res) & w.msb() != 0);
        self.szp(res, w);
        res
    }

    pub(crate) fn logic(&mut self, res: u32, w: W) -> u32 {
        let res = res & w.mask();
        self.set_flag(CF, false);
        self.set_flag(OF, false);
        self.set_flag(AF, false);
        self.szp(res, w);
        res
    }

    /// The eight ALU operations by encoding: ADD OR ADC SBB AND SUB XOR CMP.
    /// CMP returns the difference; callers do not store it.
    pub(crate) fn alu(&mut self, op: u8, a: u32, b: u32, w: W) -> u32 {
        match op & 7 {
            0 => self.add(a, b, 0, w),
            1 => self.logic(a | b, w),
            2 => {
                let c = self.flag(CF) as u32;
                self.add(a, b, c, w)
            }
            3 => {
                let c = self.flag(CF) as u32;
                self.sub(a, b, c, w)
            }
            4 => self.logic(a & b, w),
            6 => self.logic(a ^ b, w),
            _ => self.sub(a, b, 0, w),
        }
    }

    pub(crate) fn inc(&mut self, a: u32, w: W) -> u32 {
        let cf = self.flag(CF);
        let r = self.add(a, 1, 0, w);
        self.set_flag(CF, cf);
        r
    }

    pub(crate) fn dec(&mut self, a: u32, w: W) -> u32 {
        let cf = self.flag(CF);
        let r = self.sub(a, 1, 0, w);
        self.set_flag(CF, cf);
        r
    }

    /// Shift/rotate group by encoding: ROL ROR RCL RCR SHL SHR (6: none) SAR.
    /// The 8086 does not mask the count; flags come from the last single-bit
    /// step. Returns `None` for the undefined /6 form.
    pub(crate) fn shift(&mut self, op: u8, val: u32, count: u8, w: W) -> Option<u32> {
        if op == 6 {
            return None;
        }
        if count == 0 {
            return Some(val);
        }
        let msb = w.msb();
        let mask = w.mask();
        let mut v = val & mask;
        let mut cf = self.flag(CF);
        let mut of = self.flag(OF);
        for _ in 0..count {
            match op {
                0 => {
                    cf = v & msb != 0;
                    v = ((v << 1) | cf as u32) & mask;
                    of = (v & msb != 0) != cf;
                }
                1 => {
                    cf = v & 1 != 0;
                    v = (v >> 1) | if cf { msb } else { 0 };
                    of = (v ^ (v << 1)) & msb != 0;
                }
                2 => {
                    let out = v & msb != 0;
                    v = ((v << 1) | cf as u32) & mask;
                    cf = out;
                    of = (v & msb != 0) != cf;
                }
                3 => {
                    let out = v & 1 != 0;
                    v = (v >> 1) | if cf { msb } else { 0 };
                    cf = out;
                    of = (v ^ (v << 1)) & msb != 0;
                }
                4 => {
                    cf = v & msb != 0;
                    v = (v << 1) & mask;
                    of = (v & msb != 0) != cf;
                }
                5 => {
                    cf = v & 1 != 0;
                    of = v & msb != 0;
                    v >>= 1;
                }
                _ => {
                    cf = v & 1 != 0;
                    v = (v >> 1) | (v & msb);
                    of = false;
                }
            }
        }
        self.set_flag(CF, cf);
        self.set_flag(OF, of);
        if op >= 4 {
            self.szp(v, w);
            self.set_flag(AF, false);
        }
        Some(v)
    }

    pub(crate) fn mul8(&mut self, src: u8, signed: bool) {
        let al = self.reg8(0);
        let (r, overflow) = if signed {
            let r = (al as i8 as i16) * (src as i8 as i16);
            (r as u16, r as i8 as i16 != r)
        } else {
            let r = al as u16 * src as u16;
            (r, r > 0xff)
        };
        self.regs[AX] = r;
        self.set_flag(CF, overflow);
        self.set_flag(OF, overflow);
        self.szp((r >> 8) as u32, W::B);
    }

    pub(crate) fn mul16(&mut self, src: u16, signed: bool) {
        let ax = self.regs[AX];
        let (r, overflow) = if signed {
            let r = (ax as i16 as i32) * (src as i16 as i32);
            (r as u32, r as i16 as i32 != r)
        } else {
            let r = ax as u32 * src as u32;
            (r, r > 0xffff)
        };
        self.regs[AX] = r as u16;
        self.regs[DX] = (r >> 16) as u16;
        self.set_flag(CF, overflow);
        self.set_flag(OF, overflow);
        self.szp(r >> 16, W::W);
    }

    /// Unsigned `hi:lo / d` as the 8088 microcode (CORD) does it: a trial
    /// subtraction of the high half, then one restoring step per quotient bit.
    /// The flags are visible in the FLAGS a divide error pushes: AF OF SF ZF
    /// PF from the last trial subtraction, CF the complement of the quotient's
    /// top bit. Returns `None` if the quotient overflows (or `d` is zero).
    // CORD; MartyPC cpu_808x/muldiv.rs
    fn cord(&mut self, mut hi: u32, mut lo: u32, d: u32, w: W) -> Option<(u32, u32)> {
        let (mask, msb) = (w.mask(), w.msb());
        self.sub(hi, d, 0, w);
        if !self.flag(CF) {
            return None;
        }
        let mut q = 0;
        for _ in 0..w.bits() {
            let out = hi & msb != 0;
            hi = ((hi << 1) | (lo & msb != 0) as u32) & mask;
            lo = (lo << 1) & mask;
            // A bit shifted out of `hi` means it exceeds `d` without a trial.
            let fits = out || {
                self.sub(hi, d, 0, w);
                !self.flag(CF)
            };
            if fits {
                hi = hi.wrapping_sub(d) & mask;
            }
            q = (q << 1) | fits as u32;
        }
        self.set_flag(CF, q & msb == 0);
        Some((q, hi))
    }

    /// DIV/IDIV of `hi:lo` by `d`: `(quotient, remainder)`, or `None` on a
    /// divide error. IDIV divides magnitudes and rejects a quotient magnitude
    /// with the top bit set, so -128/-32768 fault as well. `negate` (a REP
    /// prefix) flips the sign of an IDIV quotient; DIV ignores it.
    // PREIDIV, POSTIDIV; MartyPC cpu_808x/muldiv.rs
    pub(crate) fn div(&mut self, hi: u32, lo: u32, d: u32, w: W, signed: bool, negate: bool) -> Option<(u32, u32)> {
        if !signed {
            return self.cord(hi, lo, d, w);
        }
        let (mask, msb) = (w.mask(), w.msb());
        let n_neg = hi & msb != 0;
        let d_neg = d & msb != 0;
        let (hi, lo) = if n_neg {
            let hi = if lo == 0 { hi.wrapping_neg() } else { !hi };
            (hi & mask, lo.wrapping_neg() & mask)
        } else {
            (hi, lo)
        };
        let d = if d_neg { d.wrapping_neg() & mask } else { d };
        let (q, r) = self.cord(hi, lo, d, w)?;
        if q & msb != 0 {
            return None;
        }
        self.set_flag(CF, false);
        self.set_flag(OF, false);
        let q = if negate ^ n_neg ^ d_neg { q.wrapping_neg() & mask } else { q };
        let r = if n_neg { r.wrapping_neg() & mask } else { r };
        Some((q, r))
    }

    /// DAA. With AF set the 8088 tests AL > 9F, not AL > 99, for the high digit.
    // 9F threshold; MartyPC cpu_808x/bcd.rs
    pub(crate) fn daa(&mut self) {
        let old_al = self.reg8(0);
        let old_cf = self.flag(CF);
        let old_af = self.flag(AF);
        let mut al = old_al;
        if (al & 0x0f) > 9 || old_af {
            al = al.wrapping_add(6);
            self.set_flag(AF, true);
        } else {
            self.set_flag(AF, false);
        }
        let limit = if old_af { 0x9f } else { 0x99 };
        if old_al > limit || old_cf {
            al = al.wrapping_add(0x60);
            self.set_flag(CF, true);
        } else {
            self.set_flag(CF, false);
        }
        self.set_reg8(0, al);
        self.szp(al as u32, W::B);
    }

    /// DAS, with DAA's high-digit test. A borrow from the low-digit adjust
    /// does not set CF.
    pub(crate) fn das(&mut self) {
        let old_al = self.reg8(0);
        let old_cf = self.flag(CF);
        let old_af = self.flag(AF);
        let mut al = old_al;
        if (al & 0x0f) > 9 || old_af {
            al = al.wrapping_sub(6);
            self.set_flag(AF, true);
        } else {
            self.set_flag(AF, false);
        }
        let limit = if old_af { 0x9f } else { 0x99 };
        if old_al > limit || old_cf {
            al = al.wrapping_sub(0x60);
            self.set_flag(CF, true);
        } else {
            self.set_flag(CF, false);
        }
        self.set_reg8(0, al);
        self.szp(al as u32, W::B);
    }

    /// AAA, 8086 form: AL += 6 does not carry into AH.
    pub(crate) fn aaa(&mut self) {
        let mut al = self.reg8(0);
        let mut ah = self.reg8(4);
        let adjust = (al & 0x0f) > 9 || self.flag(AF);
        if adjust {
            al = al.wrapping_add(6);
            ah = ah.wrapping_add(1);
        }
        self.set_flag(AF, adjust);
        self.set_flag(CF, adjust);
        self.set_reg8(0, al & 0x0f);
        self.set_reg8(4, ah);
    }

    pub(crate) fn aas(&mut self) {
        let mut al = self.reg8(0);
        let mut ah = self.reg8(4);
        let adjust = (al & 0x0f) > 9 || self.flag(AF);
        if adjust {
            al = al.wrapping_sub(6);
            ah = ah.wrapping_sub(1);
        }
        self.set_flag(AF, adjust);
        self.set_flag(CF, adjust);
        self.set_reg8(0, al & 0x0f);
        self.set_reg8(4, ah);
    }

    /// AAM runs the divide routine on 0:AL. Returns false for a zero base (a
    /// divide error), leaving the flags of the trial subtraction 0 - 0.
    pub(crate) fn aam(&mut self, base: u8) -> bool {
        let al = self.reg8(0) as u32;
        let Some((q, r)) = self.cord(0, al, base as u32, W::B) else {
            return false;
        };
        self.set_reg8(4, q as u8);
        self.set_reg8(0, r as u8);
        self.szp(r, W::B);
        true
    }

    pub(crate) fn aad(&mut self, base: u8) {
        let al = self.reg8(0) as u32;
        let prod = (self.reg8(4) as u32 * base as u32) & 0xff;
        let r = self.add(al, prod, 0, W::B);
        self.set_reg8(0, r as u8);
        self.set_reg8(4, 0);
    }
}
