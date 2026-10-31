// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Instruction decode and execution. Cycle counts are rough 8088 figures,
//! good enough to pace the emulated clock, not cycle-exact.

use crate::alu::W;
use crate::flags::*;
use crate::*;

#[derive(Clone, Copy, Debug)]
enum Rm {
    Reg(u8),
    /// Segment value and offset.
    Mem(u16, u16),
}

#[derive(Clone, Copy, Debug)]
struct ModRm {
    reg: u8,
    rm: Rm,
    /// Effective-address calculation cycles (0 for a register operand).
    ea: u32,
}

impl ModRm {
    fn is_mem(&self) -> bool {
        matches!(self.rm, Rm::Mem(..))
    }
}

const MAX_PREFIXES: usize = 16;

fn is_prefix(b: u8) -> bool {
    matches!(b, 0x26 | 0x2e | 0x36 | 0x3e | 0xf0 | 0xf2 | 0xf3)
}

impl Cpu {
    #[inline]
    fn fetch8<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u8 {
        let v = bus.read8(phys(self.sregs[CS], self.ip));
        self.ip = self.ip.wrapping_add(1);
        v
    }

    #[inline]
    fn fetch16<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u16 {
        let lo = self.fetch8(bus) as u16;
        let hi = self.fetch8(bus) as u16;
        lo | (hi << 8)
    }

    /// Segment for DS-relative accesses, honouring a segment-override prefix.
    #[inline]
    fn data_seg(&self) -> u16 {
        self.sregs[self.seg_override.unwrap_or(DS)]
    }

    fn modrm<B: Bus + ?Sized>(&mut self, bus: &mut B) -> ModRm {
        let b = self.fetch8(bus);
        let md = b >> 6;
        let reg = (b >> 3) & 7;
        let rm = b & 7;
        if md == 3 {
            return ModRm { reg, rm: Rm::Reg(rm), ea: 0 };
        }
        let r = &self.regs;
        let (mut off, bp_based, mut ea) = match rm {
            0 => (r[BX].wrapping_add(r[SI]), false, 7),
            1 => (r[BX].wrapping_add(r[DI]), false, 8),
            2 => (r[BP].wrapping_add(r[SI]), true, 8),
            3 => (r[BP].wrapping_add(r[DI]), true, 7),
            4 => (r[SI], false, 5),
            5 => (r[DI], false, 5),
            6 if md == 0 => (0, false, 6),
            6 => (r[BP], true, 5),
            _ => (r[BX], false, 5),
        };
        match md {
            0 => {
                if rm == 6 {
                    off = self.fetch16(bus);
                }
            }
            1 => {
                off = off.wrapping_add(self.fetch8(bus) as i8 as u16);
                ea += 4;
            }
            _ => {
                off = off.wrapping_add(self.fetch16(bus));
                ea += 4;
            }
        }
        let seg = match self.seg_override {
            Some(s) => {
                ea += 2;
                s
            }
            None if bp_based => SS,
            None => DS,
        };
        ModRm { reg, rm: Rm::Mem(self.sregs[seg], off), ea }
    }

    #[inline]
    fn get_reg(&self, r: u8, w: W) -> u32 {
        match w {
            W::B => self.reg8(r) as u32,
            W::W => self.regs[r as usize] as u32,
        }
    }

    #[inline]
    fn set_reg(&mut self, r: u8, w: W, v: u32) {
        match w {
            W::B => self.set_reg8(r, v as u8),
            W::W => self.regs[r as usize] = v as u16,
        }
    }

    fn get_rm<B: Bus + ?Sized>(&mut self, bus: &mut B, rm: Rm, w: W) -> u32 {
        match (rm, w) {
            (Rm::Reg(r), _) => self.get_reg(r, w),
            (Rm::Mem(s, o), W::B) => self.read8(bus, s, o) as u32,
            (Rm::Mem(s, o), W::W) => self.read16(bus, s, o) as u32,
        }
    }

    fn set_rm<B: Bus + ?Sized>(&mut self, bus: &mut B, rm: Rm, w: W, v: u32) {
        match (rm, w) {
            (Rm::Reg(r), _) => self.set_reg(r, w, v),
            (Rm::Mem(s, o), W::B) => self.write8(bus, s, o, v as u8),
            (Rm::Mem(s, o), W::W) => self.write16(bus, s, o, v as u16),
        }
    }

