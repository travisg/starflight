// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! The video adapter: an IBM EGA with 256 KiB, in its text, CGA-compatible
//! and EGA graphics modes. (A CGA personality comes later.)
//!
//! Interface contract with the rest of the machine:
//! - `mem_read`/`mem_write` receive physical addresses in A0000-BFFFF and
//!   decode them by the graphics controller's memory map (misc register bits
//!   2-3), like the card: in mode 0Dh only A000-AFFF is live; in text and CGA
//!   modes only B800-BFFF is.
//! - `port_read`/`port_write` return `None`/`false` for ports that are not the
//!   adapter's, so the bus can report them. `now` is in master-clock ticks.
//! - `int10` implements the EGA BIOS video services. It reads registers from
//!   `cpu`, may read or write `ram` (the 1 MiB address space: BDA at 0x400,
//!   caller buffers such as ES:DX palette tables), and writes results back to
//!   `cpu`. Unknown functions return `Hle::Unsupported`. Like the real BIOS
//!   it takes the mode, page and cursor positions from the BDA, so POST must
//!   call `set_mode` before the first INT 10h.
//! - The machine calls `vretrace` at the start of every vertical retrace; the
//!   CRTC start address is latched for display there.
//! - `render` draws what the monitor would show into a `Frame`.
//!
//! The adapter owns ports 3C0-3CF and 3D0-3DF (colour addressing only; the
//! misc output I/O select bit is ignored). Registers it does not model, such
//! as the CGA-only 3D8/3D9, accept writes and read FFh.
//!
//! Display memory: memory maps A0000 and B0000 reach the four planes through
//! the graphics controller (`ega`); the B8000 map is a plain linear 32 KiB
//! buffer, which is how the text and CGA-compatible modes see it.

mod ega;
mod font;
mod int10;
mod modes;
mod render;
#[cfg(test)]
mod tests;

use crate::hle::{Hle, bda, wr8, wr16};
use cpu8086::Cpu;
use ega::{GC_MISC, Planes};
use modes::{Kind, ModeParams};

pub use font::CP437;

/// One scanline lasts 912 master ticks; a frame is 262 lines (59.92 Hz).
pub const MASTER_PER_LINE: u64 = 912;
pub const LINES_PER_FRAME: u64 = 262;
pub const MASTER_PER_FRAME: u64 = MASTER_PER_LINE * LINES_PER_FRAME;
/// Vertical retrace occupies lines 224-226 of each frame.
pub const VRETRACE_LINE: u64 = 224;
pub const VRETRACE_LINES: u64 = 3;
/// Displayed lines and dots of each frame.
const ACTIVE_LINES: u64 = 200;
const ACTIVE_DOTS: u64 = 640;

/// EGA configuration switches as BIOS reports them (BDA 0x488, INT 10h
/// AH=12h CL): Enhanced Color Display.
const SWITCHES: u8 = 0x09;

/// Attribute controller registers.
const ATTR_MODE: usize = 0x10;
const ATTR_OVERSCAN: usize = 0x11;
const ATTR_PLANE_ENABLE: usize = 0x12;
const ATTR_PAN: usize = 0x13;
/// Attribute index bit 5: the palette drives the display (screen on).
const ATTR_PAS: u8 = 0x20;

/// CRTC registers.
const CRTC_CURSOR_START: usize = 0x0a;
const CRTC_CURSOR_END: usize = 0x0b;
const CRTC_START_HI: usize = 0x0c;
const CRTC_START_LO: usize = 0x0d;
const CRTC_CURSOR_HI: usize = 0x0e;
const CRTC_CURSOR_LO: usize = 0x0f;
const CRTC_OFFSET: usize = 0x13;
const CRTC_MODE: usize = 0x17;

const SEQ_MAP_MASK: usize = 2;

const CGA_MEM_SIZE: usize = 0x8000;

/// 3C0-3CF and 3D0-3DF.
const PORTS: std::ops::RangeInclusive<u16> = 0x3c0..=0x3df;

/// A rendered picture. Pixels are 0x00RRGGBB, row-major.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

/// Where a CPU address lands in display memory.
enum Target {
    Planes(usize),
    Cga(usize),
}

