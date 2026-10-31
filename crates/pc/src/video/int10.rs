// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! INT 10h, the EGA BIOS video services.
//!
//! Mode, active page, page start and cursor positions live in the BDA, as
//! with the real BIOS. Character and pixel output goes straight to display
//! memory, leaving the graphics controller registers and latches alone.
//! Graphics-mode characters use the 8x8 font; colour bit 7 XORs them in.

use super::ega::PLANE_SIZE;
use super::modes::{self, Kind, ModeParams};
use super::{
    ATTR_MODE, ATTR_OVERSCAN, CGA_MEM_SIZE, CRTC_CURSOR_END, CRTC_CURSOR_HI, CRTC_CURSOR_LO, CRTC_CURSOR_START,
    CRTC_START_HI, CRTC_START_LO, SWITCHES, Video, font,
};
use crate::hle::{DISPLAY, Hle, bda, rd8, rd16, wr8, wr16};
use cpu8086::{AX, BX, CX, Cpu, DX, ES, phys};

const ROWS: usize = 25;
const CGA_ODD_BANK: usize = 0x2000;
/// Bytes per scanline in the interleaved B8000 graphics modes.
const CGA_STRIDE: usize = 80;

fn unsupported(cpu: &Cpu) -> Hle {
    Hle::Unsupported(format!("INT 10h AX={:04X} BX={:04X}", cpu.regs[AX], cpu.regs[BX]))
}