    fn cond(&self, cc: u8) -> bool {
        let (c, z, s, o, p) = (self.flag(CF), self.flag(ZF), self.flag(SF), self.flag(OF), self.flag(PF));
        let r = match cc >> 1 {
            0 => o,
            1 => c,
            2 => z,
            3 => c || z,
            4 => s,
            5 => p,
            6 => s != o,
            _ => z || (s != o),
        };
        r != (cc & 1 != 0)
    }

    /// Rewinds to the instruction at `instr_cs:instr_ip` and reports its
    /// opcode and the byte after it, found again past the prefixes; a run of
    /// more than MAX_PREFIXES prefixes reports its first two bytes.
    fn invalid(&mut self, bus: &mut (impl Bus + ?Sized)) -> Step {
        let (cs, ip) = (self.instr_cs, self.instr_ip);
        let at = (0..=MAX_PREFIXES as u16)
            .map(|i| ip.wrapping_add(i))
            .find(|&a| !is_prefix(bus.read8(phys(cs, a))))
            .unwrap_or(ip);
        let bytes = [bus.read8(phys(cs, at)), bus.read8(phys(cs, at.wrapping_add(1)))];
        self.ip = ip;
        self.sregs[CS] = cs;
        Step::Invalid { cs, ip, bytes }
    }

    /// Executes one instruction (one iteration of a REP string instruction).
    pub fn step<B: Bus + ?Sized>(&mut self, bus: &mut B) -> Step {
        if self.halted {
            return Step::Halted;
        }
        self.instr_cs = self.sregs[CS];
        self.instr_ip = self.ip;
        self.seg_override = None;
        self.rep = Rep::None;
        self.rep_pending = false;
        self.irq_inhibit = false;
        let mut cycles = 0;
        let mut prefixes = 0;
        let op = loop {
            let b = self.fetch8(bus);
            match b {
                0x26 | 0x2e | 0x36 | 0x3e => self.seg_override = Some(((b >> 3) & 3) as usize),
                0xf0 => {}
                0xf2 => self.rep = Rep::Nz,
                0xf3 => self.rep = Rep::Z,
                _ => break b,
            }
            cycles += 2;
            prefixes += 1;
            if prefixes > MAX_PREFIXES {
                return self.invalid(bus);
            }
        };
        self.exec(bus, op, cycles)
    }

