// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Following the game's Forth as it runs.
//!
//! Every word is entered through NEXT's `jmp [bx]` (FF 27) with BX = the
//! word's CFA, so watching for that instruction sees each word executed. The
//! return stack pointer (BP) gives the nesting depth.

use crate::dict::Word;
use std::collections::{HashMap, VecDeque};

/// Opcode bytes of `jmp word ptr [bx]`, the dispatch at the end of NEXT.
pub const JMP_BX: [u8; 2] = [0xff, 0x27];

/// CFA to name, for words whose names are known.
#[derive(Clone, Debug, Default)]
pub struct Names {
    by_cfa: HashMap<u16, String>,
}

impl Names {
    pub fn from_words(words: &[Word]) -> Self {
        let by_cfa = words.iter().map(|w| (w.cfa, w.display_name())).collect();
        Names { by_cfa }
    }

    pub fn get(&self, cfa: u16) -> Option<&str> {
        self.by_cfa.get(&cfa).map(|s| s.as_str())
    }

    /// The name, or `?XXXX` for a headerless word.
    pub fn label(&self, cfa: u16) -> String {
        match self.get(cfa) {
            Some(n) => n.to_string(),
            None => format!("?{cfa:04X}"),
        }
    }
}

/// One word entry seen by the tracer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub cfa: u16,
    /// Return stack pointer at entry; smaller means deeper.
    pub rp: u16,
}

/// The most recent word entries, for reports when the machine stops.
#[derive(Clone, Debug)]
pub struct Recent {
    buf: VecDeque<Entry>,
    cap: usize,
}

impl Recent {
    pub fn new(cap: usize) -> Self {
        Recent { buf: VecDeque::with_capacity(cap), cap }
    }

    pub fn push(&mut self, e: Entry) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(e);
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.buf.iter()
    }

    /// Formats the entries oldest first, indented by return-stack depth.
    pub fn format(&self, names: &Names) -> String {
        let top = self.buf.iter().map(|e| e.rp).max().unwrap_or(0);
        let mut out = String::new();
        for e in &self.buf {
            let depth = (top.wrapping_sub(e.rp) / 2).min(40) as usize;
            out.push_str(&format!("{:04X} {:width$}{}\n", e.cfa, "", names.label(e.cfa), width = depth * 2));
        }
        out
    }
}
