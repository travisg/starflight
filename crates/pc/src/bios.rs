// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! The system BIOS: a ROM image whose entry points are emulator traps, the
//! interrupt vector table and the BIOS data area, plus the services that run
//! behind the traps (video lives in `video`).
//!
//! Entry points sit at the IBM PC/XT BIOS addresses because programs jump to
//! them directly: Starflight's keyboard handler XKEYINT chains to INT 9 with
//! `jmp F000:E987`. Each stub is `FE F8 nn` (trap nn) followed by IRET; the
//! timer stub then calls INT 1Ch and sends the EOI as the real one does.

use crate::hle::{Hle, bda, rd8, rd16, set_stacked_flag, wr8, wr16};
use crate::keyboard::{KeyEvent, translate};
use cpu8086::{AX, CX, Cpu, DX, flags, phys};

pub const ROM_SEG: u16 = 0xf000;
/// Generic stubs, one per vector: F000:1000 + 4 * vector.
const GENERIC_STUBS: u16 = 0x1000;
/// Vectors whose default handler does nothing.
const NOOP_VECTORS: &[u8] = &[0x05, 0x1b, 0x1c, 0x28, 0x2a, 0x2f, 0x33, 0x67];

const fn trap(n: u8) -> [u8; 4] {
    [0xfe, 0xf8, n, 0xcf]
}

/// (vector, offset in F000, stub bytes) at the XT BIOS addresses.
fn fixed_entries() -> Vec<(u8, u16, Vec<u8>)> {
    vec![
        // Timer: tick count, INT 1Ch, EOI.
        (0x08, 0xfea5, vec![0xfe, 0xf8, 0x08, 0xcd, 0x1c, 0x50, 0xb0, 0x20, 0xe6, 0x20, 0x58, 0xcf]),
        (0x09, 0xe987, trap(0x09).to_vec()),
        (0x10, 0xf065, trap(0x10).to_vec()),
        (0x11, 0xf84d, trap(0x11).to_vec()),
        (0x12, 0xf841, trap(0x12).to_vec()),
        (0x13, 0xec59, trap(0x13).to_vec()),
        (0x14, 0xe739, trap(0x14).to_vec()),
        (0x15, 0xf859, trap(0x15).to_vec()),
        // Keyboard services enable interrupts so a waiting read can be retried.
        (0x16, 0xe82e, vec![0xfb, 0xfe, 0xf8, 0x16, 0xcf]),
        (0x17, 0xefd2, trap(0x17).to_vec()),
        (0x19, 0xe6f2, trap(0x19).to_vec()),
        (0x1a, 0xfe6e, trap(0x1a).to_vec()),
        (0x05, 0xff54, trap(0x05).to_vec()),
    ]
}

/// Ticks per day of the 18.2 Hz BIOS clock.
const TICKS_PER_DAY: u32 = 0x1800b0;

