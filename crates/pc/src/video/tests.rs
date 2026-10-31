// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

use super::render::{rgb6, rgbi};
use super::*;
use crate::hle::{rd8, rd16, wr8};
use cpu8086::{AX, BX, CX, DX, ES};

const A000: u32 = 0xa0000;
const B800: u32 = 0xb8000;

fn with_mode(mode: u8) -> (Video, Vec<u8>) {
    let mut v = Video::new();
    let mut ram = vec![0u8; 0x100000];
    v.set_mode(mode, &mut ram);
    (v, ram)
}

fn call(v: &mut Video, ram: &mut [u8], ax: u16, bx: u16, cx: u16, dx: u16) -> Cpu {
    let mut cpu = Cpu::new();
    cpu.regs[AX] = ax;
    cpu.regs[BX] = bx;
    cpu.regs[CX] = cx;
    cpu.regs[DX] = dx;
    assert_eq!(v.int10(&mut cpu, ram), Hle::Done, "INT 10h AX={ax:04X}");
    cpu
}

fn out(v: &mut Video, port: u16, val: u8) {
    assert!(v.port_write(port, val), "port {port:03X}");
}

fn gc(v: &mut Video, i: u8, val: u8) {
    out(v, 0x3ce, i);
    out(v, 0x3cf, val);
}

fn map_mask(v: &mut Video, val: u8) {
    out(v, 0x3c4, 2);
    out(v, 0x3c5, val);
}

fn crtc(v: &mut Video, i: u8, val: u8) {
    out(v, 0x3d4, i);
    out(v, 0x3d5, val);
}

fn planes_at(v: &Video, off: usize) -> [u8; 4] {
    std::array::from_fn(|p| v.planes.get(p, off))
}

/// Writes one byte per plane at A000:off through the map mask, in write
/// mode 0 with the default rotate, function and bit mask.
fn fill_planes(v: &mut Video, off: u32, bytes: [u8; 4]) {
    for (p, b) in bytes.into_iter().enumerate() {
        map_mask(v, 1 << p);
        v.mem_write(A000 + off, b);
    }
    map_mask(v, 0x0f);
}

fn render(v: &Video) -> Frame {
    let mut f = Frame::default();
    v.render(&mut f);
    f
}

fn px(f: &Frame, x: usize, y: usize) -> u32 {
    f.pixels[y * f.width + x]
}

/// The default 16-colour palette on a 200-line monitor.
#[rustfmt::skip]
const RGBI_EXPECT: [u32; 16] = [
    0x000000, 0x0000aa, 0x00aa00, 0x00aaaa, 0xaa0000, 0xaa00aa, 0xaa5500, 0xaaaaaa,
    0x555555, 0x5555ff, 0x55ff55, 0x55ffff, 0xff5555, 0xff55ff, 0xffff55, 0xffffff,
];

#[test]
fn write_mode0_set_reset_and_bit_mask() {
    let (mut v, _) = with_mode(0x0d);
    fill_planes(&mut v, 0, [0xaa, 0x55, 0x0f, 0xf0]);
    v.mem_read(A000);
    gc(&mut v, 1, 0x05); // set/reset drives planes 0 and 2
    gc(&mut v, 0, 0x01); // plane 0 on, plane 2 off
    gc(&mut v, 8, 0xf0);
    v.mem_write(A000, 0x3c);
    assert_eq!(planes_at(&v, 0), [0xfa, 0x35, 0x0f, 0x30]);
}

#[test]
fn write_mode0_rotate_and_functions() {
    let (mut v, _) = with_mode(0x0d);
    let latches = [0xff, 0x0f, 0x00, 0x3c];
    #[rustfmt::skip]
    let cases = [
        (0x00, [0x21; 4]),
        (0x08, [0x21, 0x01, 0x00, 0x20]),
        (0x10, [0xff, 0x2f, 0x21, 0x3d]),
        (0x18, [0xde, 0x2e, 0x21, 0x1d]),
    ];
    for (func, expect) in cases {
        fill_planes(&mut v, 0, latches);
        v.mem_read(A000);
        gc(&mut v, 3, func | 4);
        v.mem_write(A000, 0x12); // rotated right 4: 0x21
        gc(&mut v, 3, 0);
        assert_eq!(planes_at(&v, 0), expect, "function {func:02X}");
    }
}

