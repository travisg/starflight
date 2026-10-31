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

pub const BLOCKS: usize = 4;
pub const ENTRIES_PER_BLOCK: usize = 48;
pub const ENTRY_SIZE: usize = 21;
pub const BLOCK_SIZE: usize = 1024;

#[derive(Clone, Debug)]
pub struct DirEntry {
    /// FILE#: position in the directory.
    pub index: usize,
    pub name: String,
    /// 1 = STARA.COM, 2 = STARB.COM, 0 = no stored data. 4 is seen on PHAZES
    /// and SAVE (in STARA per s-macke) and 3 on VES-BLT, whose start lies past
    /// the end of both files; their meaning is not yet known.
    pub fileno: u8,
    pub start_para: u16,
    pub end_para: u16,
    pub nrec: u16,
    pub reclen: u8,
    pub lsize: u8,
}

impl DirEntry {
    pub fn offset(&self) -> u32 {
        self.start_para as u32 * 16
    }

    /// Bytes reserved in the file, end paragraph inclusive.
    pub fn span(&self) -> u32 {
        if self.fileno == 0 || self.end_para < self.start_para {
            return 0;
        }
        (self.end_para as u32 - self.start_para as u32 + 1) * 16
    }

    pub fn contains(&self, fileno: u8, offset: u32) -> bool {
        self.fileno == fileno && offset >= self.offset() && offset < self.offset() + self.span()
    }
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
