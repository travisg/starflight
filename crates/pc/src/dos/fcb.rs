// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! File control blocks: name parsing (29h) and the FCB file functions the
//! game uses (0Fh, 10h, 11h, 21h, 22h).
//!
//! Host files are found by the FCB's name on every call, through a cache of
//! open files, so an FCB works wherever it sits in memory, as DOS's FCB
//! regeneration allows.

use super::clock::LocalTime;
use super::host::{self, Name};
use super::{Dos, FileIo};
use crate::hle::{DISPLAY, Hle, Mem};
use cpu8086::{Cpu, DS, DX, phys};

// Offsets in a normal FCB. An extended FCB is FF, five reserved bytes, the
// attribute, then a normal FCB.
pub(super) const DRIVE: u16 = 0x00;
pub(super) const NAME: u16 = 0x01;
pub(super) const CUR_BLOCK: u16 = 0x0c;
pub(super) const REC_SIZE: u16 = 0x0e;
pub(super) const FILE_SIZE: u16 = 0x10;
pub(super) const DATE: u16 = 0x14;
pub(super) const TIME: u16 = 0x16;
pub(super) const CUR_REC: u16 = 0x20;
pub(super) const RAND_REC: u16 = 0x21;

/// C: as an FCB drive byte (A: = 1).
pub(super) const DRIVE_C: u8 = 3;
/// Drive letters A: to C: are valid in names; only C: holds files.
const DRIVES: u8 = 3;

// FCB read/write results (FCBIO in MS-DOS 4.0 FCBIO2.ASM).
/// End of file, nothing (more) read; or the file is not there.
pub(super) const FEOF: u8 = 1;
/// The transfer would wrap the DTA segment.
pub(super) const FTRIM: u8 = 2;

/// GetLet's terminators (MS-DOS 4.0 FCB.ASM): control characters and
/// `. " / \ [ ] : < | > + = ; ,` space and tab.
pub(super) fn is_terminator(c: u8) -> bool {
    c < 0x20 || b".\"/\\[]:<|>+=;, ".contains(&c)
}

/// Delim: separators that 29h with AL bit 0 skips once.
fn is_separator(c: u8) -> bool {
    b":<|>+=;, \t".contains(&c)
}

fn is_blank(c: u8) -> bool {
    c == b' ' || c == b'\t'
}

/// Upper case as GetLet does for a-z. Bytes 80h and up stay as they are; DOS
/// 3.3 maps them through the country case table.
fn upper(c: u8) -> u8 {
    c.to_ascii_uppercase()
}

/// 29h, MakeFcb in MS-DOS 4.0 FCB.ASM. `get` reads the string at an offset
/// in its segment and `fcb` is the 16 bytes at ES:DI. Returns AL (0, 1 if a
/// `?` or `*` was seen, FF for a bad drive letter) and the offset of the
/// character that ended the parse.
pub(super) fn parse_name(get: impl Fn(u16) -> u8, mut si: u16, fcb: &mut [u8; 16], flags: u8) -> (u8, u16) {
    let mut al = 0u8;
    if flags & 2 == 0 {
        fcb[0] = 0;
    }
    if flags & 4 == 0 {
        fcb[1..9].fill(b' ');
    }
    if flags & 8 == 0 {
        fcb[9..12].fill(b' ');
    }
    // Current block and record size.
    fcb[12..16].fill(0);
    let skip_blanks = |mut si: u16| {
        while is_blank(get(si)) {
            si = si.wrapping_add(1);
        }
        si
    };
    // Blanks always go; bit 0 also drops one separator and the blanks after it.
    if flags & 1 != 0 {
        si = skip_blanks(si);
        if is_separator(get(si)) {
            si = si.wrapping_add(1);
        }
    }
    si = skip_blanks(si);
    let c = upper(get(si));
    if !is_terminator(c) && get(si.wrapping_add(1)) == b':' {
        // A bad letter still lands in the drive byte.
        let d = c.wrapping_sub(b'@');
        if c <= b'@' || d > DRIVES {
            al = 0xff;
        }
        fcb[0] = d;
        si = si.wrapping_add(2);
    }
    // A missing name leaves the field (blank, or kept with bit 2); a missing
    // extension after a dot blanks it even with bit 3.
    if !is_terminator(upper(get(si))) {
        si = get_word(&get, si, &mut fcb[1..9], &mut al);
    }
    if get(si) == b'.' {
        si = get_word(&get, si.wrapping_add(1), &mut fcb[9..12], &mut al);
    }
    (al, si)
}