pub struct Video {
    planes: Planes,
    /// The B8000 window: text and CGA-compatible modes.
    cga_mem: Box<[u8]>,
    misc_out: u8,
    seq_index: u8,
    seq: [u8; 5],
    gc_index: u8,
    gc: [u8; 9],
    /// Bits 0-4 select a register; bit 5 is the palette address source.
    attr_index: u8,
    /// The 3C0 flip-flop: the next write is data, not an index.
    attr_flip: bool,
    attr: [u8; 0x14],
    crtc_index: u8,
    crtc: [u8; 0x19],
    /// CRTC start address as latched at the last vertical retrace.
    display_start: Option<u16>,
}

impl Default for Video {
    fn default() -> Self {
        Self::new()
    }
}

impl Video {
    /// An adapter with mode 3's registers and a blank text screen. The BDA is
    /// not touched until `set_mode`.
    pub fn new() -> Self {
        let mut v = Video {
            planes: Planes::new(),
            cga_mem: vec![0; CGA_MEM_SIZE].into_boxed_slice(),
            misc_out: 0,
            seq_index: 0,
            seq: [0; 5],
            gc_index: 0,
            gc: [0; 9],
            attr_index: 0,
            attr_flip: false,
            attr: [0; 0x14],
            crtc_index: 0,
            crtc: [0; 0x19],
            display_start: None,
        };
        let p = modes::params(3).unwrap();
        v.load_regs(p);
        v.clear(p);
        v
    }

    /// Whether `set_mode` and INT 10h AH=00h support BIOS mode `mode`.
    pub fn supports_mode(mode: u8) -> bool {
        modes::params(mode & 0x7f).is_some()
    }

    fn decode(&self, addr: u32) -> Option<Target> {
        let map = (self.gc[GC_MISC] >> 2) & 3;
        let (base, size) = match map {
            0 => (0xa0000, 0x20000),
            1 => (0xa0000, 0x10000),
            2 => (0xb0000, 0x8000),
            _ => (0xb8000, 0x8000),
        };
        let off = addr.wrapping_sub(base) as usize;
        if off >= size {
            None
        } else if map == 3 {
            Some(Target::Cga(off))
        } else {
            Some(Target::Planes(off & 0xffff))
        }
    }

    pub fn mem_read(&mut self, addr: u32) -> u8 {
        match self.decode(addr) {
            Some(Target::Planes(off)) => self.planes.read(&self.gc, off),
            Some(Target::Cga(off)) => self.cga_mem[off],
            None => 0xff,
        }
    }

    pub fn mem_write(&mut self, addr: u32, v: u8) {
        match self.decode(addr) {
            Some(Target::Planes(off)) => {
                let mask = self.seq[SEQ_MAP_MASK] & 0x0f;
                self.planes.write(&self.gc, mask, off, v);
            }
            Some(Target::Cga(off)) => self.cga_mem[off] = v,
            None => {}
        }
    }

    pub fn port_read(&mut self, port: u16, now: u64) -> Option<u8> {
        if !PORTS.contains(&port) {
            return None;
        }
        let v = match port {
            // Input status 0: bit 4 senses the configuration switch chosen
            // by the misc output clock select. 1001 reads the same in either
            // switch order.
            0x3c2 => {
                let sel = (self.misc_out >> 2) & 3;
                ((SWITCHES >> (3 - sel)) & 1) << 4
            }
            0x3d5 => match self.crtc_index {
                0x0c..=0x0f => self.crtc[self.crtc_index as usize],
                _ => 0xff,
            },
            0x3da => {
                self.attr_flip = false;
                input_status1(now)
            }
            _ => 0xff,
        };
        Some(v)
    }

    pub fn port_write(&mut self, port: u16, v: u8) -> bool {
        if !PORTS.contains(&port) {
            return false;
        }
        match port {
            0x3c0 => {
                if self.attr_flip {
                    self.write_attr((self.attr_index & 0x1f) as usize, v);
                } else {
                    self.attr_index = v & 0x3f;
                }
                self.attr_flip = !self.attr_flip;
            }
            0x3c2 => self.misc_out = v,
            0x3c4 => self.seq_index = v & 0x07,
            0x3c5 => {
                if let Some(r) = self.seq.get_mut(self.seq_index as usize) {
                    *r = v;
                }
            }
            0x3ce => self.gc_index = v & 0x0f,
            0x3cf => {
                if let Some(r) = self.gc.get_mut(self.gc_index as usize) {
                    *r = v;
                }
            }
            0x3d4 => self.crtc_index = v & 0x1f,
            0x3d5 => {
                if let Some(r) = self.crtc.get_mut(self.crtc_index as usize) {
                    *r = v;
                }
            }
            _ => {}
        }
        true
    }

