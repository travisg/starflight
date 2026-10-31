// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! What the monitor shows, read from the adapter's registers.
//!
//! The layout comes from the registers rather than the BIOS mode number:
//! attribute mode control bit 0 picks text or graphics, the memory map picks
//! planes (A0000/B0000) or the B8000 buffer, GC mode bit 5 picks 2 bits per
//! pixel there, and sequencer clocking bit 3 halves the width. Lines are
//! CRTC offset x 2 bytes apart (x 2 again in word mode); the start address
//! is the one latched at the last vertical retrace. Every layout is 200
//! lines; text uses 8x8 cells.

use super::ega::{GC_MISC, GC_MODE};
use super::{
    ATTR_MODE, ATTR_PAN, ATTR_PAS, ATTR_PLANE_ENABLE, CGA_MEM_SIZE, CRTC_CURSOR_END, CRTC_CURSOR_HI, CRTC_CURSOR_LO,
    CRTC_CURSOR_START, CRTC_MODE, CRTC_OFFSET, Frame, Video, font,
};

const LINES: usize = 200;
const TEXT_ROWS: usize = 25;
/// The interleaved B8000 layouts put odd lines 8 KiB after even ones.
const CGA_ODD_BANK: usize = 0x2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Layout {
    Text { cols: usize },
    Cga2,
    Cga1 { width: usize },
    Planar { width: usize },
}

/// The standard 16 RGBI colours, colour 6 as brown.
#[rustfmt::skip]
const RGBI: [u32; 16] = [
    0x000000, 0x0000aa, 0x00aa00, 0x00aaaa, 0xaa0000, 0xaa00aa, 0xaa5500, 0xaaaaaa,
    0x555555, 0x5555ff, 0x55ff55, 0x55ffff, 0xff5555, 0xff55ff, 0xffff55, 0xffffff,
];

/// A palette value on a 200-line (CGA-compatible) monitor: bit 0 B, bit 1 G,
/// bit 2 R, bit 4 intensity; bits 3 and 5 are not wired.
pub(super) fn rgbi(v: u8) -> u32 {
    RGBI[((v & 7) | ((v >> 1) & 8)) as usize]
}

/// A palette value on the Enhanced Color Display in its 350-line modes:
/// bits 0-2 B G R at 2/3 strength, bits 3-5 b g r at 1/3.
pub(super) fn rgb6(v: u8) -> u32 {
    let c = |hi: u8, lo: u8| ((v >> hi) & 1) as u32 * 0xaa + ((v >> lo) & 1) as u32 * 0x55;
    c(2, 5) << 16 | c(1, 4) << 8 | c(0, 3)
}

impl Video {
    pub(super) fn layout(&self) -> Layout {
        let half = self.seq[1] & 0x08 != 0;
        let width = if half { 320 } else { 640 };
        if self.attr[ATTR_MODE] & 0x01 == 0 {
            Layout::Text { cols: width / 8 }
        } else if (self.gc[GC_MISC] >> 2) & 3 == 3 {
            if self.gc[GC_MODE] & 0x20 != 0 { Layout::Cga2 } else { Layout::Cga1 { width } }
        } else {
            Layout::Planar { width }
        }
    }

    fn word_mode(&self) -> bool {
        self.crtc[CRTC_MODE] & 0x40 == 0
    }

    /// Display start and line stride, in bytes of the addressed buffer.
    fn scan_geometry(&self) -> (usize, usize) {
        let unit = if self.word_mode() { 2 } else { 1 };
        (self.shown_start() as usize * unit, self.crtc[CRTC_OFFSET] as usize * 2 * unit)
    }

    /// The 16 palette entries as colours. Text modes load the ECD palette
    /// (colour 6 = 14h, 8-15 = 38h-3Fh), which only reads correctly as 6-bit
    /// colour; graphics modes load 00-07,10-17 for a 200-line monitor. With
    /// palette 00-07,10-17 in text modes too, `rgbi` would serve both.
    fn colours(&self, layout: Layout) -> [u32; 16] {
        let decode = if matches!(layout, Layout::Text { .. }) { rgb6 } else { rgbi };
        std::array::from_fn(|i| decode(self.attr[i]))
    }

    /// A text cell's buffer address, character and attribute.
    fn text_cell(&self, start: usize, stride: usize, row: usize, col: usize) -> (usize, u8, u8) {
        let a = (start + row * stride + col * 2) % CGA_MEM_SIZE;
        (a, self.cga_mem[a], self.cga_mem[(a + 1) % CGA_MEM_SIZE])
    }
}

