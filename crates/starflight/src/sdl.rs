// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! The SDL window: shows the adapter's picture at 4:3, turns host keys into
//! XT scan codes, plays the PC speaker, and paces the machine to real time.
//!
//! Emulator keys (not on an XT keyboard, so never needed by the game):
//! F11 toggles fullscreen, F12 toggles turbo, Pause pauses.

use crate::audio::{SAMPLE_RATE, Synth};
use crate::session::Session;
use anyhow::{Result, anyhow};
use pc::{Machine, Stop};
use sdl2::audio::{AudioQueue, AudioSpecDesired};
use sdl2::event::Event;
use sdl2::keyboard::Scancode;
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::video::FullscreenType;
use std::time::{Duration, Instant};

/// XT set-1 make code for a host key.
fn xt_code(sc: Scancode) -> Option<u8> {
    use Scancode::*;
    Some(match sc {
        Escape => 0x01,
        Num1 => 0x02,
        Num2 => 0x03,
        Num3 => 0x04,
        Num4 => 0x05,
        Num5 => 0x06,
        Num6 => 0x07,
        Num7 => 0x08,
        Num8 => 0x09,
        Num9 => 0x0a,
        Num0 => 0x0b,
        Minus => 0x0c,
        Equals => 0x0d,
        Backspace => 0x0e,
        Tab => 0x0f,
        Q => 0x10,
        W => 0x11,
        E => 0x12,
        R => 0x13,
        T => 0x14,
        Y => 0x15,
        U => 0x16,
        I => 0x17,
        O => 0x18,
        P => 0x19,
        LeftBracket => 0x1a,
        RightBracket => 0x1b,
        Return | KpEnter => 0x1c,
        LCtrl | RCtrl => 0x1d,
        A => 0x1e,
        S => 0x1f,
        D => 0x20,
        F => 0x21,
        G => 0x22,
        H => 0x23,
        J => 0x24,
        K => 0x25,
        L => 0x26,
        Semicolon => 0x27,
        Apostrophe => 0x28,
        Grave => 0x29,
        LShift => 0x2a,
        Backslash => 0x2b,
        Z => 0x2c,
        X => 0x2d,
        C => 0x2e,
        V => 0x2f,
        B => 0x30,
        N => 0x31,
        M => 0x32,
        Comma => 0x33,
        Period => 0x34,
        Slash | KpDivide => 0x35,
        RShift => 0x36,
        KpMultiply => 0x37,
        LAlt | RAlt => 0x38,
        Space => 0x39,
        CapsLock => 0x3a,
        F1 => 0x3b,
        F2 => 0x3c,
        F3 => 0x3d,
        F4 => 0x3e,
        F5 => 0x3f,
        F6 => 0x40,
        F7 => 0x41,
        F8 => 0x42,
        F9 => 0x43,
        F10 => 0x44,
        NumLockClear => 0x45,
        ScrollLock => 0x46,
        // The XT has only the keypad for cursor movement.
        Kp7 | Home => 0x47,
        Kp8 | Up => 0x48,
        Kp9 | PageUp => 0x49,
        KpMinus => 0x4a,
        Kp4 | Left => 0x4b,
        Kp5 => 0x4c,
        Kp6 | Right => 0x4d,
        KpPlus => 0x4e,
        Kp1 | End => 0x4f,
        Kp2 | Down => 0x50,
        Kp3 | PageDown => 0x51,
        Kp0 | Insert => 0x52,
        KpPeriod | Delete => 0x53,
        _ => return None,
    })
}

/// Largest 4:3 rectangle centred in the window.
fn fit_4_3(w: u32, h: u32) -> Rect {
    let (rw, rh) = if w * 3 > h * 4 { (h * 4 / 3, h) } else { (w, w * 3 / 4) };
    Rect::new(((w - rw) / 2) as i32, ((h - rh) / 2) as i32, rw, rh)
}

const AUDIO_MAX_QUEUED: u32 = SAMPLE_RATE / 5 * 2; // 200 ms of i16 samples, in bytes

