// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! DOS, as far as Starflight needs it: a COM loader and the INT 21h services,
//! mainly FCB file I/O, over one host directory acting as the current drive.
//!
//! Interface contract with the rest of the machine:
//! - The drive directory is a save slot holding working copies of the game
//!   files; the game reads and writes them there. It is never the original
//!   game directory (the caller guarantees that).
//! - `int21` reads registers from `cpu`, accesses memory through `mem`
//!   (`hle::Mem`: transfers into display memory reach the adapter; reading
//!   display memory or writing the ROMs is refused, and the caller stops the
//!   machine), writes results back to `cpu`, and reports carry-flag results
//!   through `hle::set_stacked_flag`.
//! - Unknown functions return `Hle::Unsupported` with AH/AL in the message,
//!   as does writing a file from display memory (AH=22h).
//!
//! Implemented: the functions the game calls (0D 0F 10 11 19 1A 21 22 29 4A;
//! s-macke starflight-reverse src/emul/call.c), plus 00/4C exit, 02/09
//! console output, 25/35 vectors and 30 version. FCB behaviour follows the
//! MS-DOS 4.0 kernel source (github.com/microsoft/MS-DOS, MIT), which kept
//! the DOS 3.3 FCB code.

mod clock;
mod fcb;
mod host;
#[cfg(test)]
mod tests;

use crate::hle::{Hle, Mem, rd8, set_stacked_flag, wr8, wr16};
use cpu8086::{AX, BP, BX, CS, CX, Cpu, DI, DS, DX, ES, SI, SP, SS, flags, phys};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One read or write of a game file, for tracing which data went where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileIo {
    /// "NAME.EXT", upper case.
    pub file: String,
    /// Byte offset in the file.
    pub offset: u64,
    /// Bytes of the transfer buffer covered; a read's zero padding of a
    /// partial record is included.
    pub len: u32,
    pub write: bool,
    /// Physical address of the transfer buffer.
    pub addr: u32,
}

// 8-bit register numbers for `Cpu::reg8`.
const AL: u8 = 0;
const DL: u8 = 2;
const AH: u8 = 4;

/// First paragraph above conventional memory.
const MEM_TOP: u16 = 0xa000;
/// The environment block sits this many paragraphs below the PSP.
const ENV_PARAS: u16 = 0x20;
/// Default drive C: (A: = 0).
const DEFAULT_DRIVE: u8 = 2;

/// Snapshots keep the console and DTA. The drive directory and logging
/// settings belong to the host side; open host files are not kept, and are
/// reopened by name on the next access.
#[derive(Serialize, Deserialize)]
pub struct Dos {
    #[serde(skip)]
    pub drive_dir: PathBuf,
    /// When set, file transfers are appended to `io_log`. The machine sets
    /// it while it keeps a file log.
    #[serde(skip)]
    pub log_io: bool,
    /// Emptied by the machine after every INT 21h.
    #[serde(skip)]
    pub io_log: Vec<FileIo>,
    /// Bytes written by functions 02h and 09h.
    pub console: Vec<u8>,
    #[serde(skip)]
    files: host::OpenFiles,
    /// Disk transfer address (segment, offset).
    dta: (u16, u16),
}

impl Dos {
    pub fn new(drive_dir: PathBuf) -> Self {
        Dos {
            drive_dir,
            log_io: false,
            io_log: Vec::new(),
            console: Vec::new(),
            files: host::OpenFiles::default(),
            dta: (0, 0x80),
        }
    }