    fn exec<B: Bus + ?Sized>(&mut self, bus: &mut B, op: u8, mut cyc: u32) -> Step {
        match op {
            // ALU ops in their six forms: r/m,r  r,r/m  acc,imm.
            0x00..=0x3f if op & 7 < 6 => {
                let alu = (op >> 3) & 7;
                match op & 7 {
                    0..=3 => {
                        let w = if op & 1 == 0 { W::B } else { W::W };
                        let m = self.modrm(bus);
                        let to_reg = op & 2 != 0;
                        let r = self.get_reg(m.reg, w);
                        let v = self.get_rm(bus, m.rm, w);
                        let res = if to_reg { self.alu(alu, r, v, w) } else { self.alu(alu, v, r, w) };
                        if alu != 7 {
                            if to_reg {
                                self.set_reg(m.reg, w, res);
                            } else {
                                self.set_rm(bus, m.rm, w, res);
                            }
                        }
                        cyc += match (m.is_mem(), to_reg || alu == 7) {
                            (false, _) => 3,
                            (true, true) => 9 + m.ea,
                            (true, false) => 16 + m.ea,
                        };
                    }
                    4 => {
                        let imm = self.fetch8(bus) as u32;
                        let res = self.alu(alu, self.reg8(0) as u32, imm, W::B);
                        if alu != 7 {
                            self.set_reg8(0, res as u8);
                        }
                        cyc += 4;
                    }
                    _ => {
                        let imm = self.fetch16(bus) as u32;
                        let res = self.alu(alu, self.regs[AX] as u32, imm, W::W);
                        if alu != 7 {
                            self.regs[AX] = res as u16;
                        }
                        cyc += 4;
                    }
                }
            }
            0x06 | 0x0e | 0x16 | 0x1e => {
                let v = self.sregs[(op >> 3) as usize];
                self.push(bus, v);
                cyc += 14;
            }
            0x07 | 0x17 | 0x1f => {
                let s = (op >> 3) as usize;
                self.sregs[s] = self.pop(bus);
                if s == SS {
                    self.irq_inhibit = true;
                }
                cyc += 12;
            }
            0x27 => {
                self.daa();
                cyc += 4;
            }
            0x2f => {
                self.das();
                cyc += 4;
            }
            0x37 => {
                self.aaa();
                cyc += 8;
            }
            0x3f => {
                self.aas();
                cyc += 8;
            }
            0x40..=0x47 => {
                let r = (op & 7) as usize;
                self.regs[r] = self.inc(self.regs[r] as u32, W::W) as u16;
                cyc += 3;
            }
            0x48..=0x4f => {
                let r = (op & 7) as usize;
                self.regs[r] = self.dec(self.regs[r] as u32, W::W) as u16;
                cyc += 3;
            }
            0x50..=0x57 => {
                let r = (op & 7) as usize;
                // The 8086 pushes SP's already-decremented value.
                let v = if r == SP { self.regs[SP].wrapping_sub(2) } else { self.regs[r] };
                self.push(bus, v);
                cyc += 15;
            }
            0x58..=0x5f => {
                let v = self.pop(bus);
                self.regs[(op & 7) as usize] = v;
                cyc += 12;
            }
            0x70..=0x7f => {
                let d = self.fetch8(bus) as i8 as u16;
                if self.cond(op & 0xf) {
                    self.ip = self.ip.wrapping_add(d);
                    cyc += 16;
                } else {
                    cyc += 4;
                }
            }
            0x80..=0x83 => {
                let w = if op & 1 == 0 { W::B } else { W::W };
                let m = self.modrm(bus);
                let imm = match op {
                    0x81 => self.fetch16(bus) as u32,
                    0x83 => self.fetch8(bus) as i8 as u16 as u32,
                    _ => self.fetch8(bus) as u32,
                };
                let a = self.get_rm(bus, m.rm, w);
                let res = self.alu(m.reg, a, imm, w);
                if m.reg != 7 {
                    self.set_rm(bus, m.rm, w, res);
                }
                cyc += if m.is_mem() { 17 + m.ea } else { 4 };
            }
            0x84 | 0x85 => {
                let w = if op & 1 == 0 { W::B } else { W::W };
                let m = self.modrm(bus);
                let a = self.get_rm(bus, m.rm, w);
                let b = self.get_reg(m.reg, w);
                self.logic(a & b, w);
                cyc += if m.is_mem() { 9 + m.ea } else { 3 };
            }
            0x86 | 0x87 => {
                let w = if op & 1 == 0 { W::B } else { W::W };
                let m = self.modrm(bus);
                let a = self.get_rm(bus, m.rm, w);
                let b = self.get_reg(m.reg, w);
                self.set_rm(bus, m.rm, w, b);
                self.set_reg(m.reg, w, a);
                cyc += if m.is_mem() { 17 + m.ea } else { 4 };
            }
            0x88..=0x8b => {
                let w = if op & 1 == 0 { W::B } else { W::W };
                let m = self.modrm(bus);
                if op & 2 == 0 {
                    let v = self.get_reg(m.reg, w);
                    self.set_rm(bus, m.rm, w, v);
                } else {
                    let v = self.get_rm(bus, m.rm, w);
                    self.set_reg(m.reg, w, v);
                }
                cyc += if m.is_mem() { 9 + m.ea } else { 2 };
            }
            0x8c => {
                let m = self.modrm(bus);
                let v = self.sregs[(m.reg & 3) as usize] as u32;
                self.set_rm(bus, m.rm, W::W, v);
                cyc += if m.is_mem() { 9 + m.ea } else { 2 };
            }
            0x8d => {
                let m = self.modrm(bus);
                match m.rm {
                    Rm::Mem(_, off) => self.regs[m.reg as usize] = off,
                    Rm::Reg(_) => return self.invalid(bus),
                }
                cyc += 2 + m.ea;
            }
            0x8e => {
                let m = self.modrm(bus);
                let v = self.get_rm(bus, m.rm, W::W) as u16;
                let s = (m.reg & 3) as usize;
                self.sregs[s] = v;
                if s == SS {
                    self.irq_inhibit = true;
                }
                cyc += if m.is_mem() { 8 + m.ea } else { 2 };
            }
            0x8f => {
                let m = self.modrm(bus);
                // 8F /1-/7 are undefined.
                if m.reg != 0 {
                    return self.invalid(bus);
                }
                let v = self.pop(bus) as u32;
                self.set_rm(bus, m.rm, W::W, v);
                cyc += 17 + m.ea;
            }
            0x90..=0x97 => {
                self.regs.swap(AX, (op & 7) as usize);
                cyc += 3;
            }
            0x98 => {
                self.regs[AX] = self.reg8(0) as i8 as i16 as u16;
                cyc += 2;
            }
            0x99 => {
                self.regs[DX] = if self.regs[AX] & 0x8000 != 0 { 0xffff } else { 0 };
                cyc += 5;
            }
            0x9a => {
                let off = self.fetch16(bus);
                let seg = self.fetch16(bus);
                let (cs, ip) = (self.sregs[CS], self.ip);
                self.push(bus, cs);
                self.push(bus, ip);
                self.sregs[CS] = seg;
                self.ip = off;
                cyc += 36;
            }
            0x9b => cyc += 4,
            0x9c => {
                let f = self.flags();
                self.push(bus, f);
                cyc += 14;
            }
            0x9d => {
                let f = self.pop(bus);
                self.set_flags(f);
                cyc += 12;
            }
            0x9e => {
                let f = (self.flags() & 0xff00) | self.reg8(4) as u16;
                self.set_flags(f);
                cyc += 4;
            }
            0x9f => {
                self.set_reg8(4, self.flags() as u8);
                cyc += 4;
            }
            0xa0..=0xa3 => {
                let off = self.fetch16(bus);
                let seg = self.data_seg();
                match op {
                    0xa0 => {
                        let v = self.read8(bus, seg, off);
                        self.set_reg8(0, v);
                    }
                    0xa1 => self.regs[AX] = self.read16(bus, seg, off),
                    0xa2 => {
                        let v = self.reg8(0);
                        self.write8(bus, seg, off, v);
                    }
                    _ => {
                        let v = self.regs[AX];
                        self.write16(bus, seg, off, v);
                    }
                }
                cyc += 14;
            }
            0xa4..=0xa7 | 0xaa..=0xaf => cyc += self.string_op(bus, op),
            0xa8 => {
                let imm = self.fetch8(bus) as u32;
                self.logic(self.reg8(0) as u32 & imm, W::B);
                cyc += 4;
            }
            0xa9 => {
                let imm = self.fetch16(bus) as u32;
                self.logic(self.regs[AX] as u32 & imm, W::W);
                cyc += 4;
            }
            0xb0..=0xb7 => {
                let v = self.fetch8(bus);
                self.set_reg8(op & 7, v);
                cyc += 4;
            }
            0xb8..=0xbf => {
                self.regs[(op & 7) as usize] = self.fetch16(bus);
                cyc += 4;
            }
            0xc2 | 0xc3 => {
                let n = if op == 0xc2 { self.fetch16(bus) } else { 0 };
                self.ip = self.pop(bus);
                self.regs[SP] = self.regs[SP].wrapping_add(n);
                cyc += 20;
            }
            0xc4 | 0xc5 => {
                let m = self.modrm(bus);
                let Rm::Mem(s, o) = m.rm else {
                    return self.invalid(bus);
                };
                let off = self.read16(bus, s, o);
                let seg = self.read16(bus, s, o.wrapping_add(2));
                self.regs[m.reg as usize] = off;
                self.sregs[if op == 0xc4 { ES } else { DS }] = seg;
                cyc += 24 + m.ea;
            }
            0xc6 | 0xc7 => {
                let w = if op & 1 == 0 { W::B } else { W::W };
                let m = self.modrm(bus);
                // C6/C7 /1-/7 are undefined.
                if m.reg != 0 {
                    return self.invalid(bus);
                }
                let imm = if w == W::B { self.fetch8(bus) as u32 } else { self.fetch16(bus) as u32 };
                self.set_rm(bus, m.rm, w, imm);
                cyc += if m.is_mem() { 10 + m.ea } else { 4 };
            }
            0xca | 0xcb => {
                let n = if op == 0xca { self.fetch16(bus) } else { 0 };
                self.ip = self.pop(bus);
                self.sregs[CS] = self.pop(bus);
                self.regs[SP] = self.regs[SP].wrapping_add(n);
                cyc += 34;
            }
            0xcc => {
                self.interrupt(bus, 3);
                cyc += 52;
            }
            0xcd => {
                let n = self.fetch8(bus);
                self.interrupt(bus, n);
                cyc += 51;
            }
            0xce => {
                if self.flag(OF) {
                    self.interrupt(bus, 4);
                    cyc += 53;
                } else {
                    cyc += 4;
                }
            }
            0xcf => {
                self.ip = self.pop(bus);
                self.sregs[CS] = self.pop(bus);
                let f = self.pop(bus);
                self.set_flags(f);
                cyc += 32;
            }
            0xd0..=0xd3 => {
                let w = if op & 1 == 0 { W::B } else { W::W };
                let m = self.modrm(bus);
                let count = if op & 2 == 0 { 1 } else { self.reg8(1) };
                let v = self.get_rm(bus, m.rm, w);
                let Some(res) = self.shift(m.reg, v, count, w) else {
                    return self.invalid(bus);
                };
                self.set_rm(bus, m.rm, w, res);
                cyc += if m.is_mem() { 15 + m.ea } else { 2 };
                if op & 2 != 0 {
                    cyc += 6 + 4 * count as u32;
                }
            }
            0xd4 => {
                let base = self.fetch8(bus);
                if !self.aam(base) {
                    self.interrupt(bus, 0);
                }
                cyc += 83;
            }
            0xd5 => {
                let base = self.fetch8(bus);
                self.aad(base);
                cyc += 60;
            }
            0xd7 => {
                let off = self.regs[BX].wrapping_add(self.reg8(0) as u16);
                let seg = self.data_seg();
                let v = self.read8(bus, seg, off);
                self.set_reg8(0, v);
                cyc += 11;
            }
            0xe0..=0xe3 => {
                let d = self.fetch8(bus) as i8 as u16;
                let take = if op == 0xe3 {
                    self.regs[CX] == 0
                } else {
                    self.regs[CX] = self.regs[CX].wrapping_sub(1);
                    let cx_nz = self.regs[CX] != 0;
                    match op {
                        0xe0 => cx_nz && !self.flag(ZF),
                        0xe1 => cx_nz && self.flag(ZF),
                        _ => cx_nz,
                    }
                };
                if take {
                    self.ip = self.ip.wrapping_add(d);
                    cyc += 17;
                } else {
                    cyc += 5;
                }
            }
            0xe4 | 0xe5 | 0xec | 0xed => {
                let port = if op & 8 == 0 { self.fetch8(bus) as u16 } else { self.regs[DX] };
                if op & 1 == 0 {
                    let v = bus.io_read8(port);
                    self.set_reg8(0, v);
                } else {
                    self.regs[AX] = bus.io_read16(port);
                }
                cyc += 10;
            }
            0xe6 | 0xe7 | 0xee | 0xef => {
                let port = if op & 8 == 0 { self.fetch8(bus) as u16 } else { self.regs[DX] };
                if op & 1 == 0 {
                    bus.io_write8(port, self.reg8(0));
                } else {
                    bus.io_write16(port, self.regs[AX]);
                }
                cyc += 10;
            }
            0xe8 => {
                let d = self.fetch16(bus);
                let ip = self.ip;
                self.push(bus, ip);
                self.ip = ip.wrapping_add(d);
                cyc += 23;
            }
            0xe9 => {
                let d = self.fetch16(bus);
                self.ip = self.ip.wrapping_add(d);
                cyc += 15;
            }
            0xea => {
                let off = self.fetch16(bus);
                let seg = self.fetch16(bus);
                self.ip = off;
                self.sregs[CS] = seg;
                cyc += 15;
            }
            0xeb => {
                let d = self.fetch8(bus) as i8 as u16;
                self.ip = self.ip.wrapping_add(d);
                cyc += 15;
            }
            0xf4 => {
                self.halted = true;
                return Step::Halted;
            }
            0xf5 => {
                let c = self.flag(CF);
                self.set_flag(CF, !c);
                cyc += 2;
            }
            0xf6 | 0xf7 => return self.group3(bus, op, cyc),
            0xf8 | 0xf9 => {
                self.set_flag(CF, op & 1 != 0);
                cyc += 2;
            }
            0xfa => {
                self.set_flag(IF, false);
                cyc += 2;
            }
            0xfb => {
                if !self.flag(IF) {
                    self.irq_inhibit = true;
                }
                self.set_flag(IF, true);
                cyc += 2;
            }
            0xfc | 0xfd => {
                self.set_flag(DF, op & 1 != 0);
                cyc += 2;
            }
            0xfe => {
                let m = self.modrm(bus);
                match m.reg {
                    0 | 1 => {
                        let v = self.get_rm(bus, m.rm, W::B);
                        let r = if m.reg == 0 { self.inc(v, W::B) } else { self.dec(v, W::B) };
                        self.set_rm(bus, m.rm, W::B, r);
                        cyc += if m.is_mem() { 15 + m.ea } else { 3 };
                    }
                    7 if matches!(m.rm, Rm::Reg(0)) => {
                        let number = self.fetch8(bus);
                        return Step::Trap { number, cycles: cyc + 4 };
                    }
                    _ => return self.invalid(bus),
                }
            }
            0xff => return self.group5(bus, cyc),
            // 0F (POP CS), 60-6F, C0/C1, C8/C9, D6 (SALC), D8-DF (ESC), F1.
            _ => return self.invalid(bus),
        }
        Step::Executed { cycles: cyc }
    }