/// Builds the ROMs, vector table and BIOS data area in `ram`. Video mode
/// setup is left to the caller.
pub fn install(ram: &mut [u8]) {
    let rom = phys(ROM_SEG, 0) as usize;
    ram[rom..rom + 0x10000].fill(0);
    for v in 0..=255u8 {
        let off = GENERIC_STUBS + 4 * v as u16;
        let a = phys(ROM_SEG, off) as usize;
        ram[a..a + 4].copy_from_slice(&trap(v));
        wr16(ram, v as u32 * 4, off);
        wr16(ram, v as u32 * 4 + 2, ROM_SEG);
    }
    for (v, off, code) in fixed_entries() {
        let a = phys(ROM_SEG, off) as usize;
        ram[a..a + code.len()].copy_from_slice(&code);
        wr16(ram, v as u32 * 4, off);
    }
    // Dummy IRET used by the user hooks 1Bh/1Ch, as on the XT.
    ram[phys(ROM_SEG, 0xff53) as usize] = 0xcf;
    for v in [0x1bu32, 0x1c] {
        wr16(ram, v * 4, 0xff53);
    }
    // Data-table vectors (video parameters, diskette parameters, graphics font).
    for v in [0x1du32, 0x1e, 0x1f, 0x43, 0x44] {
        wr16(ram, v * 4, 0);
        wr16(ram, v * 4 + 2, 0);
    }
    // Reset vector, BIOS date and the PC/XT model byte. F000:C000 stays 0,
    // so the game's Tandy check (looking for 21h there) fails.
    let reset = phys(ROM_SEG, 0xfff0) as usize;
    ram[reset..reset + 5].copy_from_slice(&[0xea, 0x5b, 0xe0, 0x00, 0xf0]);
    ram[reset + 5..reset + 13].copy_from_slice(b"01/10/86");
    ram[phys(ROM_SEG, 0xfffe) as usize] = 0xfe;
    // EGA BIOS signature at C000:0000 (32 KiB, far return as its init).
    ram[0xc0000..0xc0004].copy_from_slice(&[0x55, 0xaa, 0x40, 0xcb]);

    ram[0x400..0x500].fill(0);
    // Floppy present, 2 drives, planar RAM bits set, video bits 00 = EGA.
    wr16(ram, bda::EQUIPMENT, 0x004d);
    wr16(ram, bda::MEMORY_KB, 640);
    wr16(ram, bda::KBD_HEAD, bda::KBD_BUF_START);
    wr16(ram, bda::KBD_TAIL, bda::KBD_BUF_START);
    wr16(ram, bda::KBD_BUF_START_PTR, bda::KBD_BUF_START);
    wr16(ram, bda::KBD_BUF_END_PTR, bda::KBD_BUF_END);
    wr8(ram, bda::EGA_MISC, 0x60);
    wr8(ram, bda::EGA_SWITCHES, 0xf9);
    set_tick_count(ram, ticks_from_time_of_day());
}

fn set_tick_count(ram: &mut [u8], t: u32) {
    wr16(ram, bda::TICKS, t as u16);
    wr16(ram, bda::TICKS + 2, (t >> 16) as u16);
}

fn tick_count(ram: &[u8]) -> u32 {
    rd16(ram, bda::TICKS) as u32 | (rd16(ram, bda::TICKS + 2) as u32) << 16
}

/// The BIOS clock at power-on: the UTC time of day in 18.2 Hz ticks.
fn ticks_from_time_of_day() -> u32 {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    ((secs % 86_400 * TICKS_PER_DAY as u64) / 86_400) as u32
}

/// INT 08h (behind the trap): advance the tick count.
pub fn int08(ram: &mut [u8]) {
    let mut t = tick_count(ram) + 1;
    if t >= TICKS_PER_DAY {
        t = 0;
        wr8(ram, bda::MIDNIGHT, 1);
    }
    set_tick_count(ram, t);
}

/// Appends a key word to the BDA ring buffer; false if it is full.
pub fn push_key(ram: &mut [u8], key: u16) -> bool {
    let start = rd16(ram, bda::KBD_BUF_START_PTR);
    let end = rd16(ram, bda::KBD_BUF_END_PTR);
    let head = rd16(ram, bda::KBD_HEAD);
    let tail = rd16(ram, bda::KBD_TAIL);
    let mut next = tail + 2;
    if next >= end {
        next = start;
    }
    if next == head {
        return false;
    }
    wr16(ram, bda::BASE + tail as u32, key);
    wr16(ram, bda::KBD_TAIL, next);
    true
}

/// True when the BDA keyboard buffer is empty.
pub fn key_buffer_empty(ram: &[u8]) -> bool {
    rd16(ram, bda::KBD_HEAD) == rd16(ram, bda::KBD_TAIL)
}

/// INT 09h body: translate the latched scan code into the BDA buffer.
/// The caller acknowledges the keyboard and sends the EOI.
pub fn int09(ram: &mut [u8], code: u8) {
    let mut flags = rd8(ram, bda::KBD_FLAGS);
    let ev = translate(code, &mut flags);
    wr8(ram, bda::KBD_FLAGS, flags);
    if let KeyEvent::Key(k) = ev {
        push_key(ram, k);
    }
}

