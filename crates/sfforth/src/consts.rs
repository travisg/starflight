// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Fixed addresses in Starflight 1's STARFLT.COM (the 54183-byte build dated
//! 1988-01-04). Most come from s-macke/starflight-reverse
//! `src/disasOV/global.h`; all were checked against the binary.

/// Size of the STARFLT.COM these addresses belong to.
pub const STARFLT_COM_SIZE: usize = 54183;

/// Initial Forth IP, loaded into SI by the boot code at 0x100.
pub const BOOT_IP: u16 = 0x129;

/// Code address of the inner interpreter's shared NEXT: `lodsw; mov bx,ax; jmp [bx]`.
pub const NEXT: u16 = 0x124;

/// Code field contents that identify the common word types (s-macke CODECALL etc.).
pub const DOCOL: u16 = 0x224C;
pub const DOVAR: u16 = 0x1D29;
pub const DOCON: u16 = 0x2214;

/// Code addresses (code-field contents) of threaded-code primitives.
pub const EXIT: u16 = 0x1692;
pub const LIT: u16 = 0x175F;
pub const TWOLIT: u16 = 0x1618;
pub const BRANCH: u16 = 0x1662;
pub const ZBRANCH: u16 = 0x15FC;

/// DI always holds the address of the word OPERATOR (s-macke REGDI).
pub const OPERATOR: u16 = 0x78C;

/// Newest header of each vocabulary chain in STARFLT.COM (s-macke DICTLIST1-4).
pub const VOCAB_HEADS: [u16; 4] = [0xD40F, 0xD480, 0xD449, 0xD46D];

/// CFAs of words compiled with inline operands, as the decompiler meets them
/// in threaded code. Names are from the dictionary except `(.")`, which is
/// headerless (s-macke README, "Translation" table).
pub mod cfa {
    pub const LIT: u16 = 0x175D;
    pub const TWOLIT: u16 = 0x1616;
    pub const BRANCH: u16 = 0x1660;
    pub const ZBRANCH: u16 = 0x15FA;
    pub const LOOP: u16 = 0x15D0;
    pub const PLUSLOOP: u16 = 0x155C;
    pub const SLASHLOOP: u16 = 0x1591;
    pub const EXIT: u16 = 0x1690;
    pub const DOTQUOTE: u16 = 0x1BDC;
}
