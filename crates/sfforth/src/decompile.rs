// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Lists a colon definition's threaded code, one cell (or operand group) per
//! line. Branch offsets are relative to the offset cell itself (BRANCH does
//! `add si,[si]`).

use crate::consts::{DOCOL, cfa};
use crate::rd16;
use crate::trace::Names;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// Address of the first cell.
    pub addr: u16,
    pub text: String,
}

/// Decompiles the colon definition at `word_cfa`, stopping at an EXIT that no
/// earlier branch jumps past, or after `max_cells`.
pub fn decompile(seg: &[u8], word_cfa: u16, names: &Names, max_cells: usize) -> Option<Vec<Line>> {
    if rd16(seg, word_cfa) != DOCOL {
        return None;
    }
    let mut out = Vec::new();
    let mut ip = word_cfa.wrapping_add(2);
    let mut furthest = ip;
    for _ in 0..max_cells {
        let addr = ip;
        let w = rd16(seg, ip);
        ip = ip.wrapping_add(2);
        let text = match w {
            cfa::LIT => {
                let n = rd16(seg, ip);
                ip = ip.wrapping_add(2);
                format!("LIT {} ({n:#06X})", n as i16)
            }
            cfa::TWOLIT => {
                let (a, b) = (rd16(seg, ip), rd16(seg, ip.wrapping_add(2)));
                ip = ip.wrapping_add(4);
                format!("2LIT {a:#06X} {b:#06X}")
            }
            cfa::BRANCH | cfa::ZBRANCH | cfa::LOOP | cfa::PLUSLOOP | cfa::SLASHLOOP => {
                let target = ip.wrapping_add(rd16(seg, ip));
                ip = ip.wrapping_add(2);
                if target > furthest && target < ip.wrapping_add(0x2000) {
                    furthest = target;
                }
                format!("{} -> {target:04X}", names.label(w))
            }
            cfa::DOTQUOTE => {
                let len = seg[ip as usize] as u16;
                let s: String = (0..len).map(|i| seg[ip.wrapping_add(1 + i) as usize] as char).collect();
                ip = ip.wrapping_add(1 + len);
                format!(".\" {}\"", s.escape_debug())
            }
            _ => names.label(w),
        };
        out.push(Line { addr, text });
        if w == cfa::EXIT && ip > furthest {
            break;
        }
    }
    Some(out)
}