pub(super) fn render(v: &Video, frame: &mut Frame) {
    let layout = v.layout();
    let width = match layout {
        Layout::Text { cols } => cols * 8,
        Layout::Cga2 => 320,
        Layout::Cga1 { width } | Layout::Planar { width } => width,
    };
    frame.width = width;
    frame.height = LINES;
    frame.pixels.clear();
    frame.pixels.resize(width * LINES, 0);
    // With the palette address source clear the screen is blanked.
    if v.attr_index & ATTR_PAS == 0 {
        return;
    }
    let pal = v.colours(layout);
    let enable = v.attr[ATTR_PLANE_ENABLE] & 0x0f;
    let (start, stride) = v.scan_geometry();
    let px = &mut frame.pixels;
    match layout {
        Layout::Text { cols } => {
            let blink = v.attr[ATTR_MODE] & 0x08 != 0;
            let cursor = text_cursor(v);
            for row in 0..TEXT_ROWS {
                for col in 0..cols {
                    let (a, ch, at) = v.text_cell(start, stride, row, col);
                    let fg = pal[(at & enable & 0x0f) as usize];
                    let bg_index = if blink { (at >> 4) & 7 } else { at >> 4 };
                    let bg = pal[(bg_index & enable) as usize];
                    let g = font::glyph(ch);
                    for (line, &bits) in g.iter().enumerate() {
                        let bits = match cursor {
                            Some((ca, ref lines)) if ca == a && lines.contains(&line) => 0xff,
                            _ => bits,
                        };
                        let o = (row * 8 + line) * width + col * 8;
                        for x in 0..8 {
                            px[o + x] = if bits & (0x80 >> x) != 0 { fg } else { bg };
                        }
                    }
                }
            }
        }
        Layout::Cga2 | Layout::Cga1 { .. } => {
            let bpp = if layout == Layout::Cga2 { 2 } else { 1 };
            let per_byte = 8 / bpp;
            let pixel_mask = (1u8 << bpp) - 1;
            for y in 0..LINES {
                let bank = (y & 1) * CGA_ODD_BANK;
                let line = (start + (y >> 1) * stride) % CGA_ODD_BANK;
                for x in 0..width {
                    let b = v.cga_mem[bank + (line + x / per_byte) % CGA_ODD_BANK];
                    let shift = 8 - bpp * (x % per_byte + 1);
                    let index = (b >> shift) & pixel_mask & enable;
                    px[y * width + x] = pal[index as usize];
                }
            }
        }
        Layout::Planar { .. } => {
            let pan = (v.attr[ATTR_PAN] & 7) as usize;
            for y in 0..LINES {
                let line = start + y * stride;
                for x in 0..width {
                    let dot = x + pan;
                    let off = line + dot / 8;
                    let bit = 7 - (dot & 7);
                    let mut index = 0;
                    for p in 0..4 {
                        index |= ((v.planes.get(p, off) >> bit) & 1) << p;
                    }
                    px[y * width + x] = pal[(index & enable) as usize];
                }
            }
        }
    }
}

/// The text cursor's buffer address and lit glyph lines, if it is shown.
fn text_cursor(v: &Video) -> Option<(usize, std::ops::RangeInclusive<usize>)> {
    let first = v.crtc[CRTC_CURSOR_START];
    let last = (v.crtc[CRTC_CURSOR_END] & 0x1f) as usize;
    if first & 0x20 != 0 || (first as usize & 0x1f) > last {
        return None;
    }
    let addr = ((v.crtc[CRTC_CURSOR_HI] as usize) << 8 | v.crtc[CRTC_CURSOR_LO] as usize) * 2;
    Some((addr % CGA_MEM_SIZE, (first & 0x1f) as usize..=last))
}

pub(super) fn text_lines(v: &Video) -> Option<Vec<String>> {
    let Layout::Text { cols } = v.layout() else { return None };
    let (start, stride) = v.scan_geometry();
    let lines = (0..TEXT_ROWS)
        .map(|row| {
            let s: String = (0..cols).map(|col| font::CP437[v.text_cell(start, stride, row, col).1 as usize]).collect();
            s.trim_end().to_string()
        })
        .collect();
    Some(lines)
}