pub fn int16(cpu: &mut Cpu, ram: &mut [u8]) -> Hle {
    let ah = cpu.reg8(4);
    let head = rd16(ram, bda::KBD_HEAD);
    let tail = rd16(ram, bda::KBD_TAIL);
    match ah {
        0x00 | 0x10 => {
            if head == tail {
                return Hle::Retry;
            }
            cpu.regs[AX] = rd16(ram, bda::BASE + head as u32);
            let mut next = head + 2;
            if next >= rd16(ram, bda::KBD_BUF_END_PTR) {
                next = rd16(ram, bda::KBD_BUF_START_PTR);
            }
            wr16(ram, bda::KBD_HEAD, next);
        }
        0x01 | 0x11 => {
            let empty = head == tail;
            if !empty {
                cpu.regs[AX] = rd16(ram, bda::BASE + head as u32);
            }
            set_stacked_flag(cpu, ram, flags::ZF, empty);
        }
        0x02 | 0x12 => cpu.set_reg8(0, rd8(ram, bda::KBD_FLAGS)),
        _ => return Hle::Unsupported(format!("INT 16h AH={ah:02X}")),
    }
    Hle::Done
}

pub fn int1a(cpu: &mut Cpu, ram: &mut [u8]) -> Hle {
    match cpu.reg8(4) {
        0x00 => {
            let t = tick_count(ram);
            cpu.regs[CX] = (t >> 16) as u16;
            cpu.regs[DX] = t as u16;
            cpu.set_reg8(0, rd8(ram, bda::MIDNIGHT));
            wr8(ram, bda::MIDNIGHT, 0);
        }
        0x01 => set_tick_count(ram, (cpu.regs[CX] as u32) << 16 | cpu.regs[DX] as u32),
        ah => return Hle::Unsupported(format!("INT 1Ah AH={ah:02X} (no real-time clock on an XT)")),
    }
    Hle::Done
}

/// Services with simple fixed answers, and the default handlers of all other
/// vectors. `None` means the vector has a dedicated handler elsewhere.
pub fn simple(vector: u8, cpu: &mut Cpu, ram: &mut [u8]) -> Option<Hle> {
    let r = match vector {
        0x11 => {
            cpu.regs[AX] = rd16(ram, bda::EQUIPMENT);
            Hle::Done
        }
        0x12 => {
            cpu.regs[AX] = rd16(ram, bda::MEMORY_KB);
            Hle::Done
        }
        // No serial ports or printers: report time-outs.
        0x14 => {
            cpu.set_reg8(4, 0x80);
            Hle::Done
        }
        0x17 => {
            cpu.set_reg8(4, 0x01);
            Hle::Done
        }
        // XT: no extended services.
        0x15 => {
            cpu.set_reg8(4, 0x86);
            set_stacked_flag(cpu, ram, flags::CF, true);
            Hle::Done
        }
        v if NOOP_VECTORS.contains(&v) => Hle::Done,
        0x08 | 0x09 | 0x10 | 0x16 | 0x1a | 0x20 | 0x21 => return None,
        v => Hle::Unsupported(format!(
            "INT {v:02X}h has no handler (AX={:04X}); the program expected one to be installed",
            cpu.regs[AX]
        )),
    };
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_point_at_traps() {
        let mut ram = vec![0u8; 1 << 20];
        install(&mut ram);
        let ip = rd16(&ram, 9 * 4);
        let cs = rd16(&ram, 9 * 4 + 2);
        assert_eq!((cs, ip), (0xf000, 0xe987));
        let a = phys(cs, ip);
        assert_eq!(&ram[a as usize..a as usize + 4], &[0xfe, 0xf8, 0x09, 0xcf]);
        assert_eq!(ram[0xffffe], 0xfe);
    }

    #[test]
    fn key_buffer_ring() {
        let mut ram = vec![0u8; 1 << 20];
        install(&mut ram);
        assert!(key_buffer_empty(&ram));
        for i in 0..15 {
            assert!(push_key(&mut ram, i), "slot {i}");
        }
        assert!(!push_key(&mut ram, 99), "16-word ring holds 15 keys");
    }
}