    fn group3<B: Bus + ?Sized>(&mut self, bus: &mut B, op: u8, mut cyc: u32) -> Step {
        let w = if op & 1 == 0 { W::B } else { W::W };
        let m = self.modrm(bus);
        let mem = if m.is_mem() { m.ea + 4 } else { 0 };
        match m.reg {
            // /1 is an alias of TEST on the 8086.
            0 | 1 => {
                let imm = if w == W::B { self.fetch8(bus) as u32 } else { self.fetch16(bus) as u32 };
                let v = self.get_rm(bus, m.rm, w);
                self.logic(v & imm, w);
                cyc += 5 + mem;
            }
            2 => {
                let v = self.get_rm(bus, m.rm, w);
                self.set_rm(bus, m.rm, w, !v & w.mask());
                cyc += 3 + 2 * mem;
            }
            3 => {
                let v = self.get_rm(bus, m.rm, w);
                let r = self.sub(0, v, 0, w);
                self.set_rm(bus, m.rm, w, r);
                cyc += 3 + 2 * mem;
            }
            // The 8088's REP-prefix negation of the product is not modelled.
            4 | 5 => {
                let v = self.get_rm(bus, m.rm, w);
                let signed = m.reg == 5;
                match w {
                    W::B => self.mul8(v as u8, signed),
                    W::W => self.mul16(v as u16, signed),
                }
                cyc += if w == W::B { 75 } else { 120 } + mem;
            }
            _ => {
                let v = self.get_rm(bus, m.rm, w);
                let signed = m.reg == 7;
                let (hi, lo) = match w {
                    W::B => (self.reg8(4) as u32, self.reg8(0) as u32),
                    W::W => (self.regs[DX] as u32, self.regs[AX] as u32),
                };
                match self.div(hi, lo, v, w, signed, self.rep != Rep::None) {
                    Some((q, r)) => {
                        self.set_reg(0, w, q);
                        self.set_reg(if w == W::B { 4 } else { DX as u8 }, w, r);
                    }
                    // The 8086 returns to the instruction after the DIV.
                    None => self.interrupt(bus, 0),
                }
                cyc += if w == W::B { 85 } else { 150 } + mem;
            }
        }
        Step::Executed { cycles: cyc }
    }

