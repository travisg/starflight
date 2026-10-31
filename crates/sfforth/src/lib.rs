// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Starflight's Forth, seen from the outside.
//!
//! The game runs unmodified on the emulated 8086; this crate only reads its
//! memory: the dictionary in STARFLT.COM, the file directory shared by
//! STARA.COM/STARB.COM, and (later) the threaded code being executed.

pub mod consts;
pub mod dict;
pub mod directory;

/// Little-endian 16-bit read from a 64 KiB segment image, wrapping like the 8086.
pub fn rd16(seg: &[u8], addr: u16) -> u16 {
    u16::from_le_bytes([seg[addr as usize], seg[addr.wrapping_add(1) as usize]])
}

/// Builds the 64 KiB segment image DOS would create: the COM file at offset 0x100.
pub fn segment_image(com: &[u8]) -> Vec<u8> {
    let mut seg = vec![0u8; 0x10000];
    let n = com.len().min(0x10000 - 0x100);
    seg[0x100..0x100 + n].copy_from_slice(&com[..n]);
    seg
}
