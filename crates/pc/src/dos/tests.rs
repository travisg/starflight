// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

use super::clock::{self, LocalTime};
use super::fcb::{self, CUR_BLOCK, CUR_REC, DATE, DRIVE, DRIVE_C, FEOF, FILE_SIZE, FTRIM, RAND_REC, REC_SIZE, TIME};
use super::*;
use crate::hle::rd16;
use crate::video::Video;
use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STACK_SEG: u16 = 0x0600;
const STACK_SP: u16 = 0x0100;
const FCB_SEG: u16 = 0x1000;
const FCB_OFF: u16 = 0x0100;
const DTA_SEG: u16 = 0x2000;

/// A DOS over its own empty directory, a 1 MiB RAM and a CPU whose SS:SP
/// holds an interrupt frame (IP, CS, FLAGS) with CF set.
struct Rig {
    dos: Dos,
    cpu: Cpu,
    ram: Vec<u8>,
    video: Video,
    dir: PathBuf,
}

impl Rig {
    fn new(test: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("pc-dos-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut ram = vec![0u8; 1 << 20];
        let mut cpu = Cpu::new();
        cpu.sregs[SS] = STACK_SEG;
        cpu.regs[SP] = STACK_SP;
        wr16(&mut ram, phys(STACK_SEG, STACK_SP), 0x1234);
        wr16(&mut ram, phys(STACK_SEG, STACK_SP + 2), 0x5678);
        wr16(&mut ram, phys(STACK_SEG, STACK_SP + 4), 0xf203);
        cpu.sregs[DS] = FCB_SEG;
        cpu.regs[DX] = FCB_OFF;
        let mut dos = Dos::new(dir.clone());
        dos.log_io = true;
        dos.dta = (DTA_SEG, 0);
        Rig { dos, cpu, ram, video: Video::new(), dir }
    }

    fn file(&self, name: &str, data: &[u8]) {
        fs::write(self.dir.join(name), data).unwrap();
    }

    /// Calls INT 21h; a refused memory access gives `Hle::Unsupported`.
    fn call(&mut self, ax: u16) -> Hle {
        self.cpu.regs[AX] = ax;
        let mut mem = Mem::new(&mut self.ram, &mut self.video);
        let r = self.dos.int21(&mut self.cpu, &mut mem);
        mem.into_fault().map_or(r, Hle::Unsupported)
    }

    /// Calls a function that answers in AL and returns AL.
    fn al(&mut self, ax: u16) -> u8 {
        assert_eq!(self.call(ax), Hle::Done);
        self.cpu.reg8(AL)
    }

    fn stacked_cf(&self) -> bool {
        rd16(&self.ram, phys(STACK_SEG, STACK_SP + 4)) & flags::CF != 0
    }

    fn put(&mut self, seg: u16, off: u16, data: &[u8]) {
        put(&mut self.ram, phys(seg, off), data);
    }

    fn get(&self, seg: u16, off: u16, len: usize) -> Vec<u8> {
        (0..len).map(|i| rd8(&self.ram, phys(seg, off + i as u16))).collect()
    }

    /// Fills the FCB at FCB_SEG:FCB_OFF from `spec` with 29h.
    fn parse_fcb(&mut self, spec: &str) -> u8 {
        let mut s = spec.as_bytes().to_vec();
        s.push(0x0d);
        self.put(0x3000, 0, &s);
        let ds = self.cpu.sregs[DS];
        self.cpu.sregs[DS] = 0x3000;
        self.cpu.regs[SI] = 0;
        self.cpu.sregs[ES] = FCB_SEG;
        self.cpu.regs[DI] = FCB_OFF;
        let r = self.al(0x2900);
        self.cpu.sregs[DS] = ds;
        r
    }

    fn f8(&self, field: u16) -> u8 {
        rd8(&self.ram, phys(FCB_SEG, FCB_OFF + field))
    }

    fn f16(&self, field: u16) -> u16 {
        rd16(&self.ram, phys(FCB_SEG, FCB_OFF + field))
    }

    fn f32(&self, field: u16) -> u32 {
        self.f16(field) as u32 | (self.f16(field + 2) as u32) << 16
    }

    fn set16(&mut self, field: u16, v: u16) {
        wr16(&mut self.ram, phys(FCB_SEG, FCB_OFF + field), v);
    }

    fn set32(&mut self, field: u16, v: u32) {
        self.set16(field, v as u16);
        self.set16(field + 2, (v >> 16) as u16);
    }

    /// Opens `spec` with record size `rec_size`.
    fn open(&mut self, spec: &str, rec_size: u16) {
        self.parse_fcb(spec);
        assert_eq!(self.al(0x0f00), 0, "open {spec}");
        self.set16(REC_SIZE, rec_size);
    }

    /// Random read (21h) of record `rec`; returns AL.
    fn read(&mut self, rec: u32) -> u8 {
        self.set32(RAND_REC, rec);
        self.al(0x2100)
    }

    fn write(&mut self, rec: u32, data: &[u8]) -> u8 {
        self.put(DTA_SEG, 0, data);
        self.set32(RAND_REC, rec);
        self.al(0x2200)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
}

/// Runs 29h on `s` (CR-terminated) over the 16 bytes `fcb`; returns AL and
/// the offset parsing stopped at.
fn parse(s: &str, fcb: &mut [u8; 16], flags: u8) -> (u8, u16) {
    let mut b = s.as_bytes().to_vec();
    b.push(0x0d);
    fcb::parse_name(|o| b.get(o as usize).copied().unwrap_or(0x0d), 0, fcb, flags)
}

fn parsed(s: &str, flags: u8) -> (u8, u16, u8, String) {
    let mut f = [0xaau8; 16];
    let (al, si) = parse(s, &mut f, flags);
    assert_eq!(&f[12..], &[0; 4], "{s}: current block and record size zeroed");
    (al, si, f[0], String::from_utf8_lossy(&f[1..12]).into_owned())
}

#[test]
fn dos_load_com_layout() {
    let mut r = Rig::new("load");
    for (v, seg) in [(0x22u32, 0x1111u16), (0x23, 0x2222), (0x24, 0x3333)] {
        wr16(&mut r.ram, v * 4, 0x0100 + v as u16);
        wr16(&mut r.ram, v * 4 + 2, seg);
    }
    let image: Vec<u8> = (0..300).map(|i| i as u8).collect();
    let psp = 0x0800;
    r.dos.load_com(&mut r.cpu, &mut r.ram, psp, &image, "stara.com b:foo.*");

    let p = |r: &Rig, off: u16, len: usize| r.get(psp, off, len);
    assert_eq!(p(&r, 0, 2), [0xcd, 0x20]);
    assert_eq!(rd16(&r.ram, phys(psp, 2)), 0xa000);
    assert_eq!(p(&r, 0x0a, 12), [0x22, 1, 0x11, 0x11, 0x23, 1, 0x22, 0x22, 0x24, 1, 0x33, 0x33]);
    assert_eq!(rd16(&r.ram, phys(psp, 0x16)), psp);
    let mut jft = vec![1, 1, 1, 0, 2];
    jft.resize(20, 0xff);
    assert_eq!(p(&r, 0x18, 20), jft);
    assert_eq!(rd16(&r.ram, phys(psp, 0x2c)), psp - 0x20);
    assert_eq!(rd16(&r.ram, phys(psp, 0x32)), 20);
    assert_eq!(p(&r, 0x34, 4), [0x18, 0, 0x00, 0x08]);
    assert_eq!(p(&r, 0x50, 3), [0xcd, 0x21, 0xcb]);
    assert_eq!(p(&r, 0x5c, 12), b"\0STARA   COM");
    assert_eq!(p(&r, 0x6c, 12), b"\x02FOO     ???");
    let tail = b" stara.com b:foo.*\r";
    assert_eq!(r.ram[phys(psp, 0x80) as usize], tail.len() as u8 - 1);
    assert_eq!(p(&r, 0x81, tail.len()), tail);
    assert_eq!(p(&r, 0x100, image.len()), image);

    let mut env = b"COMSPEC=C:\\COMMAND.COM\0PATH=C:\\\0\0\x01\0C:\\STARFLT.COM\0".to_vec();
    env.push(0);
    assert_eq!(r.get(psp - 0x20, 0, env.len()), env);

    let c = &r.cpu;
    assert_eq!([c.regs[AX], c.regs[BX], c.regs[CX], c.regs[DX]], [0, 0, 0x00ff, psp]);
    assert_eq!([c.regs[SI], c.regs[DI], c.regs[BP], c.regs[SP]], [0x0100, 0xfffe, 0x091c, 0xfffe]);
    assert_eq!(c.sregs, [psp; 4]);
    assert_eq!(c.ip, 0x0100);
    assert!(c.flag(flags::IF));
    assert_eq!(rd16(&r.ram, phys(psp, 0xfffe)), 0);
    assert_eq!(r.dos.dta, (psp, 0x80));
}

#[test]
fn dos_load_com_empty_tail_and_bad_drive() {
    let mut r = Rig::new("load-empty");
    r.dos.load_com(&mut r.cpu, &mut r.ram, 0x0800, &[0x90], "");
    assert_eq!(r.get(0x0800, 0x5c, 12), b"\0           ");
    assert_eq!(r.get(0x0800, 0x6c, 12), b"\0           ");
    assert_eq!(r.get(0x0800, 0x80, 2), [0, 0x0d]);
    assert_eq!(r.cpu.regs[AX], 0);

    r.dos.load_com(&mut r.cpu, &mut r.ram, 0x0800, &[0x90], "a:x q:y");
    assert_eq!(r.cpu.regs[AX], 0xff00);
}

#[test]
fn dos_parse_names() {
    assert_eq!(parsed("  stara.com", 0), (0, 11, 0, "STARA   COM".into()));
    assert_eq!(parsed("\tstarb.com rest", 0), (0, 10, 0, "STARB   COM".into()));
    // One separator is skipped only with bit 0.
    assert_eq!(parsed(" ; foo", 1), (0, 6, 0, "FOO        ".into()));
    assert_eq!(parsed(" ; foo", 0), (0, 1, 0, "           ".into()));
    assert_eq!(parsed(";;foo", 1), (0, 1, 0, "           ".into()));
    // Drive letters: A: to C: are valid; others still land in the drive byte.
    assert_eq!(parsed("c:x", 0), (0, 3, 3, "X          ".into()));
    assert_eq!(parsed("A:", 0), (0, 2, 1, "           ".into()));
    assert_eq!(parsed("d:x.y", 0), (0xff, 5, 4, "X       Y  ".into()));
    assert_eq!(parsed("1:x", 0), (0xff, 3, 0xf1, "X          ".into()));
    // Wildcards.
    assert_eq!(parsed("*.com", 0), (1, 5, 0, "????????COM".into()));
    assert_eq!(parsed("fo?.c*", 0), (1, 6, 0, "FO?     C??".into()));
    assert_eq!(parsed("ab*cd.*x", 0), (1, 8, 0, "AB?????????".into()));
    assert_eq!(parsed("d:*.*", 0), (0xff, 5, 4, "???????????".into()));
    // Over-long words are consumed; terminators stop the parse.
    assert_eq!(parsed("abcdefghij.text", 0), (0, 15, 0, "ABCDEFGHTEX".into()));
    assert_eq!(parsed("a+b", 0), (0, 1, 0, "A          ".into()));
    assert_eq!(parsed("dir\\file", 0), (0, 3, 0, "DIR        ".into()));
    assert_eq!(parsed("x.y.z", 0), (0, 3, 0, "X       Y  ".into()));

    // Bits 1-3 keep fields that the string does not give.
    let old = *b"\x05OLDNAME EXT\x11\x22\x33\x44";
    let mut f = old;
    assert_eq!(parse("", &mut f, 0x0e), (0, 0));
    assert_eq!(&f[..12], &old[..12]);
    assert_eq!(&f[12..], &[0; 4]);
    let mut f = old;
    parse("new", &mut f, 0x0e);
    assert_eq!(&f[..12], b"\x05NEW     EXT");
    let mut f = old;
    parse("b:.c", &mut f, 0x0e);
    assert_eq!(&f[..12], b"\x02OLDNAME C  ");
    // A dot with nothing after it blanks the extension even with bit 3.
    let mut f = old;
    parse("new.", &mut f, 0x08);
    assert_eq!(&f[..12], b"\0NEW        ");
}

#[test]
fn dos_parse_call_registers() {
    let mut r = Rig::new("parse-call");
    r.put(0x3000, 0x10, b"  c:Stara.Com , next\r");
    r.cpu.sregs[DS] = 0x3000;
    r.cpu.regs[SI] = 0x10;
    r.cpu.sregs[ES] = FCB_SEG;
    r.cpu.regs[DI] = FCB_OFF;
    assert_eq!(r.al(0x2901), 0);
    assert_eq!(r.cpu.reg8(AH), 0x29);
    assert_eq!(r.cpu.regs[SI], 0x10 + 13);
    assert_eq!(r.cpu.regs[DI], FCB_OFF);
    assert_eq!(r.get(FCB_SEG, FCB_OFF, 16), b"\x03STARA   COM\0\0\0\0");
    // Continuing from SI with bit 0 skips the blank and the comma.
    assert_eq!(r.al(0x2901), 0);
    assert_eq!(r.get(FCB_SEG, FCB_OFF, 12), b"\0NEXT       ");
}

#[test]
fn dos_open_close() {
    let mut r = Rig::new("open");
    r.file("stara.com", &pattern(1000));
    let mtime = fs::metadata(r.dir.join("stara.com")).unwrap().modified().unwrap();
    assert_eq!(r.parse_fcb("STARA.COM"), 0);
    r.set16(CUR_BLOCK, 0x7777);
    r.set16(REC_SIZE, 0x4444);
    r.ram[phys(FCB_SEG, FCB_OFF + CUR_REC) as usize] = 0x55;
    r.set32(RAND_REC, 0x66666666);
    assert_eq!(r.al(0x0f00), 0);
    assert_eq!(r.cpu.reg8(AH), 0x0f);
    assert_eq!(r.f8(DRIVE), DRIVE_C);
    assert_eq!(r.f16(CUR_BLOCK), 0);
    assert_eq!(r.f16(REC_SIZE), 128);
    assert_eq!(r.f32(FILE_SIZE), 1000);
    let t = LocalTime::of(mtime);
    assert_eq!((r.f16(DATE), r.f16(TIME)), (t.dos_date(), t.dos_time()));
    assert_eq!(r.f8(CUR_REC), 0x55);
    assert_eq!(r.f32(RAND_REC), 0x66666666);
    assert_eq!(r.al(0x1000), 0);
    // Closing twice, or a file that is not there, is fine.
    assert_eq!(r.al(0x1000), 0);

    // Drive byte 3 works; A: and invalid drives fail.
    r.parse_fcb("c:stara.com");
    assert_eq!(r.al(0x0f00), 0);
    r.parse_fcb("a:stara.com");
    assert_eq!(r.al(0x0f00), 0xff);
    r.parse_fcb("stara.com");
    r.ram[phys(FCB_SEG, FCB_OFF) as usize] = 9;
    assert_eq!(r.al(0x0f00), 0xff);
    r.parse_fcb("missing.com");
    assert_eq!(r.al(0x0f00), 0xff);
    r.parse_fcb("");
    assert_eq!(r.al(0x0f00), 0xff, "blank name");
    r.parse_fcb("star?.*");
    assert_eq!(r.al(0x0f00), 0, "wildcards open the first match");
    assert_eq!(r.f32(FILE_SIZE), 1000);
    // Lower case in the FCB itself is folded.
    r.put(FCB_SEG, FCB_OFF, b"\0stara   com");
    assert_eq!(r.al(0x0f00), 0);
}

#[test]
fn dos_open_device_is_unsupported() {
    let mut r = Rig::new("device");
    r.parse_fcb("nul");
    assert!(matches!(r.call(0x0f00), Hle::Unsupported(m) if m.contains("NUL")));
}

#[test]
fn dos_random_read() {
    let mut r = Rig::new("read");
    let data = pattern(2500);
    r.file("STARA.COM", &data);
    r.open("stara.com", 1024);

    assert_eq!(r.read(0), 0);
    assert_eq!(r.get(DTA_SEG, 0, 1024), &data[..1024]);
    assert_eq!(r.read(1), 0);
    assert_eq!(r.get(DTA_SEG, 0, 1024), &data[1024..2048]);

    // Partial: 452 bytes, the rest of the record zeroed, nothing past it.
    r.put(DTA_SEG, 0, &[0xee; 1100]);
    assert_eq!(r.read(2), 3);
    assert_eq!(r.get(DTA_SEG, 0, 452), &data[2048..]);
    assert_eq!(r.get(DTA_SEG, 452, 572), vec![0; 572]);
    assert_eq!(r.get(DTA_SEG, 1024, 76), vec![0xee; 76]);

    // Past the end: AL=1 and the DTA is left alone.
    r.put(DTA_SEG, 0, &[0xee; 1024]);
    assert_eq!(r.read(3), FEOF);
    assert_eq!(r.get(DTA_SEG, 0, 1024), vec![0xee; 1024]);

    let log: Vec<_> = r.dos.io_log.iter().map(|io| (io.file.as_str(), io.offset, io.len, io.write, io.addr)).collect();
    let dta = phys(DTA_SEG, 0);
    assert_eq!(
        log,
        [
            ("STARA.COM", 0, 1024, false, dta),
            ("STARA.COM", 1024, 1024, false, dta),
            ("STARA.COM", 2048, 1024, false, dta),
            ("STARA.COM", 3072, 0, false, dta),
        ]
    );
}

#[test]
fn dos_display_memory_and_roms() {
    let mut r = Rig::new("display");
    r.dos.log_io = false;
    let data = pattern(256);
    r.file("STARA.COM", &data);
    r.open("STARA.COM", 128);
    // A read into display memory reaches the adapter (text mode: B8000).
    r.dos.dta = (0xb800, 0x0010);
    assert_eq!(r.read(1), 0);
    let shown: Vec<u8> = (0..128).map(|i| r.video.mem_read(0xb8010 + i)).collect();
    assert_eq!(shown, &data[128..]);
    assert!(r.ram[0xb8000..0xb8100].iter().all(|&b| b == 0));
    // Writing a file from display memory stops, and the file is untouched.
    let err = r.call(0x2200);
    assert!(matches!(&err, Hle::Unsupported(m) if m.contains("AH=22h") && m.contains("B8010")), "{err:?}");
    assert_eq!(fs::read(r.dir.join("STARA.COM")).unwrap(), data);
    // A read into the ROMs stops, and they keep their bytes.
    r.dos.dta = (0xf000, 0xff80);
    r.ram[0xfff80..].fill(0xcc);
    let err = r.call(0x2100);
    assert!(matches!(&err, Hle::Unsupported(m) if m.contains("ROM area at FFF80")), "{err:?}");
    assert!(r.ram[0xfff80..].iter().all(|&b| b == 0xcc));
    // So does an FCB function whose FCB sits there.
    r.cpu.sregs[DS] = 0xc000;
    r.cpu.regs[DX] = 0x0100;
    r.ram[0xc0100..0xc010c].copy_from_slice(b"\0STARA   COM");
    let err = r.call(0x0f00);
    assert!(matches!(&err, Hle::Unsupported(m) if m.contains("ROM area at C0100")), "{err:?}");
    assert_eq!(r.ram[0xc0100], 0);
    // And a string in display memory.
    r.cpu.sregs[DS] = 0xb800;
    r.cpu.regs[DX] = 0;
    let err = r.call(0x0900);
    assert!(matches!(&err, Hle::Unsupported(m) if m.contains("display memory at B8000")), "{err:?}");
    assert!(r.dos.console.is_empty());
}

#[test]
fn dos_random_record_fields() {
    let mut r = Rig::new("rr");
    let data = pattern(300 * 128 + 50);
    r.file("STARB.COM", &data);
    r.open("starb.com", 128);

    // Block and record follow the random record, which does not advance.
    assert_eq!(r.read(300), 3);
    assert_eq!((r.f16(CUR_BLOCK), r.f8(CUR_REC)), (2, 44));
    assert_eq!(r.f32(RAND_REC), 300);
    assert_eq!(r.get(DTA_SEG, 0, 50), &data[300 * 128..]);

    // With records of 64 bytes or more the fourth byte is ignored.
    assert_eq!(r.read(0x7f00_0001), 0);
    assert_eq!(r.get(DTA_SEG, 0, 128), &data[128..256]);
    assert_eq!(r.f32(RAND_REC), 0x7f00_0001);
    // Below 64 it counts.
    r.set16(REC_SIZE, 32);
    assert_eq!(r.read(0x0100_0000), FEOF);
    assert_eq!(r.read(3), 0);
    assert_eq!(r.get(DTA_SEG, 0, 32), &data[96..128]);

    // Record size 0 means 128 and is stored.
    r.set16(REC_SIZE, 0);
    assert_eq!(r.read(1), 0);
    assert_eq!(r.f16(REC_SIZE), 128);
    assert_eq!(r.get(DTA_SEG, 0, 128), &data[128..256]);

    // Reads stop at the FCB's file size, not the host file's.
    r.set32(FILE_SIZE, 200);
    assert_eq!(r.read(1), 3);
    assert_eq!(r.get(DTA_SEG, 0, 72), &data[128..200]);
    assert_eq!(r.get(DTA_SEG, 72, 56), vec![0; 56]);
}

#[test]
fn dos_random_write_extends_file() {
    let mut r = Rig::new("write");
    r.file("STARA.COM", &pattern(1000));
    r.open("STARA.COM", 512);
    r.set16(DATE, 0);
    r.set16(TIME, 0);

    let rec = vec![0x5a; 512];
    let before = LocalTime::now();
    assert_eq!(r.write(3, &rec), 0);
    let after = LocalTime::now();
    assert_eq!(r.cpu.reg8(AH), 0x22);
    // On the host at once, without a close.
    let host = fs::read(r.dir.join("STARA.COM")).unwrap();
    assert_eq!(host.len(), 2048);
    assert_eq!(&host[..1000], &pattern(1000)[..]);
    assert!(host[1000..1536].iter().all(|&b| b == 0));
    assert_eq!(&host[1536..], &rec[..]);
    assert_eq!(r.f32(FILE_SIZE), 2048);
    assert_eq!((r.f16(CUR_BLOCK), r.f8(CUR_REC), r.f32(RAND_REC)), (0, 3, 3));
    let stamp = (r.f16(DATE), r.f16(TIME));
    assert!(stamp >= (before.dos_date(), before.dos_time()) && stamp <= (after.dos_date(), after.dos_time()));

    // Overwriting inside the file keeps its size.
    assert_eq!(r.write(0, &[0x11; 512]), 0);
    assert_eq!(r.f32(FILE_SIZE), 2048);
    assert_eq!(fs::metadata(r.dir.join("STARA.COM")).unwrap().len(), 2048);

    // And it reads back.
    r.put(DTA_SEG, 0, &[0; 512]);
    assert_eq!(r.read(3), 0);
    assert_eq!(r.get(DTA_SEG, 0, 512), rec);

    let writes: Vec<_> = r.dos.io_log.iter().filter(|io| io.write).map(|io| (io.offset, io.len)).collect();
    assert_eq!(writes, [(1536, 512), (0, 512)]);
}

#[test]
fn dos_random_io_without_open() {
    let mut r = Rig::new("no-open");
    r.file("STARA.COM", &pattern(256));
    // An FCB that was never opened still finds its file by name, within
    // the file size it carries.
    r.parse_fcb("stara.com");
    r.set16(REC_SIZE, 128);
    r.set32(FILE_SIZE, 256);
    assert_eq!(r.read(1), 0);
    assert_eq!(r.get(DTA_SEG, 0, 128), &pattern(256)[128..]);
    // A missing file or a bad drive reads as end of file.
    r.parse_fcb("gone.com");
    assert_eq!(r.read(0), FEOF);
    r.parse_fcb("a:stara.com");
    assert_eq!(r.read(0), FEOF);
}

#[test]
fn dos_dta_segment_wrap() {
    let mut r = Rig::new("wrap");
    r.file("STARA.COM", &pattern(4096));
    r.open("STARA.COM", 512);
    r.dos.dta = (DTA_SEG, 0xff00);
    r.put(DTA_SEG, 0xff00, &[0xee; 0x100]);
    assert_eq!(r.read(0), FTRIM);
    assert_eq!(r.get(DTA_SEG, 0xff00, 0x100), vec![0xee; 0x100]);
    assert!(r.dos.io_log.is_empty());
    // A record ending exactly at the segment's end still moves, with AL=2.
    r.dos.dta = (DTA_SEG, 0xfe00);
    assert_eq!(r.read(1), FTRIM);
    assert_eq!(r.get(DTA_SEG, 0xfe00, 512), &pattern(4096)[512..1024]);
}

#[test]
fn dos_extended_fcb() {
    let mut r = Rig::new("ext");
    r.file("STARA.COM", &pattern(600));
    r.put(FCB_SEG, FCB_OFF, &[0xff, 0, 0, 0, 0, 0, 0x00]);
    r.put(FCB_SEG, FCB_OFF + 7, b"\0STARA   COM");
    assert_eq!(r.al(0x0f00), 0);
    let at = |r: &Rig, f: u16| rd8(&r.ram, phys(FCB_SEG, FCB_OFF + 7 + f));
    assert_eq!(at(&r, DRIVE), DRIVE_C);
    assert_eq!(rd16(&r.ram, phys(FCB_SEG, FCB_OFF + 7 + REC_SIZE)), 128);
    wr16(&mut r.ram, phys(FCB_SEG, FCB_OFF + 7 + RAND_REC), 4);
    assert_eq!(r.al(0x2100), 3);
    assert_eq!(r.get(DTA_SEG, 0, 88), &pattern(600)[512..]);
    assert_eq!(at(&r, CUR_REC), 4);
    assert_eq!(r.al(0x1000), 0);
}

#[test]
fn dos_find_first() {
    let mut r = Rig::new("find");
    r.file("stara.com", &pattern(1234));
    r.file("STARB.COM", &[1, 2, 3]);
    r.file("readme.txt", b"x");
    r.file("longfilename.text", b"x");
    r.file("two.dots.txt", b"x");
    fs::create_dir(r.dir.join("SAVES")).unwrap();

    r.parse_fcb("star?.com");
    assert_eq!(r.al(0x1100), 0);
    let d = r.get(DTA_SEG, 0, 33);
    assert_eq!(d[0], DRIVE_C);
    assert_eq!(&d[1..12], b"STARA   COM");
    assert_eq!(d[12], host::ARCHIVE);
    assert_eq!(u32::from_le_bytes(d[29..33].try_into().unwrap()), 1234);
    let mtime = fs::metadata(r.dir.join("stara.com")).unwrap().modified().unwrap();
    let t = LocalTime::of(mtime);
    assert_eq!(u16::from_le_bytes([d[23], d[24]]), t.dos_time());
    assert_eq!(u16::from_le_bytes([d[25], d[26]]), t.dos_date());

    r.parse_fcb("c:starb.*");
    assert_eq!(r.al(0x1100), 0);
    assert_eq!(r.get(DTA_SEG, 0, 12), b"\x03STARB   COM");

    // Directories need the attribute; plain FCBs skip them.
    r.parse_fcb("s*.*");
    assert_eq!(r.al(0x1100), 0);
    assert_eq!(r.get(DTA_SEG, 1, 11), b"STARA   COM");
    r.put(FCB_SEG, FCB_OFF, &[0xff, 0, 0, 0, 0, 0, host::DIRECTORY]);
    r.put(FCB_SEG, FCB_OFF + 7, b"\0S??????????");
    assert_eq!(r.al(0x1100), 0);
    let d = r.get(DTA_SEG, 0, 40);
    assert_eq!(&d[..7], &[0xff, 0, 0, 0, 0, 0, host::DIRECTORY]);
    assert_eq!(d[7], DRIVE_C);
    assert_eq!(&d[8..19], b"SAVES      ");
    assert_eq!(d[19], host::DIRECTORY);

    // Names without an 8.3 form are invisible.
    r.parse_fcb("longfi*.*");
    assert_eq!(r.al(0x1100), 0xff);
    r.parse_fcb("two.*");
    assert_eq!(r.al(0x1100), 0xff);
    r.parse_fcb("nothing.*");
    assert_eq!(r.al(0x1100), 0xff);
    r.parse_fcb("a:*.*");
    assert_eq!(r.al(0x1100), 0xff);
}

#[cfg(unix)]
#[test]
fn dos_symlinks_are_not_followed() {
    let mut r = Rig::new("symlink");
    let outside = r.dir.with_extension("outside");
    fs::write(&outside, b"original").unwrap();
    std::os::unix::fs::symlink(&outside, r.dir.join("LINK.COM")).unwrap();
    r.parse_fcb("link.com");
    assert_eq!(r.al(0x0f00), 0xff);
    assert_eq!(r.al(0x1100), 0xff);
    r.set16(REC_SIZE, 8);
    r.set32(FILE_SIZE, 8);
    assert_eq!(r.write(0, b"clobber!"), FEOF);
    assert_eq!(fs::read(&outside).unwrap(), b"original");
    fs::remove_file(&outside).unwrap();
}

#[test]
fn dos_resize_block() {
    let mut r = Rig::new("resize");
    let mut cpu = Cpu::new();
    r.dos.load_com(&mut cpu, &mut r.ram, 0x0800, &[0x90], "");
    r.cpu.sregs[ES] = 0x0800;

    r.cpu.regs[BX] = 0x9800;
    assert_eq!(r.call(0x4a00), Hle::Done);
    assert!(!r.stacked_cf());
    assert_eq!(r.cpu.regs[AX], 0x0800);

    r.cpu.regs[BX] = 0x9801;
    assert_eq!(r.call(0x4a00), Hle::Done);
    assert!(r.stacked_cf());
    assert_eq!((r.cpu.regs[AX], r.cpu.regs[BX]), (8, 0x9800));

    r.cpu.regs[BX] = 0x1000;
    assert_eq!(r.call(0x4a00), Hle::Done);
    assert!(!r.stacked_cf());

    // Any block may grow up to A000.
    r.cpu.sregs[ES] = 0x5000;
    r.cpu.regs[BX] = 0x5000;
    assert_eq!(r.call(0x4a00), Hle::Done);
    assert!(!r.stacked_cf());
    r.cpu.regs[BX] = 0x5001;
    assert_eq!(r.call(0x4a00), Hle::Done);
    assert!(r.stacked_cf());
    assert_eq!((r.cpu.regs[AX], r.cpu.regs[BX]), (8, 0x5000));
}

#[test]
fn dos_small_functions() {
    let mut r = Rig::new("small");
    assert_eq!(r.al(0x1900), 2);
    assert_eq!(r.call(0x0d00), Hle::Done);

    r.cpu.regs[DX] = b'A' as u16;
    assert_eq!(r.al(0x0200), b'A');
    r.cpu.regs[DX] = b'\t' as u16;
    assert_eq!(r.al(0x0200), b' ');
    r.put(0x3000, 0, b"hi$there");
    r.cpu.sregs[DS] = 0x3000;
    r.cpu.regs[DX] = 0;
    assert_eq!(r.al(0x0900), b'$');
    assert_eq!(r.dos.console, b"A\thi");

    r.cpu.sregs[DS] = 0x1234;
    r.cpu.regs[DX] = 0x0010;
    assert_eq!(r.al(0x1a00), 0x00);
    assert_eq!(r.dos.dta, (0x1234, 0x0010));

    r.cpu.sregs[DS] = 0xabcd;
    r.cpu.regs[DX] = 0x0123;
    assert_eq!(r.call(0x2560), Hle::Done);
    assert_eq!(r.get(0, 0x180, 4), [0x23, 0x01, 0xcd, 0xab]);
    r.cpu.regs[BX] = 0;
    r.cpu.sregs[ES] = 0;
    assert_eq!(r.call(0x3560), Hle::Done);
    assert_eq!((r.cpu.sregs[ES], r.cpu.regs[BX]), (0xabcd, 0x0123));

    r.cpu.regs[BX] = 0xffff;
    r.cpu.regs[CX] = 0xffff;
    assert_eq!(r.call(0x3000), Hle::Done);
    assert_eq!([r.cpu.regs[AX], r.cpu.regs[BX], r.cpu.regs[CX]], [0x1e03, 0, 0]);

    assert_eq!(r.call(0x0000), Hle::Exit(0));
    assert_eq!(r.call(0x4c07), Hle::Exit(7));
}

#[test]
fn dos_unimplemented_functions_stop() {
    let mut r = Rig::new("unsupported");
    for ax in [0x0100u16, 0x0e02, 0x1200, 0x1400, 0x1600, 0x2700, 0x2a00, 0x3d02, 0x4800] {
        let want = format!("AH={:02X} AL={:02X}", ax >> 8, ax & 0xff);
        assert!(matches!(r.call(ax), Hle::Unsupported(m) if m.contains(&want)), "{ax:04X}");
    }
}

#[test]
fn dos_host_names() {
    assert_eq!(host::dos_name("stara.com"), Some(*b"STARA   COM"));
    assert_eq!(host::dos_name("README"), Some(*b"README     "));
    assert_eq!(host::dos_name("a-b_c$~!.x1"), Some(*b"A-B_C$~!X1 "));
    for bad in ["", ".hidden", "name.", "toolongname", "a.long", "two.dots.x", "sp ace", "wild?", "a+b", "caf\u{e9}"] {
        assert_eq!(host::dos_name(bad), None, "{bad:?}");
    }
    assert_eq!(host::display(b"STARA   COM"), "STARA.COM");
    assert_eq!(host::display(b"README     "), "README");
    assert!(host::matches(b"STAR?   ???", b"STARB   COM"));
    assert!(!host::matches(b"STAR?   ???", b"STARBX  COM"));
    assert!(host::attr_matches(0, host::ARCHIVE | host::READ_ONLY));
    assert!(!host::attr_matches(0, host::DIRECTORY));
    assert!(host::attr_matches(host::DIRECTORY, host::DIRECTORY));
    assert!(!host::attr_matches(host::VOLUME, host::ARCHIVE));
}

#[test]
fn dos_date_time_words() {
    // 2000-03-01 12:34:56 UTC.
    let t = clock::utc(951_914_096);
    assert_eq!(t, LocalTime { year: 2000, month: 3, day: 1, hour: 12, minute: 34, second: 56 });
    assert_eq!(t.dos_date(), 20 << 9 | 3 << 5 | 1);
    assert_eq!(t.dos_time(), 12 << 11 | 34 << 5 | 28);
    assert_eq!(clock::utc(0).dos_date(), 1 << 5 | 1, "before 1980 clamps");
    assert_eq!(clock::utc(-86_400).day, 31);

    let now = LocalTime::now();
    assert!((2024..2108).contains(&now.year));
    assert!((1..=12).contains(&now.month) && (1..=31).contains(&now.day));
    assert!(now.hour < 24 && now.minute < 60 && now.second < 60);
    // Local time is within a day of UTC.
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let date = |t: LocalTime| (t.year, t.month, t.day);
    assert!([-86_400, 0, 86_400].iter().any(|d| date(clock::utc(secs + d)) == date(now)));
    // File times go through the same conversion.
    let t = LocalTime::of(UNIX_EPOCH + Duration::from_secs(secs as u64));
    assert!([-86_400, 0, 86_400].iter().any(|d| date(clock::utc(secs + d)) == date(t)));
}