pub(super) fn int10(v: &mut Video, cpu: &mut Cpu, ram: &mut [u8]) -> Hle {
    let [al, ah] = cpu.regs[AX].to_le_bytes();
    let [bl, bh] = cpu.regs[BX].to_le_bytes();
    let [cl, ch] = cpu.regs[CX].to_le_bytes();
    let [dl, dh] = cpu.regs[DX].to_le_bytes();
    let mode = modes::params(rd8(ram, bda::VIDEO_MODE));
    match ah {
        0x00 => {
            if !Video::supports_mode(al) {
                return unsupported(cpu);
            }
            v.set_mode(al, ram);
        }
        0x01 => {
            wr16(ram, bda::CURSOR_TYPE, cpu.regs[CX]);
            v.crtc[CRTC_CURSOR_START] = ch;
            v.crtc[CRTC_CURSOR_END] = cl;
        }
        0x03 => {
            let (row, col) = cursor(ram, bh);
            cpu.regs[DX] = (row as u16) << 8 | col as u16;
            cpu.regs[CX] = rd16(ram, bda::CURSOR_TYPE);
        }
        0x0b => {
            let Some(p) = mode else { return unsupported(cpu) };
            let mut sel = rd8(ram, bda::CGA_PALETTE);
            match bh {
                0 => sel = (sel & 0xe0) | (bl & 0x1f),
                1 => sel = (sel & 0xdf) | (bl & 1) << 5,
                _ => return unsupported(cpu),
            }
            wr8(ram, bda::CGA_PALETTE, sel);
            set_cga_colours(v, p, sel, bh == 0);
        }
        0x0f => {
            let m = rd8(ram, bda::VIDEO_MODE) | (rd8(ram, bda::EGA_MISC) & 0x80);
            cpu.regs[AX] = (rd8(ram, bda::VIDEO_COLS) as u16) << 8 | m as u16;
            cpu.regs[BX] = (rd8(ram, bda::VIDEO_PAGE) as u16) << 8 | bl as u16;
        }
        0x10 => match al {
            0x00 if bl < 0x14 => v.write_attr(bl as usize, bh),
            0x01 => v.write_attr(ATTR_OVERSCAN, bh),
            0x02 => {
                let table = phys(cpu.sregs[ES], cpu.regs[DX]);
                if (0..17).any(|i| DISPLAY.contains(&((table + i) & 0xfffff))) {
                    return Hle::Unsupported(format!(
                        "INT 10h AX=1002h: palette table in display memory at {table:05X}"
                    ));
                }
                for i in 0..16 {
                    v.write_attr(i, rd8(ram, table + i as u32));
                }
                v.write_attr(ATTR_OVERSCAN, rd8(ram, table + 16));
            }
            0x03 if bl < 2 => {
                let blink = bl == 1;
                v.attr[ATTR_MODE] = (v.attr[ATTR_MODE] & !0x08) | if blink { 0x08 } else { 0 };
                let m = rd8(ram, bda::CGA_MODE_REG);
                wr8(ram, bda::CGA_MODE_REG, (m & !0x20) | if blink { 0x20 } else { 0 });
            }
            _ => return unsupported(cpu),
        },
        0x12 if bl == 0x10 => {
            // Colour, 256 KiB, no feature bits.
            cpu.regs[BX] = 0x0003;
            cpu.regs[CX] = SWITCHES as u16;
        }
        // VGA display combination: an EGA returns with AL unchanged.
        0x1a => {}
        0x02 | 0x05..=0x0a | 0x0c..=0x0e => {
            let Some(p) = mode else { return unsupported(cpu) };
            let page = bh & (p.pages - 1);
            match ah {
                0x02 => set_cursor(v, ram, p, page, dh, dl),
                0x05 => select_page(v, ram, p, al),
                0x06 | 0x07 => {
                    let active = rd8(ram, bda::VIDEO_PAGE);
                    let win = Window { top: ch, left: cl, bottom: dh, right: dl };
                    scroll(v, p, active, ah == 0x06, al, bh, win);
                }
                0x08 => {
                    let (row, col) = cursor(ram, page);
                    let (row, col) = (row as usize, col as usize);
                    cpu.regs[AX] = if p.kind == Kind::Text {
                        let a = text_addr(p, page, row * p.cols + col);
                        (v.cga_mem[(a + 1) % CGA_MEM_SIZE] as u16) << 8 | v.cga_mem[a] as u16
                    } else {
                        read_char(v, p, page, row, col) as u16
                    };
                }
                0x09 | 0x0a => {
                    let (row, col) = cursor(ram, page);
                    let first = row as usize * p.cols + col as usize;
                    let attr = (ah == 0x09).then_some(bl);
                    for cell in first..first + cpu.regs[CX] as usize {
                        if p.kind == Kind::Text {
                            put_text(v, p, page, cell, al, attr);
                        } else if cell < ROWS * p.cols {
                            draw_char(v, p, page, cell / p.cols, cell % p.cols, al, bl);
                        }
                    }
                }
                0x0c | 0x0d => {
                    if p.kind == Kind::Text {
                        return unsupported(cpu);
                    }
                    let (x, y) = (cpu.regs[CX] as usize, cpu.regs[DX] as usize);
                    if ah == 0x0c {
                        put_pixel(v, p, page, x, y, al);
                    } else {
                        let c = get_pixel(v, p, page, x, y);
                        cpu.regs[AX] = (ah as u16) << 8 | c as u16;
                    }
                }
                _ => teletype(v, ram, p, al, bl),
            }
        }
        _ => return unsupported(cpu),
    }
    Hle::Done
}

/// A character window, inclusive.
#[derive(Clone, Copy)]
struct Window {
    top: u8,
    left: u8,
    bottom: u8,
    right: u8,
}

fn cursor(ram: &[u8], page: u8) -> (u8, u8) {
    let w = rd16(ram, bda::CURSOR_POS + 2 * (page & 7) as u32);
    ((w >> 8) as u8, w as u8)
}

fn set_cursor(v: &mut Video, ram: &mut [u8], p: &ModeParams, page: u8, row: u8, col: u8) {
    wr16(ram, bda::CURSOR_POS + 2 * page as u32, (row as u16) << 8 | col as u16);
    if page == rd8(ram, bda::VIDEO_PAGE) {
        sync_cursor(v, ram, p);
    }
}

