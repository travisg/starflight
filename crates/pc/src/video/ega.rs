// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! EGA display memory: four 64 KiB planes behind the graphics controller's
//! latches, read modes and write modes.
//!
//! Odd/even and chain addressing are not modelled here; the text and
//! CGA-compatible modes use a linear buffer instead (see `Video::cga_mem`).

use serde::{Deserialize, Serialize};

pub const PLANE_SIZE: usize = 0x10000;

/// Graphics controller register indices.
pub const GC_SET_RESET: usize = 0;
pub const GC_ENABLE_SET_RESET: usize = 1;
pub const GC_COLOR_COMPARE: usize = 2;
pub const GC_ROTATE: usize = 3;
pub const GC_READ_MAP: usize = 4;
pub const GC_MODE: usize = 5;
pub const GC_MISC: usize = 6;
pub const GC_DONT_CARE: usize = 7;
pub const GC_BIT_MASK: usize = 8;

#[derive(Serialize, Deserialize)]
pub struct Planes {
    /// Plane `p` byte `off` is at `p * PLANE_SIZE + off`.
    pub mem: Box<[u8]>,
    pub latch: [u8; 4],
}

impl Planes {
    pub fn new() -> Self {
        Planes { mem: vec![0; 4 * PLANE_SIZE].into_boxed_slice(), latch: [0; 4] }
    }

    #[inline]
    pub fn get(&self, plane: usize, off: usize) -> u8 {
        self.mem[plane * PLANE_SIZE + (off & 0xffff)]
    }

    #[inline]
    pub fn set(&mut self, plane: usize, off: usize, v: u8) {
        self.mem[plane * PLANE_SIZE + (off & 0xffff)] = v;
    }

    /// A CPU read: loads all four latches, then returns per the read mode.
    pub fn read(&mut self, gc: &[u8; 9], off: usize) -> u8 {
        for p in 0..4 {
            self.latch[p] = self.get(p, off);
        }
        if gc[GC_MODE] & 0x08 == 0 {
            return self.latch[(gc[GC_READ_MAP] & 3) as usize];
        }
        // Read mode 1: 1 where the pixel matches the colour compare value in
        // every plane selected by colour don't care.
        let mut r = 0xff;
        for p in 0..4 {
            if gc[GC_DONT_CARE] & (1 << p) != 0 {
                let want = if gc[GC_COLOR_COMPARE] & (1 << p) != 0 { 0xff } else { 0 };
                r &= !(self.latch[p] ^ want);
            }
        }
        r
    }

    /// A CPU write to the planes enabled in `map_mask`. Write mode 3 does not
    /// exist on the EGA; it is treated as write mode 0.
    pub fn write(&mut self, gc: &[u8; 9], map_mask: u8, off: usize, v: u8) {
        let mode = gc[GC_MODE] & 3;
        let rotated = v.rotate_right((gc[GC_ROTATE] & 7) as u32);
        let func = (gc[GC_ROTATE] >> 3) & 3;
        let mask = gc[GC_BIT_MASK];
        for p in 0..4 {
            if map_mask & (1 << p) == 0 {
                continue;
            }
            let latch = self.latch[p];
            let out = if mode == 1 {
                latch
            } else {
                let data = if mode == 2 {
                    if v & (1 << p) != 0 { 0xff } else { 0 }
                } else if gc[GC_ENABLE_SET_RESET] & (1 << p) != 0 {
                    if gc[GC_SET_RESET] & (1 << p) != 0 { 0xff } else { 0 }
                } else {
                    rotated
                };
                let data = match func {
                    0 => data,
                    1 => data & latch,
                    2 => data | latch,
                    _ => data ^ latch,
                };
                (data & mask) | (latch & !mask)
            };
            self.set(p, off, out);
        }
    }
}
