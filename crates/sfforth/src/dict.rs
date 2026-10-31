// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Dictionary headers.
//!
//! Header layout: `[link][NFA][name][CFA]`. The link (2 bytes before the NFA)
//! points at the previous header's NFA, 0 ends the chain. The NFA byte holds
//! 0x80 | flags | length (low 5 bits). Name characters are stored XOR 0x7F, with
//! bit 7 set on the last stored character; a header may store fewer characters
//! than its declared length. One-character names are stored plain, as is the
//! FIG-style null word (length 0, one NUL character, immediate) at NFA 0x253A.
//! (Name encoding: s-macke patch.c `DecryptDictionary`.)

use crate::consts::{DOCOL, DOCON, DOVAR, VOCAB_HEADS};
use crate::rd16;
use std::collections::HashSet;

/// Where a word's name came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// Decoded from the header in the game binary.
    Binary,
}

impl Provenance {
    pub fn label(self) -> &'static str {
        match self {
            Provenance::Binary => "binary",
        }
    }
}

/// What a word's code field points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Machine code word: the code field points just past itself.
    Code,
    Colon,
    Variable,
    Constant,
    /// Defined by some other defining word; holds its code address.
    Does(u16),
}

impl Kind {
    pub fn classify(seg: &[u8], cfa: u16) -> Kind {
        match rd16(seg, cfa) {
            cf if cf == cfa.wrapping_add(2) => Kind::Code,
            DOCOL => Kind::Colon,
            DOVAR => Kind::Variable,
            DOCON => Kind::Constant,
            other => Kind::Does(other),
        }
    }

    pub fn label(self) -> String {
        match self {
            Kind::Code => "CODE".into(),
            Kind::Colon => "COLON".into(),
            Kind::Variable => "VAR".into(),
            Kind::Constant => "CONST".into(),
            Kind::Does(a) => format!("DOES:{a:04X}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Word {
    pub name: String,
    /// Length from the NFA byte; longer than `name` when the header is truncated.
    pub declared_len: u8,
    pub flags: u8,
    pub nfa: u16,
    pub cfa: u16,
    pub kind: Kind,
    /// Index into `VOCAB_HEADS` of the chain the header was found on.
    pub vocab: usize,
    pub provenance: Provenance,
}

impl Word {
    /// Precedence bit (FIG-Forth style).
    pub fn immediate(&self) -> bool {
        self.flags & 0x40 != 0
    }

    pub fn truncated(&self) -> bool {
        self.name.len() < self.declared_len as usize
    }

    /// The name with control characters made visible (the null word is `\0`).
    pub fn display_name(&self) -> String {
        self.name.escape_debug().to_string()
    }
}

/// Decodes the header at `nfa`, returning the name, declared length, flags and CFA.
pub fn decode_header(seg: &[u8], nfa: u16) -> Option<(String, u8, u8, u16)> {
    let nfa_byte = seg[nfa as usize];
    let len = nfa_byte & 0x1f;
    let mut name = String::new();
    let mut p = nfa.wrapping_add(1);
    loop {
        let c = seg[p as usize];
        let ch = if len <= 1 { c & 0x7f } else { (c ^ 0x7f) & 0x7f };
        name.push(ch as char);
        p = p.wrapping_add(1);
        if c & 0x80 != 0 {
            break;
        }
        if name.len() >= 32 {
            return None;
        }
    }
    Some((name, len, nfa_byte & 0xe0, p))
}

/// Walks every vocabulary chain in a STARFLT.COM segment image, newest first.
/// A word reachable from several chains is reported once.
pub fn decode_dictionary(seg: &[u8]) -> Vec<Word> {
    let mut words = Vec::new();
    let mut seen = HashSet::new();
    for (vocab, &head) in VOCAB_HEADS.iter().enumerate() {
        let mut nfa = head;
        while nfa != 0 && seen.insert(nfa) {
            let Some((name, declared_len, flags, cfa)) = decode_header(seg, nfa) else {
                break;
            };
            words.push(Word {
                name,
                declared_len,
                flags,
                nfa,
                cfa,
                kind: Kind::classify(seg, cfa),
                vocab,
                provenance: Provenance::Binary,
            });
            nfa = rd16(seg, nfa.wrapping_sub(2));
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(seg: &mut [u8], link: u16, nfa: u16, name: &str, code_field: u16) -> u16 {
        seg[nfa as usize - 2..nfa as usize].copy_from_slice(&link.to_le_bytes());
        let bytes = name.as_bytes();
        seg[nfa as usize] = 0x80 | bytes.len() as u8;
        for (i, &b) in bytes.iter().enumerate() {
            let mut c = if bytes.len() == 1 { b } else { b ^ 0x7f };
            if i == bytes.len() - 1 {
                c |= 0x80;
            }
            seg[nfa as usize + 1 + i] = c;
        }
        let cfa = nfa + 1 + bytes.len() as u16;
        seg[cfa as usize..cfa as usize + 2].copy_from_slice(&code_field.to_le_bytes());
        cfa
    }

    #[test]
    fn decodes_names_and_kinds() {
        let mut seg = vec![0u8; 0x10000];
        let a = header(&mut seg, 0, 0x1000, "(!SET)", 0);
        seg[a as usize..a as usize + 2].copy_from_slice(&(a + 2).to_le_bytes());
        header(&mut seg, 0x1000, 0x1100, "+", DOCOL);
        let (name, len, _, cfa) = decode_header(&seg, 0x1000).unwrap();
        assert_eq!((name.as_str(), len, cfa), ("(!SET)", 6, a));
        assert_eq!(Kind::classify(&seg, a), Kind::Code);
        let (name, _, _, cfa) = decode_header(&seg, 0x1100).unwrap();
        assert_eq!(name, "+");
        assert_eq!(Kind::classify(&seg, cfa), Kind::Colon);
    }

    #[test]
    fn decodes_null_word() {
        let mut seg = vec![0u8; 0x10000];
        seg[0x2000] = 0xc0;
        seg[0x2001] = 0x80;
        let (name, len, flags, cfa) = decode_header(&seg, 0x2000).unwrap();
        assert_eq!((name.as_str(), len, flags, cfa), ("\0", 0, 0xc0, 0x2002));
    }
}