/// Points the CRTC cursor at the active page's cursor position.
fn sync_cursor(v: &mut Video, ram: &[u8], p: &ModeParams) {
    if p.kind != Kind::Text {
        return;
    }
    let (row, col) = cursor(ram, rd8(ram, bda::VIDEO_PAGE));
    let a = rd16(ram, bda::VIDEO_PAGE_START) / 2 + (row as u16 * p.cols as u16) + col as u16;
    v.crtc[CRTC_CURSOR_HI] = (a >> 8) as u8;
    v.crtc[CRTC_CURSOR_LO] = a as u8;
}

fn select_page(v: &mut Video, ram: &mut [u8], p: &ModeParams, page: u8) {
    if p.pages == 1 {
        return;
    }
    let page = page & (p.pages - 1);
    let start = page as u16 * p.page_size;
    wr8(ram, bda::VIDEO_PAGE, page);
    wr16(ram, bda::VIDEO_PAGE_START, start);
    // Text modes address the CRTC in words, the planar modes in bytes.
    let crtc_start = if p.kind == Kind::Text { start / 2 } else { start };
    v.crtc[CRTC_START_HI] = (crtc_start >> 8) as u8;
    v.crtc[CRTC_START_LO] = crtc_start as u8;
    sync_cursor(v, ram, p);
}

/// Buffer address of text cell `cell` (row * cols + col) of `page`.
fn text_addr(p: &ModeParams, page: u8, cell: usize) -> usize {
    (page as usize * p.page_size as usize + cell * 2) % CGA_MEM_SIZE
}

fn put_text(v: &mut Video, p: &ModeParams, page: u8, cell: usize, ch: u8, attr: Option<u8>) {
    let a = text_addr(p, page, cell);
    v.cga_mem[a] = ch;
    if let Some(at) = attr {
        v.cga_mem[(a + 1) % CGA_MEM_SIZE] = at;
    }
}

/// Offset of graphics scanline `y` of `page`: within a plane for the planar
/// modes, within the B8000 buffer otherwise.
fn gfx_line(p: &ModeParams, page: u8, y: usize) -> usize {
    match p.kind {
        Kind::Planar => (page as usize * p.page_size as usize + y * p.cols) % PLANE_SIZE,
        _ => (y & 1) * CGA_ODD_BANK + (y >> 1) * CGA_STRIDE,
    }
}

/// Bytes per character cell per scanline.
fn cell_bytes(p: &ModeParams) -> usize {
    if p.kind == Kind::Cga2 { 2 } else { 1 }
}

/// Scrolls a window of the active page by `n` rows (0 = clear), filling with
/// attribute or colour `fill`.
fn scroll(v: &mut Video, p: &ModeParams, page: u8, up: bool, n: u8, fill: u8, w: Window) {
    let bottom = (w.bottom as usize).min(ROWS - 1);
    let right = (w.right as usize).min(p.cols - 1);
    let (top, left) = (w.top as usize, w.left as usize);
    if top > bottom || left > right {
        return;
    }
    let height = bottom - top + 1;
    let n = if n == 0 || n as usize > height { height } else { n as usize };
    let rows: Vec<usize> = if up { (top..=bottom).collect() } else { (top..=bottom).rev().collect() };
    for row in rows {
        let src = if up { row + n } else { row.wrapping_sub(n) };
        let from = (src >= top && src <= bottom).then_some(src);
        scroll_row(v, p, page, row, from, left, right, fill);
    }
}