    fn group5<B: Bus + ?Sized>(&mut self, bus: &mut B, mut cyc: u32) -> Step {
        let m = self.modrm(bus);
        match m.reg {
            0 | 1 => {
                let v = self.get_rm(bus, m.rm, W::W);
                let r = if m.reg == 0 { self.inc(v, W::W) } else { self.dec(v, W::W) };
                self.set_rm(bus, m.rm, W::W, r);
                cyc += if m.is_mem() { 15 + m.ea } else { 3 };
            }
            2 => {
                let target = self.get_rm(bus, m.rm, W::W) as u16;
                let ip = self.ip;
                self.push(bus, ip);
                self.ip = target;
                cyc += 21 + m.ea;
            }
            3 | 5 => {
                let Rm::Mem(s, o) = m.rm else {
                    return self.invalid(bus);
                };
                let off = self.read16(bus, s, o);
                let seg = self.read16(bus, s, o.wrapping_add(2));
                if m.reg == 3 {
                    let (cs, ip) = (self.sregs[CS], self.ip);
                    self.push(bus, cs);
                    self.push(bus, ip);
                    cyc += 37 + m.ea;
                } else {
                    cyc += 24 + m.ea;
                }
                self.sregs[CS] = seg;
                self.ip = off;
            }
            4 => {
                self.ip = self.get_rm(bus, m.rm, W::W) as u16;
                cyc += 11 + m.ea;
            }
            6 => {
                let v = match m.rm {
                    Rm::Reg(r) if r as usize == SP => self.regs[SP].wrapping_sub(2),
                    _ => self.get_rm(bus, m.rm, W::W) as u16,
                };
                self.push(bus, v);
                cyc += 16 + m.ea;
            }
            _ => return self.invalid(bus),
        }
        Step::Executed { cycles: cyc }
    }

