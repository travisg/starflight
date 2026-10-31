// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Real-mode 8086 interpreter core.
//!
//! Executes the documented 8086 instruction set. 80186+ opcodes, x87 escapes
//! and undefined encodings return [`Step::Invalid`] rather than guessing, so the
//! machine can stop and report them.
//!
//! There is no decode cache: every instruction is fetched from the bus.
//! Starflight modifies its own code (its timer handler rewrites the immediate
//! at 0x193, and the INTERRUPT word writes its INT number at 0x16C9).
//!
//! A REP-prefixed string instruction runs one iteration per [`Cpu::step`] and
//! leaves IP on its first prefix until CX runs out, so interrupts are taken
//! between iterations as on real hardware ([`Cpu::rep_pending`] says so).
//!
//! `FE F8 nn` (FE /7 with a register operand, undefined on the 8086) is the
//! emulator trap: it returns [`Step::Trap`] with `nn` and IP past the three
//! bytes. The machine's BIOS ROM uses it to call into Rust.
//!
//! The optional `serde` feature derives `Serialize`/`Deserialize` for [`Cpu`]
//! (all of its state, private fields included) for machine snapshots.

mod alu;
mod exec;

/// Physical memory and I/O ports as seen by the CPU.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn write8(&mut self, addr: u32, val: u8);
    fn io_read8(&mut self, port: u16) -> u8;
    fn io_write8(&mut self, port: u16, val: u8);

    fn io_read16(&mut self, port: u16) -> u16 {
        let lo = self.io_read8(port) as u16;
        let hi = self.io_read8(port.wrapping_add(1)) as u16;
        lo | (hi << 8)
    }

    fn io_write16(&mut self, port: u16, val: u16) {
        self.io_write8(port, val as u8);
        self.io_write8(port.wrapping_add(1), (val >> 8) as u8);
    }
}

/// Register indices in instruction-encoding order.
pub const AX: usize = 0;
pub const CX: usize = 1;
pub const DX: usize = 2;
pub const BX: usize = 3;
pub const SP: usize = 4;
pub const BP: usize = 5;
pub const SI: usize = 6;
pub const DI: usize = 7;

/// Segment register indices in instruction-encoding order.
pub const ES: usize = 0;
pub const CS: usize = 1;
pub const SS: usize = 2;
pub const DS: usize = 3;

pub mod flags {
    pub const CF: u16 = 1 << 0;
    pub const PF: u16 = 1 << 2;
    pub const AF: u16 = 1 << 4;
    pub const ZF: u16 = 1 << 6;
    pub const SF: u16 = 1 << 7;
    pub const TF: u16 = 1 << 8;
    pub const IF: u16 = 1 << 9;
    pub const DF: u16 = 1 << 10;
    pub const OF: u16 = 1 << 11;
    /// Bits that can be changed; bits 12-15 and bit 1 always read as 1 on the 8086.
    pub const WRITABLE: u16 = 0x0fd5;
    pub const FIXED_ONES: u16 = 0xf002;
}

use flags::*;

/// Outcome of one [`Cpu::step`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// An instruction ran; `cycles` is its approximate 8088 cost.
    Executed { cycles: u32 },
    /// HLT ran, or the CPU was already halted; it waits for an interrupt.
    Halted,
    /// The emulator trap `FE F8 nn` ran; IP is past it.
    Trap { number: u8, cycles: u32 },
    /// An opcode this core does not execute. IP is left at the instruction.
    Invalid { cs: u16, ip: u16, bytes: [u8; 2] },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