/// Copies character row `from` to `row` between columns `left..=right`, or
/// fills it when `from` is `None`.
#[allow(clippy::too_many_arguments)]
fn scroll_row(
    v: &mut Video,
    p: &ModeParams,
    page: u8,
    row: usize,
    from: Option<usize>,
    left: usize,
    right: usize,
    fill: u8,
) {
    if p.kind == Kind::Text {
        for col in left..=right {
            let dst = text_addr(p, page, row * p.cols + col);
            let (ch, at) = match from {
                Some(src) => {
                    let s = text_addr(p, page, src * p.cols + col);
                    (v.cga_mem[s], v.cga_mem[(s + 1) % CGA_MEM_SIZE])
                }
                None => (b' ', fill),
            };
            v.cga_mem[dst] = ch;
            v.cga_mem[(dst + 1) % CGA_MEM_SIZE] = at;
        }
        return;
    }
    let cb = cell_bytes(p);
    for line in 0..8 {
        let dst = gfx_line(p, page, row * 8 + line);
        let src = from.map(|s| gfx_line(p, page, s * 8 + line));
        for x in left * cb..(right + 1) * cb {
            if p.kind == Kind::Planar {
                for plane in 0..4 {
                    let b = match src {
                        Some(s) => v.planes.get(plane, s + x),
                        None if fill & (1 << plane) != 0 => 0xff,
                        None => 0,
                    };
                    v.planes.set(plane, dst + x, b);
                }
            } else {
                let b = match src {
                    Some(s) => v.cga_mem[(s + x) % CGA_MEM_SIZE],
                    None => fill,
                };
                v.cga_mem[(dst + x) % CGA_MEM_SIZE] = b;
            }
        }
    }
}

/// Draws a character in a graphics mode: foreground `colour`, background 0,
/// or XORed in when bit 7 is set. Mode 6 ignores the colour.
fn draw_char(v: &mut Video, p: &ModeParams, page: u8, row: usize, col: usize, ch: u8, colour: u8) {
    let xor = colour & 0x80 != 0;
    let g = font::glyph(ch);
    for (line, &bits) in g.iter().enumerate() {
        let a = gfx_line(p, page, row * 8 + line) + col * cell_bytes(p);
        match p.kind {
            Kind::Planar => {
                for plane in 0..4 {
                    let d = if colour & (1 << plane) != 0 { bits } else { 0 };
                    let old = v.planes.get(plane, a);
                    v.planes.set(plane, a, if xor { old ^ d } else { d });
                }
            }
            Kind::Cga2 => {
                let c = (colour & 3) as u16;
                let mut w = 0u16;
                for i in 0..8 {
                    if bits & (0x80 >> i) != 0 {
                        w |= c << (14 - 2 * i);
                    }
                }
                for (k, b) in w.to_be_bytes().into_iter().enumerate() {
                    let m = &mut v.cga_mem[(a + k) % CGA_MEM_SIZE];
                    *m = if xor { *m ^ b } else { b };
                }
            }
            Kind::Cga1 => {
                let m = &mut v.cga_mem[a % CGA_MEM_SIZE];
                *m = if xor { *m ^ bits } else { bits };
            }
            Kind::Text => unreachable!(),
        }
    }
}

/// The character drawn at a graphics-mode cell (any non-zero pixel is
/// foreground), or 0 if it matches none.
fn read_char(v: &Video, p: &ModeParams, page: u8, row: usize, col: usize) -> u8 {
    let mut rows = [0u8; 8];
    for (line, r) in rows.iter_mut().enumerate() {
        let a = gfx_line(p, page, row * 8 + line) + col * cell_bytes(p);
        *r = match p.kind {
            Kind::Planar => (0..4).fold(0, |acc, plane| acc | v.planes.get(plane, a)),
            Kind::Cga2 => {
                let w = u16::from_be_bytes([v.cga_mem[a % CGA_MEM_SIZE], v.cga_mem[(a + 1) % CGA_MEM_SIZE]]);
                (0..8).fold(0, |acc, i| if (w >> (14 - 2 * i)) & 3 != 0 { acc | 0x80 >> i } else { acc })
            }
            _ => v.cga_mem[a % CGA_MEM_SIZE],
        };
    }
    font::find(&rows).unwrap_or(0)
}

/// Buffer offset, shift and width mask of pixel (x, y) in a graphics mode.
fn pixel_at(p: &ModeParams, page: u8, x: usize, y: usize) -> Option<(usize, u32, u8)> {
    if x >= p.width || y >= ROWS * 8 {
        return None;
    }
    let line = gfx_line(p, page, y);
    Some(match p.kind {
        Kind::Cga2 => (line + x / 4, (6 - 2 * (x & 3)) as u32, 3),
        _ => (line + x / 8, (7 - (x & 7)) as u32, 1),
    })
}