    /// Builds a PSP (and environment) at `psp_seg`, copies `image` to
    /// psp:0100 and sets CS=DS=ES=SS=psp, IP=0100, SP=FFFE with a zero word
    /// pushed. `cmdline` becomes the command tail.
    pub fn load_com(&mut self, cpu: &mut Cpu, ram: &mut [u8], psp_seg: u16, image: &[u8], cmdline: &str) {
        assert!(0x100 + image.len() <= 0xfffe, "COM image of {} bytes does not fit below the stack", image.len());

        let env_seg = psp_seg.wrapping_sub(ENV_PARAS);
        let env = phys(env_seg, 0);
        for i in 0..ENV_PARAS as u32 * 16 {
            wr8(ram, env + i, 0);
        }
        let mut block = b"COMSPEC=C:\\COMMAND.COM\0PATH=C:\\\0\0".to_vec();
        block.extend_from_slice(&1u16.to_le_bytes());
        block.extend_from_slice(b"C:\\STARFLT.COM\0");
        put(ram, env, &block);

        let psp = phys(psp_seg, 0);
        for i in 0..0x100 {
            wr8(ram, psp + i, 0);
        }
        put(ram, psp, &[0xcd, 0x20]);
        wr16(ram, psp + 0x02, MEM_TOP);
        // INT 22h/23h/24h vectors as they were at load.
        for (i, vector) in [0x22u32, 0x23, 0x24].into_iter().enumerate() {
            for b in 0..4 {
                wr8(ram, psp + 0x0a + 4 * i as u32 + b, rd8(ram, vector * 4 + b));
            }
        }
        wr16(ram, psp + 0x16, psp_seg);
        // Job file table: stdin/out/err on CON (SFT 1), stdaux AUX (0), stdprn PRN (2).
        let mut jft = [0xffu8; 20];
        jft[..5].copy_from_slice(&[1, 1, 1, 0, 2]);
        put(ram, psp + 0x18, &jft);
        wr16(ram, psp + 0x2c, env_seg);
        wr16(ram, psp + 0x32, jft.len() as u16);
        wr16(ram, psp + 0x34, 0x18);
        wr16(ram, psp + 0x36, psp_seg);
        put(ram, psp + 0x50, &[0xcd, 0x21, 0xcb]);

        // Command tail; COMMAND.COM's tails start with the separator.
        let mut tail: Vec<u8> = cmdline.bytes().collect();
        if tail.first().is_some_and(|&c| c != b' ' && c != b'\t') {
            tail.insert(0, b' ');
        }
        tail.truncate(126);
        wr8(ram, psp + 0x80, tail.len() as u8);
        put(ram, psp + 0x81, &tail);
        wr8(ram, psp + 0x81 + tail.len() as u32, 0x0d);

        // Default FCBs from the first two arguments, as COMMAND.COM fills them
        // (29h with AL=1). AL/AH report a bad drive letter in either.
        let mut drive_bad = [false; 2];
        let mut si = 0x81u16;
        for (n, at) in [0x5cu16, 0x6c].into_iter().enumerate() {
            let mut f = [0u8; 16];
            let (r, end) = fcb::parse_name(|o| rd8(ram, phys(psp_seg, o)), si, &mut f, 1);
            put(ram, psp + at as u32, &f);
            drive_bad[n] = r == 0xff;
            si = end;
            loop {
                let c = rd8(ram, phys(psp_seg, si));
                if c == 0x0d || c == b'/' || b" \t,;=".contains(&c) || si >= 0xff {
                    break;
                }
                si += 1;
            }
        }

        put(ram, phys(psp_seg, 0x100), image);
        wr16(ram, phys(psp_seg, 0xfffe), 0);

        cpu.regs[AX] = if drive_bad[1] { 0xff00 } else { 0 } | if drive_bad[0] { 0x00ff } else { 0 };
        cpu.regs[BX] = 0;
        cpu.regs[CX] = 0x00ff;
        cpu.regs[DX] = psp_seg;
        cpu.regs[SI] = 0x0100;
        cpu.regs[DI] = 0xfffe;
        cpu.regs[BP] = 0x091c;
        cpu.regs[SP] = 0xfffe;
        for s in [ES, CS, SS, DS] {
            cpu.sregs[s] = psp_seg;
        }
        cpu.ip = 0x0100;
        cpu.set_flags(flags::IF);

        self.dta = (psp_seg, 0x80);
    }

