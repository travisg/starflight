// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Helpers shared by the BIOS and DOS handlers that run behind emulator traps.
//!
//! A handler runs with the interrupt frame of the caller's `INT` on the stack
//! (IP, CS, FLAGS at SS:SP, +2, +4) and its ROM stub ends in IRET, so results
//! returned in flags are written into the stacked FLAGS word.
//!
//! The `rd`/`wr` functions access the 1 MiB array directly, for fixed places
//! such as the vector table and the BIOS data area. Addresses the program
//! chose (FCBs, the DTA, the caller's stack) go through `Mem`.

use crate::video::Video;
use cpu8086::{Cpu, SP, SS, phys};
use std::cell::OnceCell;
use std::ops::Range;

/// Display memory: the video adapter decodes it.
pub const DISPLAY: Range<u32> = 0xa0000..0xc0000;

/// Outcome of a high-level (Rust) interrupt handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hle {
    /// Handled; the stub's IRET returns to the caller.
    Done,
    /// Not ready (e.g. INT 16h AH=0 with no key): rerun the stub later.
    Retry,
    /// The program asked DOS to terminate.
    Exit(u8),
    /// A function this machine does not implement. The machine stops loudly.
    Unsupported(String),
}

pub fn rd8(ram: &[u8], addr: u32) -> u8 {
    ram[(addr & 0xfffff) as usize]
}

pub fn rd16(ram: &[u8], addr: u32) -> u16 {
    rd8(ram, addr) as u16 | (rd8(ram, addr + 1) as u16) << 8
}

pub fn wr8(ram: &mut [u8], addr: u32, v: u8) {
    ram[(addr & 0xfffff) as usize] = v;
}

pub fn wr16(ram: &mut [u8], addr: u32, v: u16) {
    wr8(ram, addr, v as u8);
    wr8(ram, addr + 1, (v >> 8) as u8);
}

/// Guest memory at addresses a program chose, as a trap handler reaches it:
/// RAM below A0000, display memory through the adapter, and the ROMs from
/// C0000 read-only. Handlers neither read display memory (the game never
/// has them do it) nor write the ROMs, which hold the trap stubs: such an
/// access does nothing (a read gives FFh) and the first one is kept for
/// `into_fault`, to stop the machine with.
pub struct Mem<'a> {
    ram: &'a mut [u8],
    video: &'a mut Video,
    fault: OnceCell<String>,
}

impl<'a> Mem<'a> {
    pub fn new(ram: &'a mut [u8], video: &'a mut Video) -> Self {
        Mem { ram, video, fault: OnceCell::new() }
    }

    pub fn rd8(&self, addr: u32) -> u8 {
        let a = addr & 0xfffff;
        if DISPLAY.contains(&a) {
            self.refuse(format!("read of display memory at {a:05X}"));
            return 0xff;
        }
        self.ram[a as usize]
    }

    pub fn rd16(&self, addr: u32) -> u16 {
        self.rd8(addr) as u16 | (self.rd8(addr + 1) as u16) << 8
    }

    pub fn wr8(&mut self, addr: u32, v: u8) {
        let a = addr & 0xfffff;
        if a < DISPLAY.start {
            self.ram[a as usize] = v;
        } else if a < DISPLAY.end {
            self.video.mem_write(a, v);
        } else {
            self.refuse(format!("write to the ROM area at {a:05X}"));
        }
    }

    pub fn wr16(&mut self, addr: u32, v: u16) {
        self.wr8(addr, v as u8);
        self.wr8(addr + 1, (v >> 8) as u8);
    }

    /// Copies `data` to physical memory, wrapping at 1 MiB.
    pub fn put(&mut self, addr: u32, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            self.wr8(addr + i as u32, b);
        }
    }

    /// An access was refused.
    pub fn refused(&self) -> bool {
        self.fault.get().is_some()
    }

    /// The first refused access, if any.
    pub fn into_fault(self) -> Option<String> {
        self.fault.into_inner()
    }

    fn refuse(&self, what: String) {
        let _ = self.fault.set(what);
    }
}

/// Physical address of the caller's stacked FLAGS word.
fn stacked_flags_addr(cpu: &Cpu) -> u32 {
    phys(cpu.sregs[SS], cpu.regs[SP].wrapping_add(4))
}

/// Sets or clears flag bits in the caller's stacked FLAGS (restored by IRET).
pub fn set_stacked_flag(cpu: &Cpu, mem: &mut Mem, mask: u16, on: bool) {
    let a = stacked_flags_addr(cpu);
    let f = mem.rd16(a);
    mem.wr16(a, if on { f | mask } else { f & !mask });
}

/// BIOS data area.
pub mod bda {
    pub const BASE: u32 = 0x400;
    pub const EQUIPMENT: u32 = 0x410;
    pub const MEMORY_KB: u32 = 0x413;
    pub const KBD_FLAGS: u32 = 0x417;
    pub const KBD_FLAGS2: u32 = 0x418;
    pub const KBD_HEAD: u32 = 0x41a;
    pub const KBD_TAIL: u32 = 0x41c;
    /// The 16-word keyboard ring buffer, as offsets from 0040:0000.
    pub const KBD_BUF_START: u16 = 0x1e;
    pub const KBD_BUF_END: u16 = 0x3e;
    pub const VIDEO_MODE: u32 = 0x449;
    pub const VIDEO_COLS: u32 = 0x44a;
    pub const VIDEO_PAGE_SIZE: u32 = 0x44c;
    pub const VIDEO_PAGE_START: u32 = 0x44e;
    pub const CURSOR_POS: u32 = 0x450;
    pub const CURSOR_TYPE: u32 = 0x460;
    pub const VIDEO_PAGE: u32 = 0x462;
    pub const CRTC_BASE: u32 = 0x463;
    pub const CGA_MODE_REG: u32 = 0x465;
    pub const CGA_PALETTE: u32 = 0x466;
    pub const TICKS: u32 = 0x46c;
    pub const MIDNIGHT: u32 = 0x470;
    pub const KBD_BUF_START_PTR: u32 = 0x480;
    pub const KBD_BUF_END_PTR: u32 = 0x482;
    pub const VIDEO_ROWS: u32 = 0x484;
    pub const CHAR_HEIGHT: u32 = 0x485;
    pub const EGA_MISC: u32 = 0x487;
    pub const EGA_SWITCHES: u32 = 0x488;
}