    /// Palette and overscan registers hold 6-bit colours.
    fn write_attr(&mut self, i: usize, v: u8) {
        if i < self.attr.len() {
            self.attr[i] = if i < 0x10 || i == ATTR_OVERSCAN { v & 0x3f } else { v };
        }
    }

    fn load_regs(&mut self, p: &ModeParams) {
        self.misc_out = p.misc;
        self.seq = [0x03, p.seq[0], p.seq[1], p.seq[2], p.seq[3]];
        self.crtc = p.crtc;
        self.attr = p.attr;
        self.attr_index = ATTR_PAS;
        self.attr_flip = false;
        self.gc = p.gc;
    }

    fn clear(&mut self, p: &ModeParams) {
        match p.kind {
            Kind::Text => {
                for cell in self.cga_mem.as_chunks_mut::<2>().0 {
                    *cell = [b' ', 0x07];
                }
            }
            Kind::Cga2 | Kind::Cga1 => self.cga_mem.fill(0),
            Kind::Planar => self.planes.mem.fill(0),
        }
    }

    /// Sets a BIOS video mode and its BDA fields, as INT 10h AH=00h does.
    /// Bit 7 of `mode` keeps display memory. Panics on a mode this adapter
    /// does not support (see `supports_mode`).
    pub fn set_mode(&mut self, mode: u8, ram: &mut [u8]) {
        let keep = mode & 0x80 != 0;
        let mode = mode & 0x7f;
        let p = modes::params(mode).unwrap_or_else(|| panic!("video mode {mode:02X}h is not supported"));
        self.load_regs(p);
        if !keep {
            self.clear(p);
        }
        wr8(ram, bda::VIDEO_MODE, mode);
        wr16(ram, bda::VIDEO_COLS, p.cols as u16);
        wr16(ram, bda::VIDEO_PAGE_SIZE, p.page_size);
        wr16(ram, bda::VIDEO_PAGE_START, 0);
        for page in 0..8 {
            wr16(ram, bda::CURSOR_POS + 2 * page, 0);
        }
        let cursor = (p.crtc[CRTC_CURSOR_START] as u16) << 8 | p.crtc[CRTC_CURSOR_END] as u16;
        wr16(ram, bda::CURSOR_TYPE, cursor);
        wr8(ram, bda::VIDEO_PAGE, 0);
        wr16(ram, bda::CRTC_BASE, 0x3d4);
        wr8(ram, bda::CGA_MODE_REG, p.cga_mode);
        wr8(ram, bda::CGA_PALETTE, p.cga_palette);
        wr8(ram, bda::VIDEO_ROWS, 24);
        wr16(ram, bda::CHAR_HEIGHT, 8);
        // 256 KiB, EGA active, colour; bit 7 = memory kept by this mode set.
        wr8(ram, bda::EGA_MISC, 0x60 | if keep { 0x80 } else { 0 });
        wr8(ram, bda::EGA_SWITCHES, SWITCHES);
    }

    pub fn int10(&mut self, cpu: &mut Cpu, ram: &mut [u8]) -> Hle {
        int10::int10(self, cpu, ram)
    }

    pub fn vretrace(&mut self) {
        self.display_start = Some(self.crtc_start());
    }

    fn crtc_start(&self) -> u16 {
        (self.crtc[CRTC_START_HI] as u16) << 8 | self.crtc[CRTC_START_LO] as u16
    }

    /// The start address being displayed: the live value until the first
    /// vertical retrace latches one.
    fn shown_start(&self) -> u16 {
        self.display_start.unwrap_or_else(|| self.crtc_start())
    }

    pub fn render(&self, frame: &mut Frame) {
        render::render(self, frame);
    }

    /// The screen as text in text modes (CP437 mapped to Unicode), else `None`.
    pub fn text_lines(&self) -> Option<Vec<String>> {
        render::text_lines(self)
    }
}

/// Input status 1: bit 0 = blanking (not in active display), bit 3 =
/// vertical retrace.
fn input_status1(now: u64) -> u8 {
    let t = now % MASTER_PER_FRAME;
    let line = t / MASTER_PER_LINE;
    let dot = t % MASTER_PER_LINE;
    let mut v = 0;
    if !(line < ACTIVE_LINES && dot < ACTIVE_DOTS) {
        v |= 0x01;
    }
    if (VRETRACE_LINE..VRETRACE_LINE + VRETRACE_LINES).contains(&line) {
        v |= 0x08;
    }
    v
}
