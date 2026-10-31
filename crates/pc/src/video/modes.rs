// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Register settings for the BIOS video modes.
//!
//! Values follow the IBM EGA BIOS video parameter tables for a 200-line
//! display (reconstructed, not copied), except the text palette: modes 0-3
//! load the Enhanced Color Display's 6-bit palette (see `render`).

/// How a mode lays out display memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Character/attribute pairs in the linear B8000 buffer.
    Text,
    /// 2 bits per pixel, B8000 interleaved (even lines at 0000, odd at 2000).
    Cga2,
    /// 1 bit per pixel, interleaved like `Cga2`.
    Cga1,
    /// Four planes at A0000, 1 bit per pixel per plane.
    Planar,
}

pub struct ModeParams {
    pub kind: Kind,
    /// Character columns (25 rows in every mode).
    pub cols: usize,
    pub width: usize,
    pub page_size: u16,
    pub pages: u8,
    /// BDA 0x465 and 0x466, the CGA mode and colour registers' shadows.
    pub cga_mode: u8,
    pub cga_palette: u8,
    /// Sequencer registers 1-4.
    pub seq: [u8; 4],
    pub misc: u8,
    pub crtc: [u8; 25],
    pub attr: [u8; 20],
    pub gc: [u8; 9],
}

#[rustfmt::skip]
const TEXT_PALETTE: [u8; 16] =
    [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x14, 0x07, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f];
#[rustfmt::skip]
const PALETTE16: [u8; 16] =
    [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17];

const fn attr(pal: [u8; 16], mode: u8, plane_enable: u8) -> [u8; 20] {
    let mut a = [0u8; 20];
    let mut i = 0;
    while i < 16 {
        a[i] = pal[i];
        i += 1;
    }
    a[0x10] = mode;
    a[0x11] = 0;
    a[0x12] = plane_enable;
    a[0x13] = 0;
    a
}

#[rustfmt::skip]
const CRTC_TEXT40: [u8; 25] = [
    0x37, 0x27, 0x2d, 0x37, 0x31, 0x15, 0x04, 0x11, 0x00, 0x07, 0x06, 0x07, 0x00, 0x00, 0x00, 0x00,
    0xe1, 0x24, 0xc7, 0x14, 0x08, 0xe0, 0xf0, 0xa3, 0xff,
];
#[rustfmt::skip]
const CRTC_TEXT80: [u8; 25] = [
    0x70, 0x4f, 0x5c, 0x2f, 0x5f, 0x07, 0x04, 0x11, 0x00, 0x07, 0x06, 0x07, 0x00, 0x00, 0x00, 0x00,
    0xe1, 0x24, 0xc7, 0x28, 0x08, 0xe0, 0xf0, 0xa3, 0xff,
];
#[rustfmt::skip]
const CRTC_320: [u8; 25] = [
    0x37, 0x27, 0x2d, 0x37, 0x30, 0x14, 0x04, 0x11, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xe1, 0x24, 0xc7, 0x14, 0x00, 0xe0, 0xf0, 0xa2, 0xff,
];
#[rustfmt::skip]
const CRTC_640: [u8; 25] = [
    0x70, 0x4f, 0x59, 0x2d, 0x5e, 0x06, 0x04, 0x11, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xe0, 0x23, 0xc7, 0x28, 0x00, 0xdf, 0xef, 0xc2, 0xff,
];

/// `crtc` with max scan line 0 and byte addressing, for the planar modes.
const fn planar_crtc(mut crtc: [u8; 25]) -> [u8; 25] {
    crtc[0x09] = 0x00;
    crtc[0x17] = 0xe3;
    crtc
}

const GC_TEXT: [u8; 9] = [0, 0, 0, 0, 0, 0x10, 0x0e, 0x00, 0xff];
const GC_CGA2: [u8; 9] = [0, 0, 0, 0, 0, 0x30, 0x0f, 0x00, 0xff];
const GC_CGA1: [u8; 9] = [0, 0, 0, 0, 0, 0x00, 0x0d, 0x00, 0xff];
const GC_PLANAR: [u8; 9] = [0, 0, 0, 0, 0, 0x00, 0x05, 0x0f, 0xff];