enum Rep {
    #[default]
    None,
    /// F3: REP / REPE.
    Z,
    /// F2: REPNE.
    Nz,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cpu {
    pub regs: [u16; 8],
    pub sregs: [u16; 4],
    pub ip: u16,
    flags: u16,
    pub halted: bool,
    /// Interrupts are not recognised after this instruction (it was STI with
    /// IF clear, MOV SS or POP SS).
    pub irq_inhibit: bool,
    /// A REP string instruction has iterations left; IP points back at it.
    pub rep_pending: bool,
    /// CS:IP of the instruction most recently started, prefixes included.
    pub instr_cs: u16,
    pub instr_ip: u16,
    seg_override: Option<usize>,
    rep: Rep,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
pub fn phys(seg: u16, off: u16) -> u32 {
    (((seg as u32) << 4) + off as u32) & 0xfffff
}

impl Cpu {
    /// Power-on state: CS:IP = FFFF:0000.
    pub fn new() -> Self {
        let mut sregs = [0; 4];
        sregs[CS] = 0xffff;
        Cpu {
            regs: [0; 8],
            sregs,
            ip: 0,
            flags: FIXED_ONES,
            halted: false,
            irq_inhibit: false,
            rep_pending: false,
            instr_cs: 0xffff,
            instr_ip: 0,
            seg_override: None,
            rep: Rep::None,
        }
    }

    pub fn flags(&self) -> u16 {
        self.flags
    }

    pub fn set_flags(&mut self, f: u16) {
        self.flags = (f & WRITABLE) | FIXED_ONES;
    }

    #[inline]
    pub fn flag(&self, mask: u16) -> bool {
        self.flags & mask != 0
    }

    #[inline]
    pub fn set_flag(&mut self, mask: u16, on: bool) {
        if on {
            self.flags |= mask;
        } else {
            self.flags &= !mask;
        }
    }

    /// Interrupts may be delivered before the next instruction.
    pub fn irq_allowed(&self) -> bool {
        self.flag(IF) && !self.irq_inhibit
    }

    /// 8-bit register by encoding: AL CL DL BL AH CH DH BH.
    #[inline]
    pub fn reg8(&self, r: u8) -> u8 {
        let r = r as usize;
        if r < 4 { self.regs[r] as u8 } else { (self.regs[r - 4] >> 8) as u8 }
    }

    #[inline]
    pub fn set_reg8(&mut self, r: u8, v: u8) {
        let r = r as usize;
        if r < 4 {
            self.regs[r] = (self.regs[r] & 0xff00) | v as u16;
        } else {
            self.regs[r - 4] = (self.regs[r - 4] & 0x00ff) | ((v as u16) << 8);
        }
    }

    #[inline]
    pub fn read8<B: Bus + ?Sized>(&mut self, bus: &mut B, seg: u16, off: u16) -> u8 {
        bus.read8(phys(seg, off))
    }

    /// Word read; the high byte wraps within the segment at offset FFFF.
    #[inline]
    pub fn read16<B: Bus + ?Sized>(&mut self, bus: &mut B, seg: u16, off: u16) -> u16 {
        let lo = bus.read8(phys(seg, off)) as u16;
        let hi = bus.read8(phys(seg, off.wrapping_add(1))) as u16;
        lo | (hi << 8)
    }

    #[inline]
    pub fn write8<B: Bus + ?Sized>(&mut self, bus: &mut B, seg: u16, off: u16, v: u8) {
        bus.write8(phys(seg, off), v);
    }

    #[inline]
    pub fn write16<B: Bus + ?Sized>(&mut self, bus: &mut B, seg: u16, off: u16, v: u16) {
        bus.write8(phys(seg, off), v as u8);
        bus.write8(phys(seg, off.wrapping_add(1)), (v >> 8) as u8);
    }

    pub fn push<B: Bus + ?Sized>(&mut self, bus: &mut B, v: u16) {
        let sp = self.regs[SP].wrapping_sub(2);
        self.regs[SP] = sp;
        let ss = self.sregs[SS];
        self.write16(bus, ss, sp, v);
    }

    pub fn pop<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u16 {
        let sp = self.regs[SP];
        let ss = self.sregs[SS];
        let v = self.read16(bus, ss, sp);
        self.regs[SP] = sp.wrapping_add(2);
        v
    }

    /// Enters an interrupt or exception handler through the vector table.
    pub fn interrupt<B: Bus + ?Sized>(&mut self, bus: &mut B, vector: u8) {
        let f = self.flags;
        self.push(bus, f);
        let cs = self.sregs[CS];
        self.push(bus, cs);
        let ip = self.ip;
        self.push(bus, ip);
        self.set_flag(IF, false);
        self.set_flag(TF, false);
        let base = vector as u16 * 4;
        self.ip = self.read16(bus, 0, base);
        self.sregs[CS] = self.read16(bus, 0, base.wrapping_add(2));
        self.halted = false;
    }
}