/// MUSTGETWORD: fills `field` and blank-pads it; `*` fills the rest with `?`.
/// Characters past the field's length are consumed and dropped.
fn get_word(get: &impl Fn(u16) -> u8, mut si: u16, field: &mut [u8], al: &mut u8) -> u16 {
    let mut pos = 0;
    // A segment with no terminator at all would loop forever in DOS.
    for _ in 0..=0xffff {
        let c = upper(get(si));
        if is_terminator(c) {
            break;
        }
        si = si.wrapping_add(1);
        if pos == field.len() {
            continue;
        }
        if c == b'*' {
            field[pos..].fill(b'?');
            pos = field.len();
            *al |= 1;
        } else {
            field[pos] = c;
            pos += 1;
            if c == b'?' {
                *al |= 1;
            }
        }
    }
    field[pos..].fill(b' ');
    si
}

/// An FCB in emulated memory; `off` addresses the normal part.
struct Fcb {
    seg: u16,
    off: u16,
    /// Attribute byte of an extended FCB.
    attr: Option<u8>,
}

impl Fcb {
    fn at(mem: &Mem, seg: u16, off: u16) -> Fcb {
        if mem.rd8(phys(seg, off)) == 0xff {
            Fcb { seg, off: off.wrapping_add(7), attr: Some(mem.rd8(phys(seg, off.wrapping_add(6)))) }
        } else {
            Fcb { seg, off, attr: None }
        }
    }

    fn addr(&self, field: u16) -> u32 {
        phys(self.seg, self.off.wrapping_add(field))
    }

    fn rd8(&self, mem: &Mem, field: u16) -> u8 {
        mem.rd8(self.addr(field))
    }

    fn wr8(&self, mem: &mut Mem, field: u16, v: u8) {
        mem.wr8(self.addr(field), v)
    }

    fn rd16(&self, mem: &Mem, field: u16) -> u16 {
        self.rd8(mem, field) as u16 | (self.rd8(mem, field + 1) as u16) << 8
    }

    fn wr16(&self, mem: &mut Mem, field: u16, v: u16) {
        self.wr8(mem, field, v as u8);
        self.wr8(mem, field + 1, (v >> 8) as u8);
    }

    fn rd32(&self, mem: &Mem, field: u16) -> u32 {
        self.rd16(mem, field) as u32 | (self.rd16(mem, field + 2) as u32) << 16
    }

    fn wr32(&self, mem: &mut Mem, field: u16, v: u32) {
        self.wr16(mem, field, v as u16);
        self.wr16(mem, field + 2, (v >> 16) as u16);
    }

    /// The upper-cased name if the drive is C: (0 = default) and the name is
    /// one DOS accepts: not starting with a blank, and no control characters
    /// or terminators other than blanks (LodName, MS-DOS 4.0 FCB.ASM).
    fn name(&self, mem: &Mem) -> Option<Name> {
        if !matches!(self.rd8(mem, DRIVE), 0 | DRIVE_C) {
            return None;
        }
        let mut n = [0u8; 11];
        for (i, b) in n.iter_mut().enumerate() {
            let c = upper(self.rd8(mem, NAME + i as u16));
            if c != b' ' && is_terminator(c) {
                return None;
            }
            *b = c;
        }
        (n[0] != b' ').then_some(n)
    }

    /// GetRecSize: 0 means 128, which is stored back.
    fn rec_size(&self, mem: &mut Mem) -> u32 {
        let r = self.rd16(mem, REC_SIZE);
        if r != 0 {
            return r as u32;
        }
        self.wr16(mem, REC_SIZE, 128);
        128
    }

    /// GetRR: four bytes, or three when records are 64 bytes or more.
    fn random_record(&self, mem: &Mem, rec_size: u32) -> u32 {
        let rr = self.rd32(mem, RAND_REC);
        if rec_size >= 64 { rr & 0x00ff_ffff } else { rr }
    }