const fn text(cols: usize, cga_mode: u8) -> ModeParams {
    let wide = cols == 80;
    ModeParams {
        kind: Kind::Text,
        cols,
        width: cols * 8,
        page_size: if wide { 0x1000 } else { 0x0800 },
        pages: 8,
        cga_mode,
        cga_palette: 0x30,
        seq: [if wide { 0x01 } else { 0x0b }, 0x03, 0x00, 0x03],
        misc: 0x23,
        crtc: if wide { CRTC_TEXT80 } else { CRTC_TEXT40 },
        attr: attr(TEXT_PALETTE, 0x08, 0x0f),
        gc: GC_TEXT,
    }
}

#[rustfmt::skip]
const MODE_4: ModeParams = ModeParams {
    kind: Kind::Cga2, cols: 40, width: 320, page_size: 0x4000, pages: 1,
    cga_mode: 0x2a, cga_palette: 0x30,
    seq: [0x0b, 0x03, 0x00, 0x02], misc: 0x23, crtc: CRTC_320,
    attr: attr([0x00, 0x13, 0x15, 0x17, 0x02, 0x04, 0x06, 0x07,
                0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17], 0x01, 0x03),
    gc: GC_CGA2,
};

const MODE_5: ModeParams = ModeParams { cga_mode: 0x2e, ..MODE_4 };

#[rustfmt::skip]
const MODE_6: ModeParams = ModeParams {
    kind: Kind::Cga1, cols: 80, width: 640, page_size: 0x4000, pages: 1,
    cga_mode: 0x1e, cga_palette: 0x3f,
    seq: [0x01, 0x01, 0x00, 0x06], misc: 0x23, crtc: CRTC_640,
    attr: attr([0x00, 0x17, 0x17, 0x17, 0x17, 0x17, 0x17, 0x17,
                0x17, 0x17, 0x17, 0x17, 0x17, 0x17, 0x17, 0x17], 0x01, 0x01),
    gc: GC_CGA1,
};

#[rustfmt::skip]
const MODE_D: ModeParams = ModeParams {
    kind: Kind::Planar, cols: 40, width: 320, page_size: 0x2000, pages: 8,
    cga_mode: 0x29, cga_palette: 0x30,
    seq: [0x0b, 0x0f, 0x00, 0x06], misc: 0x23, crtc: planar_crtc(CRTC_320),
    attr: attr(PALETTE16, 0x01, 0x0f),
    gc: GC_PLANAR,
};

#[rustfmt::skip]
const MODE_E: ModeParams = ModeParams {
    kind: Kind::Planar, cols: 80, width: 640, page_size: 0x4000, pages: 4,
    cga_mode: 0x29, cga_palette: 0x30,
    seq: [0x01, 0x0f, 0x00, 0x06], misc: 0x23, crtc: planar_crtc(CRTC_640),
    attr: attr(PALETTE16, 0x01, 0x0f),
    gc: GC_PLANAR,
};

const MODE_0: ModeParams = text(40, 0x2c);
const MODE_1: ModeParams = text(40, 0x28);
const MODE_2: ModeParams = text(80, 0x2d);
const MODE_3: ModeParams = text(80, 0x29);

/// The parameters of BIOS mode `mode`, if this adapter supports it.
pub fn params(mode: u8) -> Option<&'static ModeParams> {
    Some(match mode {
        0x00 => &MODE_0,
        0x01 => &MODE_1,
        0x02 => &MODE_2,
        0x03 => &MODE_3,
        0x04 => &MODE_4,
        0x05 => &MODE_5,
        0x06 => &MODE_6,
        0x0d => &MODE_D,
        0x0e => &MODE_E,
        _ => return None,
    })
}