    /// One iteration of MOVS/CMPS/STOS/LODS/SCAS, repeating via IP rewind.
    fn string_op<B: Bus + ?Sized>(&mut self, bus: &mut B, op: u8) -> u32 {
        let repeated = self.rep != Rep::None;
        if repeated && self.regs[CX] == 0 {
            return 9;
        }
        let w = if op & 1 == 0 { W::B } else { W::W };
        let size: u16 = if w == W::B { 1 } else { 2 };
        let delta = if self.flag(DF) { size.wrapping_neg() } else { size };
        let src = self.data_seg();
        let es = self.sregs[ES];
        let (si, di) = (self.regs[SI], self.regs[DI]);
        let read = |cpu: &mut Self, bus: &mut B, seg, off| -> u32 {
            if w == W::B { cpu.read8(bus, seg, off) as u32 } else { cpu.read16(bus, seg, off) as u32 }
        };
        let write = |cpu: &mut Self, bus: &mut B, seg, off, v: u32| {
            if w == W::B { cpu.write8(bus, seg, off, v as u8) } else { cpu.write16(bus, seg, off, v as u16) }
        };
        let compares = matches!(op, 0xa6 | 0xa7 | 0xae | 0xaf);
        match op {
            0xa4 | 0xa5 => {
                let v = read(self, bus, src, si);
                write(self, bus, es, di, v);
                self.regs[SI] = si.wrapping_add(delta);
                self.regs[DI] = di.wrapping_add(delta);
            }
            0xa6 | 0xa7 => {
                let a = read(self, bus, src, si);
                let b = read(self, bus, es, di);
                self.sub(a, b, 0, w);
                self.regs[SI] = si.wrapping_add(delta);
                self.regs[DI] = di.wrapping_add(delta);
            }
            0xaa | 0xab => {
                let v = self.get_reg(0, w);
                write(self, bus, es, di, v);
                self.regs[DI] = di.wrapping_add(delta);
            }
            0xac | 0xad => {
                let v = read(self, bus, src, si);
                self.set_reg(0, w, v);
                self.regs[SI] = si.wrapping_add(delta);
            }
            _ => {
                let a = self.get_reg(0, w);
                let b = read(self, bus, es, di);
                self.sub(a, b, 0, w);
                self.regs[DI] = di.wrapping_add(delta);
            }
        }
        if repeated {
            self.regs[CX] = self.regs[CX].wrapping_sub(1);
            let more = self.regs[CX] != 0 && (!compares || self.flag(ZF) == (self.rep == Rep::Z));
            if more {
                self.ip = self.instr_ip;
                self.rep_pending = true;
            }
        }
        if compares { 22 } else { 17 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 MiB of RAM and no I/O devices.
    struct Ram(Vec<u8>);

    impl Bus for Ram {
        fn read8(&mut self, addr: u32) -> u8 {
            self.0[addr as usize]
        }
        fn write8(&mut self, addr: u32, val: u8) {
            self.0[addr as usize] = val;
        }
        fn io_read8(&mut self, _port: u16) -> u8 {
            0xff
        }
        fn io_write8(&mut self, _port: u16, _val: u8) {}
    }

    const CODE_SEG: u16 = 0x1000;

    /// A CPU at 1000:0100 running `code`, with SS:SP = 2000:0100.
    fn rig(code: &[u8]) -> (Cpu, Ram) {
        let mut bus = Ram(vec![0; 1 << 20]);
        let at = phys(CODE_SEG, 0x100) as usize;
        bus.0[at..at + code.len()].copy_from_slice(code);
        let mut cpu = Cpu::new();
        cpu.sregs = [0x3000, CODE_SEG, 0x2000, 0x3000];
        cpu.ip = 0x100;
        cpu.regs[SP] = 0x100;
        (cpu, bus)
    }

    fn invalid_bytes(code: &[u8]) -> [u8; 2] {
        let (mut cpu, mut bus) = rig(code);
        match cpu.step(&mut bus) {
            Step::Invalid { cs, ip, bytes } => {
                assert_eq!((cs, ip), (CODE_SEG, 0x100), "{code:02X?}");
                assert_eq!((cpu.sregs[CS], cpu.ip), (CODE_SEG, 0x100), "{code:02X?}");
                bytes
            }
            s => panic!("{code:02X?}: {s:?}"),
        }
    }

    #[test]
    fn invalid_reports_the_opcode_and_the_byte_after_it() {
        // Opcodes found invalid after their ModRM and displacement were read.
        assert_eq!(invalid_bytes(&[0x8d, 0xc0, 0x90]), [0x8d, 0xc0]);
        assert_eq!(invalid_bytes(&[0x2e, 0xf3, 0xc4, 0xc0, 0x90]), [0xc4, 0xc0]);
        assert_eq!(invalid_bytes(&[0xd0, 0xb6, 0x34, 0x12, 0x90]), [0xd0, 0xb6]);
        assert_eq!(invalid_bytes(&[0xfe, 0x10, 0x90]), [0xfe, 0x10]);
        assert_eq!(invalid_bytes(&[0x26, 0xff, 0xff, 0x90]), [0xff, 0xff]);
        assert_eq!(invalid_bytes(&[0xc6, 0x8e, 0x34, 0x12, 0x56, 0x90]), [0xc6, 0x8e]);
        assert_eq!(invalid_bytes(&[0x0f, 0x05]), [0x0f, 0x05]);
        let mut prefixes = vec![0x26; MAX_PREFIXES + 1];
        prefixes.push(0x90);
        assert_eq!(invalid_bytes(&prefixes), [0x26, 0x26]);
        // Sixteen prefixes still make an instruction.
        let (mut cpu, mut bus) = rig(&[[0x26; MAX_PREFIXES].as_slice(), &[0x90]].concat());
        assert!(matches!(cpu.step(&mut bus), Step::Executed { .. }));
    }

    #[test]
    fn undefined_pop_and_mov_immediate_forms_are_invalid() {
        for reg in 1..8u8 {
            // 8F /reg with a register operand; POP would move SP.
            let (mut cpu, mut bus) = rig(&[0x8f, 0xc0 | reg << 3]);
            assert!(matches!(cpu.step(&mut bus), Step::Invalid { .. }), "8F /{reg}");
            assert_eq!(cpu.regs[SP], 0x100);
            // C6 /reg to [BX], C7 /reg to AX.
            let (mut cpu, mut bus) = rig(&[0xc6, reg << 3 | 7, 0x55]);
            assert!(matches!(cpu.step(&mut bus), Step::Invalid { .. }), "C6 /{reg}");
            assert_eq!(bus.0[phys(0x3000, 0) as usize], 0);
            let (mut cpu, mut bus) = rig(&[0xc7, 0xc0 | reg << 3, 0x34, 0x12]);
            assert!(matches!(cpu.step(&mut bus), Step::Invalid { .. }), "C7 /{reg}");
            assert_eq!(cpu.regs[AX], 0);
        }
        // The /0 forms run.
        let (mut cpu, mut bus) = rig(&[0xc7, 0xc0, 0x34, 0x12, 0x8f, 0xc3]);
        bus.0[phys(0x2000, 0x100) as usize] = 0x78;
        assert!(matches!(cpu.step(&mut bus), Step::Executed { .. }));
        assert!(matches!(cpu.step(&mut bus), Step::Executed { .. }));
        assert_eq!((cpu.regs[AX], cpu.regs[BX], cpu.regs[SP]), (0x1234, 0x0078, 0x102));
    }
}