fn put_pixel(v: &mut Video, p: &ModeParams, page: u8, x: usize, y: usize, colour: u8) {
    let Some((a, shift, mask)) = pixel_at(p, page, x, y) else { return };
    let xor = colour & 0x80 != 0;
    let planes = if p.kind == Kind::Planar { 4 } else { 1 };
    for plane in 0..planes {
        let c = if p.kind == Kind::Planar { (colour >> plane) & 1 } else { colour & mask };
        let old = if p.kind == Kind::Planar { v.planes.get(plane, a) } else { v.cga_mem[a % CGA_MEM_SIZE] };
        let new = if xor { old ^ (c << shift) } else { (old & !(mask << shift)) | (c << shift) };
        if p.kind == Kind::Planar {
            v.planes.set(plane, a, new);
        } else {
            v.cga_mem[a % CGA_MEM_SIZE] = new;
        }
    }
}

fn get_pixel(v: &Video, p: &ModeParams, page: u8, x: usize, y: usize) -> u8 {
    let Some((a, shift, mask)) = pixel_at(p, page, x, y) else { return 0 };
    if p.kind == Kind::Planar {
        (0..4).fold(0, |acc, plane| acc | ((v.planes.get(plane, a) >> shift) & 1) << plane)
    } else {
        (v.cga_mem[a % CGA_MEM_SIZE] >> shift) & mask
    }
}

/// Teletype output on the active page. BEL is ignored: the BIOS has no
/// speaker here.
fn teletype(v: &mut Video, ram: &mut [u8], p: &ModeParams, ch: u8, colour: u8) {
    let page = rd8(ram, bda::VIDEO_PAGE);
    let (row, col) = cursor(ram, page);
    let (mut row, mut col) = (row as usize, col as usize);
    match ch {
        0x07 => return,
        0x08 => col = col.saturating_sub(1),
        0x0a => row += 1,
        0x0d => col = 0,
        _ => {
            if p.kind == Kind::Text {
                put_text(v, p, page, row * p.cols + col, ch, None);
            } else {
                draw_char(v, p, page, row, col, ch, colour);
            }
            col += 1;
            if col >= p.cols {
                col = 0;
                row += 1;
            }
        }
    }
    if row >= ROWS {
        row = ROWS - 1;
        // Text scrolls in the attribute found at the cursor; graphics in 0.
        let fill = if p.kind == Kind::Text {
            v.cga_mem[(text_addr(p, page, row * p.cols + col) + 1) % CGA_MEM_SIZE]
        } else {
            0
        };
        let win = Window { top: 0, left: 0, bottom: (ROWS - 1) as u8, right: (p.cols - 1) as u8 };
        scroll(v, p, page, true, 1, fill, win);
    }
    set_cursor(v, ram, p, page, row as u8, col as u8);
}

/// INT 10h AH=0Bh: BH=0 sets the background/border from BL (bit 4 brightens
/// the 4-colour palette), BH=1 picks the 4-colour palette from BL bit 0.
/// Graphics modes also get palette 0 and 1-3, as DOSBox does
/// (INT10_SetBackgroundBorder/INT10_SetColorSelect); text modes only the
/// border.
fn set_cga_colours(v: &mut Video, p: &ModeParams, sel: u8, background: bool) {
    let border = (sel & 7) | (sel & 8) << 1;
    if background {
        v.write_attr(ATTR_OVERSCAN, border);
    }
    if p.kind == Kind::Text {
        return;
    }
    if background {
        v.write_attr(0, border);
    }
    let base = (sel & 0x10) | 2 | (sel >> 5) & 1;
    for i in 0..3 {
        v.write_attr(1 + i, base + 2 * i as u8);
    }
}