    /// SetExtent: current block and current record (0-127) for record `rec`.
    fn set_position(&self, mem: &mut Mem, rec: u32) {
        self.wr8(mem, CUR_REC, (rec & 0x7f) as u8);
        self.wr16(mem, CUR_BLOCK, (rec >> 7) as u16);
    }
}

/// Stops the machine on a failure of the host file system, rather than
/// handing the game a DOS error for a problem in the emulator's environment.
fn host_error(ah: u8, name: &Name, e: std::io::Error) -> Hle {
    Hle::Unsupported(format!("INT 21h AH={ah:02X} {}: host file error: {e}", host::display(name)))
}

/// Device names DOS would open instead of a file; this machine has none.
fn check_device(ah: u8, name: &Name) -> Result<(), Hle> {
    const DEVICES: &[&[u8]] =
        &[b"CON", b"AUX", b"PRN", b"NUL", b"COM1", b"COM2", b"COM3", b"COM4", b"LPT1", b"LPT2", b"LPT3", b"CLOCK$"];
    let base = name[..8].trim_ascii_end();
    if DEVICES.contains(&base) {
        return Err(Hle::Unsupported(format!("INT 21h AH={ah:02X} on device {}", String::from_utf8_lossy(base))));
    }
    Ok(())
}

impl Dos {
    /// 0Fh, $FCB_Open in MS-DOS 4.0 FCBIO2.ASM. Sets the drive byte, current
    /// block 0, record size 128, file size, date and time; leaves the current
    /// and random record fields alone. A `?` opens the first match.
    pub(super) fn fcb_open(&mut self, cpu: &Cpu, mem: &mut Mem) -> Result<u8, Hle> {
        let fcb = Fcb::at(mem, cpu.sregs[DS], cpu.regs[DX]);
        let Some(name) = fcb.name(mem) else { return Ok(0xff) };
        check_device(0x0f, &name)?;
        let found = self.files.get(&self.drive_dir, &name).and_then(|f| match f {
            Some((file, _)) => file.metadata().map(Some),
            None => Ok(None),
        });
        let Some(meta) = found.map_err(|e| host_error(0x0f, &name, e))? else { return Ok(0xff) };
        let t = LocalTime::of(meta.modified().unwrap_or(std::time::UNIX_EPOCH));
        fcb.wr8(mem, DRIVE, DRIVE_C);
        fcb.wr16(mem, CUR_BLOCK, 0);
        fcb.wr16(mem, REC_SIZE, 128);
        fcb.wr32(mem, FILE_SIZE, meta.len().min(u32::MAX as u64) as u32);
        fcb.wr16(mem, DATE, t.dos_date());
        fcb.wr16(mem, TIME, t.dos_time());
        Ok(0)
    }

    /// 10h. Writes are already on the host, so this drops the open file.
    /// AL=0 always, as $FCB_Close (MS-DOS 4.0 FCBIO.ASM) gives for FCBs it
    /// cannot find or that were closed before.
    pub(super) fn fcb_close(&mut self, cpu: &Cpu, mem: &mut Mem) -> Result<u8, Hle> {
        let fcb = Fcb::at(mem, cpu.sregs[DS], cpu.regs[DX]);
        if let Some(name) = fcb.name(mem) {
            self.files.close(&self.drive_dir, &name).map_err(|e| host_error(0x10, &name, e))?;
        }
        Ok(0)
    }

    /// 11h, $Dir_Search_First in MS-DOS 4.0 SEARCH.ASM. The DTA gets the
    /// extended header (FF, five zeros, the search attribute) for an extended
    /// FCB, then the drive byte and the 32-byte directory entry. The search
    /// position for 12h is not kept, as 12h is not implemented.
    pub(super) fn fcb_find_first(&mut self, cpu: &Cpu, mem: &mut Mem) -> Result<u8, Hle> {
        let fcb = Fcb::at(mem, cpu.sregs[DS], cpu.regs[DX]);
        let Some(pattern) = fcb.name(mem) else { return Ok(0xff) };
        let search = fcb.attr.unwrap_or(0);
        let entries = host::list(&self.drive_dir).map_err(|e| host_error(0x11, &pattern, e))?;
        let Some(e) = entries.iter().find(|e| host::matches(&pattern, &e.name) && host::attr_matches(search, e.attr))
        else {
            return Ok(0xff);
        };
        let mut out = Vec::with_capacity(40);
        if let Some(a) = fcb.attr {
            out.extend_from_slice(&[0xff, 0, 0, 0, 0, 0, a]);
        }
        out.push(match fcb.rd8(mem, DRIVE) {
            0 => DRIVE_C,
            d => d,
        });
        out.extend_from_slice(&e.dir_entry());
        let (seg, off) = self.dta;
        for (i, &b) in out.iter().enumerate() {
            mem.wr8(phys(seg, off.wrapping_add(i as u16)), b);
        }
        Ok(0)
    }