    pub fn int21(&mut self, cpu: &mut Cpu, mem: &mut Mem) -> Hle {
        let (ah, al) = (cpu.reg8(AH), cpu.reg8(AL));
        let fcb_result = match ah {
            0x00 => return Hle::Exit(0),
            0x02 => {
                let c = cpu.reg8(DL);
                self.console.push(c);
                // AL is the last character shown; a tab is shown as blanks.
                cpu.set_reg8(AL, if c == b'\t' { b' ' } else { c });
                return Hle::Done;
            }
            0x09 => {
                let (seg, off) = (cpu.sregs[DS], cpu.regs[DX]);
                for i in 0..=0xffffu16 {
                    let c = mem.rd8(phys(seg, off.wrapping_add(i)));
                    if c == b'$' || mem.refused() {
                        break;
                    }
                    self.console.push(c);
                }
                cpu.set_reg8(AL, b'$');
                return Hle::Done;
            }
            // Disk reset: writes already reached the host files.
            0x0d => return Hle::Done,
            0x0f => self.fcb_open(cpu, mem),
            0x10 => self.fcb_close(cpu, mem),
            0x11 => self.fcb_find_first(cpu, mem),
            0x19 => Ok(DEFAULT_DRIVE),
            0x1a => {
                self.dta = (cpu.sregs[DS], cpu.regs[DX]);
                return Hle::Done;
            }
            0x21 => self.fcb_random(cpu, mem, false),
            0x22 => self.fcb_random(cpu, mem, true),
            0x25 => {
                let v = al as u32 * 4;
                mem.wr16(v, cpu.regs[DX]);
                mem.wr16(v + 2, cpu.sregs[DS]);
                return Hle::Done;
            }
            0x29 => {
                let (es, di) = (cpu.sregs[ES], cpu.regs[DI]);
                let mut f = [0u8; 16];
                for (i, b) in f.iter_mut().enumerate() {
                    *b = mem.rd8(phys(es, di.wrapping_add(i as u16)));
                }
                let ds = cpu.sregs[DS];
                let (r, si) = fcb::parse_name(|o| mem.rd8(phys(ds, o)), cpu.regs[SI], &mut f, al);
                for (i, &b) in f.iter().enumerate() {
                    mem.wr8(phys(es, di.wrapping_add(i as u16)), b);
                }
                cpu.regs[SI] = si;
                Ok(r)
            }
            0x30 => {
                // 3.30: AL major, AH minor; OEM and serial number 0.
                cpu.regs[AX] = 0x1e03;
                cpu.regs[BX] = 0;
                cpu.regs[CX] = 0;
                return Hle::Done;
            }
            0x35 => {
                let v = al as u32 * 4;
                cpu.regs[BX] = mem.rd16(v);
                cpu.sregs[ES] = mem.rd16(v + 2);
                return Hle::Done;
            }
            0x4a => return resize(cpu, mem),
            0x4c => return Hle::Exit(al),
            _ => return Hle::Unsupported(format!("INT 21h AH={ah:02X} AL={al:02X}")),
        };
        match fcb_result {
            Ok(r) => {
                cpu.set_reg8(AL, r);
                Hle::Done
            }
            Err(h) => h,
        }
    }
}

/// 4Ah. The program owns everything up to A000, so a block at ES may grow to
/// there; AX=ES on success, as $SETBLOCK returns.
fn resize(cpu: &mut Cpu, mem: &mut Mem) -> Hle {
    let (es, bx) = (cpu.sregs[ES], cpu.regs[BX]);
    let max = MEM_TOP.saturating_sub(es);
    if bx <= max {
        cpu.regs[AX] = es;
        set_stacked_flag(cpu, mem, flags::CF, false);
    } else {
        cpu.regs[AX] = 8; // insufficient memory
        cpu.regs[BX] = max;
        set_stacked_flag(cpu, mem, flags::CF, true);
    }
    Hle::Done
}

/// Copies `data` to physical memory, wrapping at 1 MiB.
fn put(ram: &mut [u8], addr: u32, data: &[u8]) {
    for (i, &b) in data.iter().enumerate() {
        wr8(ram, addr + i as u32, b);
    }
}
