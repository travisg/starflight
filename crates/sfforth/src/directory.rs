// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! The file directory at the start of STARA.COM (and repeated in STARB.COM).
//!
//! Four 1 KiB blocks of 48 entries of 21 bytes (the last 16 bytes of each
//! block are unused): name[12], file number, start and end paragraph, record
//! count, record size, and one more byte. An entry's position in the table is
//! its FILE# in the game. Field layout: s-macke extract.c `LoadDir`; the block
//! layout was also worked out in this repo author's 2002 `scanstarflt.cpp`.
//!
//! Start and end paragraphs address STARA.COM followed by STARB.COM as one
//! space: offsets from 256000 (STARA's size) on are in STARB (s-macke
//! extract.c `Extract`).

pub const BLOCKS: usize = 4;
pub const ENTRIES_PER_BLOCK: usize = 48;
pub const ENTRY_SIZE: usize = 21;
pub const BLOCK_SIZE: usize = 1024;
/// Size of STARA.COM; directory offsets past it fall in STARB.COM.
pub const STARA_SIZE: u32 = 256_000;

#[derive(Clone, Debug)]
pub struct DirEntry {
    /// FILE#: position in the directory.
    pub index: usize,
    pub name: String,
    /// 0 for entries with no stored data; otherwise 1-4, meaning not yet
    /// known. Which file holds the data follows from the start offset.
    pub fileno: u8,
    pub start_para: u16,
    pub end_para: u16,
    pub nrec: u16,
    pub reclen: u8,
    pub lsize: u8,
}

impl DirEntry {
    /// Offset in the combined STARA+STARB space.
    pub fn offset(&self) -> u32 {
        self.start_para as u32 * 16
    }

    /// The file holding the entry (1 = STARA.COM, 2 = STARB.COM) and the
    /// offset within it.
    pub fn location(&self) -> (u8, u32) {
        let o = self.offset();
        if o >= STARA_SIZE { (2, o - STARA_SIZE) } else { (1, o) }
    }

    /// Bytes reserved in the file, end paragraph inclusive.
    pub fn span(&self) -> u32 {
        if self.fileno == 0 || self.end_para < self.start_para {
            return 0;
        }
        (self.end_para as u32 - self.start_para as u32 + 1) * 16
    }

    /// Whether byte `offset` of file `file` (1 = STARA, 2 = STARB) is in this entry.
    pub fn contains(&self, file: u8, offset: u32) -> bool {
        let (f, start) = self.location();
        self.span() > 0 && f == file && offset >= start && offset < start + self.span()
    }
}

/// Record `rec` of a table entry, from the contents of STARA.COM and
/// STARB.COM. Records never cross a 1 KiB block boundary; the first block may
/// be partial (s-macke extract.c `ExtractRecord`, after the game's RECADD).
pub fn record<'a>(stara: &'a [u8], starb: &'a [u8], e: &DirEntry, rec: u16) -> Option<&'a [u8]> {
    let size = e.reclen as usize;
    if size == 0 || rec >= e.nrec {
        return None;
    }
    let (file, start) = e.location();
    let data = if file == 1 { stara } else { starb };
    let (block, within) = ((start as usize) & !0x3ff, (start as usize) & 0x3ff);
    let first = (BLOCK_SIZE - within) / size;
    let rec = rec as usize;
    let at = if rec < first {
        block + within + rec * size
    } else {
        let per = BLOCK_SIZE / size;
        let r = rec - first;
        block + BLOCK_SIZE + (r / per) * BLOCK_SIZE + (r % per) * size
    };
    data.get(at..at + size)
}

/// Parses the directory from the first 4 KiB of STARA.COM or STARB.COM.
/// Entries with a blank name are skipped but keep their FILE# numbering.
pub fn parse(data: &[u8]) -> Vec<DirEntry> {
    let mut out = Vec::new();
    for blk in 0..BLOCKS {
        for j in 0..ENTRIES_PER_BLOCK {
            let i = blk * BLOCK_SIZE + j * ENTRY_SIZE;
            let Some(e) = data.get(i..i + ENTRY_SIZE) else {
                return out;
            };
            let name: String = e[..12].iter().map(|&b| b as char).collect();
            let name = name.trim_end_matches([' ', '\0']).to_string();
            if name.is_empty() {
                continue;
            }
            out.push(DirEntry {
                index: blk * ENTRIES_PER_BLOCK + j,
                name,
                fileno: e[12],
                start_para: u16::from_le_bytes([e[13], e[14]]),
                end_para: u16::from_le_bytes([e[15], e[16]]),
                nrec: u16::from_le_bytes([e[17], e[18]]),
                reclen: e[19],
                lsize: e[20],
            });
        }
    }
    out
}