#[test]
fn write_mode1_copies_latches_ignoring_bit_mask() {
    let (mut v, _) = with_mode(0x0d);
    fill_planes(&mut v, 10, [0x11, 0x22, 0x33, 0x44]);
    v.mem_read(A000 + 10);
    gc(&mut v, 5, 0x01);
    gc(&mut v, 8, 0x0f);
    v.mem_write(A000 + 20, 0x00);
    assert_eq!(planes_at(&v, 20), [0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn write_mode2_colour_from_data_bits() {
    let (mut v, _) = with_mode(0x0d);
    v.mem_read(A000); // latches 0
    gc(&mut v, 5, 0x02);
    gc(&mut v, 8, 0x81);
    v.mem_write(A000, 0x05);
    assert_eq!(planes_at(&v, 0), [0x81, 0x00, 0x81, 0x00]);

    // XOR against latches of all ones.
    gc(&mut v, 5, 0x00);
    gc(&mut v, 8, 0xff);
    fill_planes(&mut v, 1, [0xff; 4]);
    gc(&mut v, 5, 0x02);
    gc(&mut v, 8, 0x81);
    gc(&mut v, 3, 0x18);
    v.mem_read(A000 + 1);
    v.mem_write(A000 + 1, 0x05);
    assert_eq!(planes_at(&v, 1), [0x7e, 0xff, 0x7e, 0xff]);
}

#[test]
fn map_mask_limits_planes() {
    let (mut v, _) = with_mode(0x0d);
    map_mask(&mut v, 0x02);
    v.mem_write(A000 + 5, 0x99);
    assert_eq!(planes_at(&v, 5), [0, 0x99, 0, 0]);
}

#[test]
fn read_mode0_returns_selected_map() {
    let (mut v, _) = with_mode(0x0d);
    fill_planes(&mut v, 3, [0x10, 0x20, 0x30, 0x40]);
    for p in 0..4u8 {
        gc(&mut v, 4, p);
        assert_eq!(v.mem_read(A000 + 3), 0x10 * (p + 1));
    }
}

#[test]
fn read_mode1_colour_compare_with_dont_care() {
    let (mut v, _) = with_mode(0x0d);
    gc(&mut v, 5, 0x02);
    for x in 0..8u8 {
        gc(&mut v, 8, 0x80 >> x);
        v.mem_read(A000);
        v.mem_write(A000, x); // pixel x has colour x
    }
    gc(&mut v, 8, 0xff);
    gc(&mut v, 5, 0x08);
    gc(&mut v, 2, 0x05);
    assert_eq!(v.mem_read(A000), 0x80 >> 5);
    // Plane 0 don't care: colours 4 and 5 both match 4.
    gc(&mut v, 7, 0x0e);
    gc(&mut v, 2, 0x04);
    assert_eq!(v.mem_read(A000), (0x80 >> 4) | (0x80 >> 5));
}

#[test]
fn every_read_loads_latches() {
    let (mut v, _) = with_mode(0x0d);
    fill_planes(&mut v, 0, [0x01, 0x02, 0x04, 0x08]);
    gc(&mut v, 4, 2);
    v.mem_read(A000);
    gc(&mut v, 8, 0x00); // every bit from the latches
    v.mem_write(A000 + 7, 0xff);
    assert_eq!(planes_at(&v, 7), [0x01, 0x02, 0x04, 0x08]);
}

#[test]
fn memory_map_decoding() {
    let (mut v, _) = with_mode(0x0d);
    v.mem_write(A000 + 0x1234, 0x5a);
    assert_eq!(v.mem_read(A000 + 0x1234), 0x5a);
    assert_eq!(v.mem_read(A000 + 0xffff), 0x00);
    assert_eq!(v.mem_read(B800), 0xff);
    v.mem_write(B800, 0x77);
    assert_eq!(v.cga_mem[0], b' ');

    let (mut v, _) = with_mode(0x03);
    v.mem_write(B800 + 0x7fff, 0x42);
    assert_eq!(v.cga_mem[0x7fff], 0x42);
    assert_eq!(v.mem_read(B800 + 0x7fff), 0x42);
    assert_eq!(v.mem_read(A000), 0xff);
    assert_eq!(v.mem_read(0xb0000), 0xff);

    // 128 KiB map: A0000-BFFFF all reach the planes.
    map_mask(&mut v, 0x0f);
    gc(&mut v, 6, 0x01);
    v.mem_write(0xb8010, 0x3c);
    assert_eq!(planes_at(&v, 0x8010), [0x3c; 4]);
    // 32 KiB at B0000.
    gc(&mut v, 6, 0x09);
    v.mem_write(0xb0020, 0xc3);
    assert_eq!(planes_at(&v, 0x20), [0xc3; 4]);
    assert_eq!(v.mem_read(A000), 0xff);
    assert_eq!(v.mem_read(B800), 0xff);
}

#[test]
fn input_status1_across_a_frame() {
    let mut v = Video::new();
    let at = |line: u64, dot: u64| line * MASTER_PER_LINE + dot;
    let mut st = |now| v.port_read(0x3da, now).unwrap();
    assert_eq!(st(at(0, 0)), 0x00);
    assert_eq!(st(at(0, 639)), 0x00);
    assert_eq!(st(at(0, 640)), 0x01);
    assert_eq!(st(at(199, 0)), 0x00);
    assert_eq!(st(at(200, 0)), 0x01);
    assert_eq!(st(at(VRETRACE_LINE, 0)), 0x09);
    assert_eq!(st(at(VRETRACE_LINE + VRETRACE_LINES - 1, 900)), 0x09);
    assert_eq!(st(at(VRETRACE_LINE + VRETRACE_LINES, 0)), 0x01);
    assert_eq!(st(MASTER_PER_FRAME * 5), 0x00);

    let (mut retrace_edges, mut blank_edges, mut prev) = (0, 0, 0u8);
    for now in (MASTER_PER_FRAME..2 * MASTER_PER_FRAME).step_by(16) {
        let s = st(now);
        retrace_edges += (s & !prev & 0x08 != 0) as u32;
        blank_edges += (s & !prev & 0x01 != 0) as u32;
        prev = s;
    }
    assert_eq!(retrace_edges, 1);
    assert_eq!(blank_edges, 200);
}

#[test]
fn reading_3da_resets_attribute_flip_flop() {
    let (mut v, _) = with_mode(0x0d);
    out(&mut v, 0x3c0, 0x21); // index 1; the flip-flop now expects data
    v.port_read(0x3da, 0);
    out(&mut v, 0x3c0, 0x22); // an index again
    out(&mut v, 0x3c0, 0x3f);
    assert_eq!(v.attr[1], 0x01);
    assert_eq!(v.attr[2], 0x3f);
}

#[test]
fn registers_an_ega_lacks_are_not_answered() {
    let mut v = Video::new();
    // Outside the adapter.
    assert_eq!(v.port_read(0x3b4, 0), None);
    assert_eq!(v.port_read(0x3ba, 0), None);
    assert!(!v.port_write(0x3b4, 0));
    assert!(!v.port_write(0x3e0, 0));
    // CGA-only and unmodelled registers, and write-only ones read.
    assert!(!v.port_write(0x3d8, 0x09));
    assert!(!v.port_write(0x3d9, 0x30));
    assert!(!v.port_write(0x3da, 0));
    assert_eq!(v.port_read(0x3d9, 0), None);
    assert_eq!(v.port_read(0x3c5, 0), None);
    assert_eq!(v.port_read(0x3c0, 0), None);
    // Indexes past the last register.
    out(&mut v, 0x3c4, 0x05);
    assert!(!v.port_write(0x3c5, 0));
    out(&mut v, 0x3ce, 0x09);
    assert!(!v.port_write(0x3cf, 0));
    out(&mut v, 0x3d4, 0x19);
    assert!(!v.port_write(0x3d5, 0));
    v.port_read(0x3da, 0);
    out(&mut v, 0x3c0, 0x14);
    assert!(!v.port_write(0x3c0, 0x3f));
    out(&mut v, 0x3c0, 0x21); // the flip-flop still turned: an index
    out(&mut v, 0x3c0, 0x05);
    assert_eq!(v.attr[1], 0x05);
}

#[test]
fn crtc_start_and_cursor_are_readable() {
    let mut v = Video::new();
    crtc(&mut v, 0x0c, 0x12);
    crtc(&mut v, 0x0f, 0x34);
    crtc(&mut v, 0x13, 0x28);
    out(&mut v, 0x3d4, 0x0c);
    assert_eq!(v.port_read(0x3d5, 0), Some(0x12));
    out(&mut v, 0x3d4, 0x0f);
    assert_eq!(v.port_read(0x3d5, 0), Some(0x34));
    out(&mut v, 0x3d4, 0x13);
    assert_eq!(v.port_read(0x3d5, 0), None, "write-only");
}

#[test]
fn mode_0d_render_of_a_known_pattern() {
    let (mut v, _) = with_mode(0x0d);
    gc(&mut v, 5, 0x02);
    for x in 0..16u32 {
        gc(&mut v, 8, 0x80 >> (x % 8));
        v.mem_read(A000 + 40 + x / 8);
        v.mem_write(A000 + 40 + x / 8, x as u8); // line 1, pixel x = colour x
    }
    let f = render(&v);
    assert_eq!((f.width, f.height, f.pixels.len()), (320, 200, 320 * 200));
    for (x, &colour) in RGBI_EXPECT.iter().enumerate() {
        assert_eq!(px(&f, x, 1), colour, "pixel {x}");
        assert_eq!(px(&f, x, 0), 0);
    }

    // Colour plane enable masks the pixel value.
    out(&mut v, 0x3c0, 0x32);
    out(&mut v, 0x3c0, 0x01);
    assert_eq!(px(&render(&v), 3, 1), RGBI_EXPECT[1]);
    out(&mut v, 0x3c0, 0x32);
    out(&mut v, 0x3c0, 0x0f);

    // The start address is live until a retrace latches it.
    crtc(&mut v, 0x0d, 40);
    assert_eq!(px(&render(&v), 15, 0), RGBI_EXPECT[15]);
    v.vretrace();
    crtc(&mut v, 0x0d, 0);
    assert_eq!(px(&render(&v), 15, 0), RGBI_EXPECT[15]);
    v.vretrace();
    assert_eq!(px(&render(&v), 15, 0), 0);
    assert_eq!(px(&render(&v), 15, 1), RGBI_EXPECT[15]);

    // The CRTC offset sets the line stride.
    crtc(&mut v, 0x13, 0x28);
    assert_eq!(px(&render(&v), 15, 1), 0);
}

#[test]
fn palette_decoders() {
    assert_eq!(rgbi(0x06), 0xaa5500);
    assert_eq!(rgbi(0x16), 0xffff55);
    assert_eq!(rgbi(0x3f), 0xffffff);
    assert_eq!(rgbi(0x08), 0x000000);
    assert_eq!(rgb6(0x14), 0xaa5500);
    assert_eq!(rgb6(0x38), 0x555555);
    assert_eq!(rgb6(0x3e), 0xffff55);
}

#[test]
fn palette_address_source_clear_blanks_the_screen() {
    let (mut v, _) = with_mode(0x0d);
    v.mem_write(A000, 0x80);
    out(&mut v, 0x3c0, 0x00);
    out(&mut v, 0x3c0, 0x3f);
    assert_eq!(px(&render(&v), 0, 0), 0);
    out(&mut v, 0x3c0, 0x20);
    assert_eq!(px(&render(&v), 0, 0), 0xffffff);
}

#[test]
fn render_text_modes() {
    let (mut v, _) = with_mode(0x03);
    v.mem_write(B800, b'A');
    v.mem_write(B800 + 1, 0x1e);
    v.mem_write(B800 + 2, 0xdb);
    v.mem_write(B800 + 3, 0x06);
    crtc(&mut v, 0x0a, 0x20); // cursor off
    let f = render(&v);
    assert_eq!((f.width, f.height), (640, 200));
    // 'A' row 0 is ..##....: yellow on blue.
    assert_eq!(px(&f, 0, 0), 0x0000aa);
    assert_eq!(px(&f, 2, 0), 0xffff55);
    // Colour 6 is brown.
    assert_eq!(px(&f, 8, 0), 0xaa5500);
    assert_eq!(px(&f, 16, 0), 0);

    let (mut v, _) = with_mode(0x01);
    v.mem_write(B800 + 82, 0xdb); // row 1, column 1
    v.mem_write(B800 + 83, 0x0f);
    let f = render(&v);
    assert_eq!((f.width, f.height), (320, 200));
    assert_eq!(px(&f, 8, 8), 0xffffff);
    assert_eq!(px(&f, 7, 8), 0);
}

#[test]
fn text_cursor_and_blink_attribute() {
    let (mut v, mut ram) = with_mode(0x03);
    v.mem_write(B800, b' ');
    v.mem_write(B800 + 1, 0x9f);
    let f = render(&v);
    // Cursor at 0,0 on lines 6-7; the blink bit leaves a dim background.
    assert_eq!(px(&f, 0, 0), 0x0000aa);
    assert_eq!(px(&f, 0, 5), 0x0000aa);
    assert_eq!(px(&f, 0, 6), 0xffffff);
    call(&mut v, &mut ram, 0x1003, 0x0000, 0, 0);
    assert_eq!(px(&render(&v), 0, 0), 0x5555ff);
    assert_eq!(rd8(&ram, 0x465) & 0x20, 0);
}

#[test]
fn render_cga_compatible_modes() {
    let (mut v, _) = with_mode(0x04);
    v.mem_write(B800, 0x1b); // pixels 0-3 = colours 0, 1, 2, 3
    v.mem_write(B800 + 0x2000, 0xc0); // line 1
    let f = render(&v);
    assert_eq!((f.width, f.height), (320, 200));
    assert_eq!([px(&f, 0, 0), px(&f, 1, 0), px(&f, 2, 0), px(&f, 3, 0)], [0, 0x55ffff, 0xff55ff, 0xffffff]);
    assert_eq!(px(&f, 0, 1), 0xffffff);
    assert_eq!(px(&f, 0, 2), 0);

    let (mut v, _) = with_mode(0x06);
    v.mem_write(B800 + 0x2000 + 80, 0x40); // line 3, pixel 1
    let f = render(&v);
    assert_eq!((f.width, f.height), (640, 200));
    assert_eq!(px(&f, 1, 3), 0xffffff);
    assert_eq!(px(&f, 0, 3), 0);
    assert_eq!(px(&f, 1, 2), 0);
}

#[test]
fn render_mode_0e() {
    let (mut v, _) = with_mode(0x0e);
    gc(&mut v, 5, 0x02);
    gc(&mut v, 8, 0x01);
    v.mem_read(A000 + 80 + 79);
    v.mem_write(A000 + 80 + 79, 0x0c); // line 1, x = 639
    let f = render(&v);
    assert_eq!((f.width, f.height), (640, 200));
    assert_eq!(px(&f, 639, 1), 0xff5555);
    assert_eq!(px(&f, 638, 1), 0);
}

#[test]
fn set_mode_fills_the_bda() {
    let (_, ram) = with_mode(0x0d);
    assert_eq!(rd8(&ram, 0x449), 0x0d);
    assert_eq!(rd16(&ram, 0x44a), 40);
    assert_eq!(rd16(&ram, 0x44c), 0x2000);
    assert_eq!(rd16(&ram, 0x463), 0x3d4);
    assert_eq!(rd8(&ram, 0x484), 24);
    assert_eq!(rd16(&ram, 0x485), 8);
    assert_eq!(rd8(&ram, 0x487), 0x60);
    assert_eq!(rd8(&ram, 0x488), 0x09);
    let (_, ram) = with_mode(0x03);
    assert_eq!(rd16(&ram, 0x44a), 80);
    assert_eq!(rd16(&ram, 0x460), 0x0607);
}

#[test]
#[should_panic(expected = "13h")]
fn set_mode_refuses_unknown_modes() {
    with_mode(0x13);
}

#[test]
fn int10_get_mode_after_set_mode() {
    let (mut v, mut ram) = with_mode(0x03);
    call(&mut v, &mut ram, 0x000d, 0, 0, 0);
    let cpu = call(&mut v, &mut ram, 0x0f00, 0, 0, 0);
    assert_eq!(cpu.regs[AX], 0x280d);
    assert_eq!(cpu.regs[BX] >> 8, 0);
    // Bit 7 keeps display memory and is reported back.
    v.mem_write(A000, 0x55);
    call(&mut v, &mut ram, 0x008d, 0, 0, 0);
    assert_eq!(v.mem_read(A000), 0x55);
    assert_eq!(call(&mut v, &mut ram, 0x0f00, 0, 0, 0).regs[AX], 0x288d);
}

#[test]
fn teletype_then_text_lines() {
    let (mut v, mut ram) = with_mode(0x03);
    for &b in b"Hello\r\nW\xb3rld\x08\x08x" {
        call(&mut v, &mut ram, 0x0e00 | b as u16, 0, 0, 0);
    }
    let lines = v.text_lines().unwrap();
    assert_eq!(lines.len(), 25);
    assert_eq!(lines[0], "Hello");
    assert_eq!(lines[1], "W│rxd");
    let cpu = call(&mut v, &mut ram, 0x0300, 0, 0, 0);
    assert_eq!(cpu.regs[DX], 0x0104);
    assert_eq!(cpu.regs[CX], 0x0607);
    assert_eq!((v.crtc[0x0e], v.crtc[0x0f]), (0, 84));

    // Past the bottom line the screen scrolls.
    for i in 0..30 {
        for b in format!("line {i}\r\n").bytes() {
            call(&mut v, &mut ram, 0x0e00 | b as u16, 0, 0, 0);
        }
    }
    let lines = v.text_lines().unwrap();
    assert_eq!(lines[0], "line 6");
    assert_eq!(lines[23], "line 29");
    assert_eq!(lines[24], "");

    let (v, _) = with_mode(0x0d);
    assert_eq!(v.text_lines(), None);
}

#[test]
fn int10_cursor_char_and_attribute() {
    let (mut v, mut ram) = with_mode(0x03);
    call(&mut v, &mut ram, 0x0200, 0, 0, 0x0203); // row 2, column 3
    call(&mut v, &mut ram, 0x0900 | b'*' as u16, 0x004e, 3, 0);
    call(&mut v, &mut ram, 0x0a00 | b'#' as u16, 0x0000, 1, 0);
    assert_eq!(v.text_lines().unwrap()[2], "   #**");
    assert_eq!(call(&mut v, &mut ram, 0x0800, 0, 0, 0).regs[AX], 0x4e00 | b'#' as u16);
    // Page 1 starts 4 KiB in; the CRTC start is in words.
    call(&mut v, &mut ram, 0x0501, 0, 0, 0);
    assert_eq!(rd16(&ram, 0x44e), 0x1000);
    assert_eq!((v.crtc[0x0c], v.crtc[0x0d]), (0x08, 0x00));
    assert_eq!(v.text_lines().unwrap()[2], "");
}

#[test]
fn int10_scroll_text_window() {
    let (mut v, mut ram) = with_mode(0x03);
    for (row, ch) in b"abcd".iter().enumerate() {
        v.mem_write(B800 + row as u32 * 160, *ch);
    }
    call(&mut v, &mut ram, 0x0601, 0x1f00, 0x0000, 0x0350);
    let lines = v.text_lines().unwrap();
    assert_eq!(&lines[..5], ["b", "c", "d", "", ""]);
    assert_eq!(v.mem_read(B800 + 3 * 160 + 1), 0x1f);
    assert_eq!(v.mem_read(B800 + 4 * 160 + 1), 0x07);
    call(&mut v, &mut ram, 0x0702, 0x0700, 0x0000, 0x0350);
    assert_eq!(&v.text_lines().unwrap()[..4], ["", "", "b", "c"]);
    // AL=0 clears the window.
    call(&mut v, &mut ram, 0x0600, 0x0700, 0x0000, 0x184f);
    assert!(v.text_lines().unwrap().iter().all(String::is_empty));
}

#[test]
fn int10_graphics_text_pixels_and_scroll() {
    let (mut v, mut ram) = with_mode(0x0d);
    call(&mut v, &mut ram, 0x0200, 0, 0, 0x0100); // row 1
    call(&mut v, &mut ram, 0x0e00 | b'A' as u16, 0x000f, 0, 0);
    call(&mut v, &mut ram, 0x0200, 0, 0, 0x0100);
    assert_eq!(call(&mut v, &mut ram, 0x0800, 0, 0, 0).regs[AX] & 0xff, b'A' as u16);
    assert_eq!(planes_at(&v, 8 * 40), [0x30; 4]); // 'A' row 0 is ..##....

    // Up one row, filling with colour 4.
    call(&mut v, &mut ram, 0x0601, 0x0400, 0x0000, 0x1827);
    assert_eq!(planes_at(&v, 0), [0x30; 4]);
    assert_eq!(planes_at(&v, 24 * 8 * 40), [0, 0, 0xff, 0]);
    call(&mut v, &mut ram, 0x0600, 0x0000, 0x0000, 0x1827);
    assert_eq!(planes_at(&v, 0), [0; 4]);

    call(&mut v, &mut ram, 0x0c0b, 0, 9, 3);
    assert_eq!(call(&mut v, &mut ram, 0x0d00, 0, 9, 3).regs[AX] & 0xff, 0x0b);
    call(&mut v, &mut ram, 0x0c81, 0, 9, 3);
    assert_eq!(call(&mut v, &mut ram, 0x0d00, 0, 9, 3).regs[AX] & 0xff, 0x0a);

    for mode in [0x04, 0x06] {
        let (mut v, mut ram) = with_mode(mode);
        call(&mut v, &mut ram, 0x0c01, 0, 5, 7);
        call(&mut v, &mut ram, 0x0c81, 0, 6, 7);
        assert_eq!(call(&mut v, &mut ram, 0x0d00, 0, 5, 7).regs[AX] & 0xff, 1, "mode {mode}");
        assert_eq!(call(&mut v, &mut ram, 0x0d00, 0, 6, 7).regs[AX] & 0xff, 1, "mode {mode}");
        assert_eq!(call(&mut v, &mut ram, 0x0d00, 0, 4, 7).regs[AX] & 0xff, 0, "mode {mode}");
        call(&mut v, &mut ram, 0x0900 | b'Z' as u16, 0x0003, 1, 0);
        assert_eq!(call(&mut v, &mut ram, 0x0800, 0, 0, 0).regs[AX] & 0xff, b'Z' as u16, "mode {mode}");
    }
}

#[test]
fn int10_palette_functions() {
    let (mut v, mut ram) = with_mode(0x0d);
    call(&mut v, &mut ram, 0x1000, 0x3f05, 0, 0);
    assert_eq!(v.attr[5], 0x3f);
    for i in 0..17u32 {
        wr8(&mut ram, 0x1000 + i, 0x20 + i as u8);
    }
    let mut cpu = Cpu::new();
    cpu.regs[AX] = 0x1002;
    cpu.sregs[ES] = 0x0100;
    cpu.regs[DX] = 0;
    assert_eq!(v.int10(&mut cpu, &mut ram), Hle::Done);
    let expect: [u8; 16] = std::array::from_fn(|i| 0x20 + i as u8);
    assert_eq!(v.attr[..16], expect);
    assert_eq!(v.attr[0x11], 0x30);
}

#[test]
fn int10_ega_info_and_vga_probe() {
    let (mut v, mut ram) = with_mode(0x03);
    let cpu = call(&mut v, &mut ram, 0x1200, 0xff10, 0xffff, 0);
    assert_eq!((cpu.regs[BX], cpu.regs[CX]), (0x0003, 0x0009));
    assert_eq!(call(&mut v, &mut ram, 0x1a00, 0, 0, 0).regs[AX], 0x1a00);
}

#[test]
fn int10_unknown_functions_are_reported() {
    let (mut v, mut ram) = with_mode(0x03);
    let mut cpu = Cpu::new();
    cpu.regs[AX] = 0x1130;
    cpu.regs[BX] = 0x0600;
    assert_eq!(v.int10(&mut cpu, &mut ram), Hle::Unsupported("INT 10h AX=1130 BX=0600".into()));
    cpu.regs[AX] = 0x0013;
    assert!(matches!(v.int10(&mut cpu, &mut ram), Hle::Unsupported(_)));
    cpu.regs[AX] = 0x0c01; // pixels in a text mode
    assert!(matches!(v.int10(&mut cpu, &mut ram), Hle::Unsupported(_)));
}

#[test]
fn glyphs_cover_cp437() {
    for code in 0..=255u8 {
        let blank = font::glyph(code).iter().all(|&r| r == 0);
        assert_eq!(blank, matches!(code, 0x00 | 0x20 | 0xff), "code {code:02X}");
    }
    assert_eq!(font::glyph(0xdb), &[0xff; 8]);
    assert_eq!(font::glyph(b'A')[0], 0x30);
    assert_eq!(CP437[0xc9], '╔');
    assert_eq!(CP437[0x01], '☺');
    assert_eq!(CP437[b'~' as usize], '~');
}