    /// 21h and 22h: one record at the random record number. FCBIO in MS-DOS
    /// 4.0 FCBIO2.ASM with the Random flag:
    /// - the current block and record are set from the random record, which
    ///   itself does not advance;
    /// - reads stop at the FCB's file size: AL=0 full record, 1 nothing read,
    ///   3 partial record padded with zeros;
    /// - writes go straight to the host file, raise the FCB's file size to
    ///   cover them and stamp its date and time;
    /// - AL=2 when the record would run past the end of the DTA's segment.
    pub(super) fn fcb_random(&mut self, cpu: &Cpu, mem: &mut Mem, write: bool) -> Result<u8, Hle> {
        let ah = if write { 0x22 } else { 0x21 };
        let fcb = Fcb::at(mem, cpu.sregs[DS], cpu.regs[DX]);
        let rec_size = fcb.rec_size(mem);
        let rec = fcb.random_record(mem, rec_size);
        fcb.set_position(mem, rec);
        // DOS forms the byte position in 32 bits.
        let pos = rec.wrapping_mul(rec_size);

        let (seg, off) = self.dta;
        let mut err = 0;
        let mut records = 1;
        if off as u32 + rec_size > 0xffff {
            err = FTRIM;
            let room = if off == 0 { 0xffff } else { 0x10000 - off as u32 };
            records = room / rec_size;
        }
        let want = records * rec_size;
        let dta = phys(seg, off);

        // Bytes moved to or from the file, and the file's name.
        let mut moved = 0;
        let mut file_name = None;
        if want != 0 {
            let Some(name) = fcb.name(mem) else { return Ok(FEOF) };
            let opened = self.files.get(&self.drive_dir, &name).map_err(|e| host_error(ah, &name, e))?;
            let Some((file, actual)) = opened else { return Ok(FEOF) };
            let size = fcb.rd32(mem, FILE_SIZE);
            if write {
                if let Some(a) = (0..want).map(|i| (dta + i) & 0xfffff).find(|a| DISPLAY.contains(a)) {
                    return Err(Hle::Unsupported(format!(
                        "INT 21h AH=22h from a DTA in display memory ({a:05X}): not supported"
                    )));
                }
                let data: Vec<u8> = (0..want).map(|i| mem.rd8(dta + i)).collect();
                host::write_at(file, pos as u64, &data).map_err(|e| host_error(ah, &actual, e))?;
                let end = (pos as u64 + want as u64).min(u32::MAX as u64) as u32;
                fcb.wr32(mem, FILE_SIZE, size.max(end));
                moved = want;
            } else {
                let avail = size.saturating_sub(pos).min(want);
                let data = host::read_at(file, pos as u64, avail).map_err(|e| host_error(ah, &actual, e))?;
                mem.put(dta, &data);
                moved = data.len() as u32;
            }
            file_name = Some(actual);
        }

        if moved / rec_size != records {
            err = FEOF;
        }
        let mut covered = moved;
        let partial = moved % rec_size;
        if partial != 0 && !write {
            err = FTRIM | FEOF;
            for i in moved..moved - partial + rec_size {
                mem.wr8(dta + i, 0);
            }
            covered = moved - partial + rec_size;
        }
        if write {
            let now = LocalTime::now();
            fcb.wr16(mem, DATE, now.dos_date());
            fcb.wr16(mem, TIME, now.dos_time());
        }
        if let (true, Some(name)) = (self.log_io, file_name) {
            self.io_log.push(FileIo { file: host::display(&name), offset: pos as u64, len: covered, write, addr: dta });
        }
        Ok(err)
    }
}