pub fn run(mut s: Session, scale: u32) -> Result<()> {
    let sdl = sdl2::init().map_err(|e| anyhow!(e))?;
    let video = sdl.video().map_err(|e| anyhow!(e))?;
    let window = video
        .window("Starflight", 320 * scale, 240 * scale)
        .position_centered()
        .resizable()
        .build()?;
    let mut canvas = window.into_canvas().present_vsync().build()?;
    let tc = canvas.texture_creator();
    let mut events = sdl.event_pump().map_err(|e| anyhow!(e))?;

    let audio = sdl.audio().map_err(|e| anyhow!(e))?;
    let spec = AudioSpecDesired { freq: Some(SAMPLE_RATE as i32), channels: Some(1), samples: Some(1024) };
    let queue: AudioQueue<i16> = audio.open_queue(None, &spec).map_err(|e| anyhow!(e))?;
    queue.resume();
    s.machine.bus.speaker_log = Some(Vec::new());
    let mut synth = Synth::new(s.machine.bus.speaker_state(), s.machine.now());
    let mut samples = Vec::new();

    let mut frame = pc::video::Frame::default();
    let mut texture: Option<(sdl2::render::Texture, usize, usize)> = None;
    let mut shown_frames = u64::MAX;
    let mut turbo = false;
    let mut paused = false;
    let mut stopped: Option<Stop> = None;
    let mut last = Instant::now();

    'main: loop {
        for e in events.poll_iter() {
            match e {
                Event::Quit { .. } => break 'main,
                Event::KeyDown { scancode: Some(Scancode::F11), repeat: false, .. } => {
                    let w = canvas.window_mut();
                    let fs = if w.fullscreen_state() == FullscreenType::Off { FullscreenType::Desktop } else { FullscreenType::Off };
                    w.set_fullscreen(fs).map_err(|e| anyhow!(e))?;
                }
                Event::KeyDown { scancode: Some(Scancode::F12), repeat: false, .. } => turbo = !turbo,
                Event::KeyDown { scancode: Some(Scancode::Pause), repeat: false, .. } => paused = !paused,
                Event::KeyDown { scancode: Some(sc), .. } => {
                    if let Some(c) = xt_code(sc) {
                        s.machine.key_code(c);
                    }
                }
                Event::KeyUp { scancode: Some(sc), .. } => {
                    if let Some(c) = xt_code(sc) {
                        s.machine.key_code(c | 0x80);
                    }
                }
                _ => {}
            }
        }

        let now = Instant::now();
        let dt = (now - last).min(Duration::from_millis(100));
        last = now;
        if stopped.is_none() && !paused {
            let result = if turbo {
                let budget = Instant::now() + Duration::from_millis(14);
                let mut r = None;
                while r.is_none() && Instant::now() < budget {
                    r = s.machine.run_for_ms(5.0);
                }
                r
            } else {
                s.machine.run_for_ms(dt.as_secs_f64() * 1000.0)
            };
            let log = s.machine.bus.speaker_log.as_mut().map(std::mem::take).unwrap_or_default();
            if turbo {
                synth.skip_to(s.machine.now(), s.machine.bus.speaker_state());
            } else {
                samples.clear();
                synth.render(&log, s.machine.now(), &mut samples);
                if queue.size() < AUDIO_MAX_QUEUED {
                    queue.queue_audio(&samples).map_err(|e| anyhow!(e))?;
                }
            }
            if let Some(stop) = result {
                if let Stop::Exit(_) = stop {
                    break 'main;
                }
                eprint!("{}", s.report(&stop));
                canvas.window_mut().set_title(&format!("Starflight - stopped: {stop}"))?;
                stopped = Some(stop);
            }
        } else if paused {
            synth.skip_to(s.machine.now(), s.machine.bus.speaker_state());
        }

        let title = if paused {
            Some("Starflight (paused)")
        } else if stopped.is_none() {
            Some(if turbo { "Starflight (turbo)" } else { "Starflight" })
        } else {
            None
        };
        if let Some(t) = title {
            if canvas.window().title() != t {
                canvas.window_mut().set_title(t)?;
            }
        }

        if s.machine.frames != shown_frames {
            shown_frames = s.machine.frames;
            s.machine.render(&mut frame);
            if frame.width > 0 && frame.height > 0 {
                let fits = matches!(&texture, Some((_, w, h)) if *w == frame.width && *h == frame.height);
                if !fits {
                    let t = tc.create_texture_streaming(PixelFormatEnum::RGB888, frame.width as u32, frame.height as u32)?;
                    texture = Some((t, frame.width, frame.height));
                }
                let (t, w, _) = texture.as_mut().unwrap();
                let w = *w;
                t.with_lock(None, |buf, pitch| {
                    for (y, row) in frame.pixels.chunks(w).enumerate() {
                        let line = &mut buf[y * pitch..y * pitch + w * 4];
                        for (dst, p) in line.chunks_exact_mut(4).zip(row) {
                            dst.copy_from_slice(&p.to_le_bytes());
                        }
                    }
                })
                .map_err(|e| anyhow!(e))?;
            }
        }
        canvas.set_draw_color(sdl2::pixels::Color::BLACK);
        canvas.clear();
        if let Some((t, _, _)) = &texture {
            let (w, h) = canvas.output_size().map_err(|e| anyhow!(e))?;
            canvas.copy(t, None, Some(fit_4_3(w, h))).map_err(|e| anyhow!(e))?;
        }
        canvas.present();
    }
    eprintln!(
        "played {:.1}s of emulated time",
        Machine::master_to_ms(s.machine.now()) / 1000.0
    );
    Ok(())
}
