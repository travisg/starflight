// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! PC speaker synthesis from the machine's log of speaker state changes.
//! Each output sample averages the cone position over its interval (which
//! also renders bit-banged sound through port 61h), then a DC blocker removes
//! the offset of a cone held in one position.

use pc::bus::MASTER_HZ;
use pc::pit::{MASTER_PER_PIT, SpeakerState};

pub const SAMPLE_RATE: u32 = 44_100;
const SUBSAMPLES: u64 = 8;
const AMPLITUDE: f32 = 6000.0;

pub struct Synth {
    state: SpeakerState,
    /// Master tick of the next sample.
    next: f64,
    step: f64,
    dc_in: f32,
    dc_out: f32,
}

impl Synth {
    pub fn new(state: SpeakerState, now: u64) -> Self {
        Synth { state, next: now as f64, step: MASTER_HZ as f64 / SAMPLE_RATE as f64, dc_in: 0.0, dc_out: 0.0 }
    }

    /// Renders samples up to master tick `until`, applying `events` (sorted by
    /// time) as their times are reached.
    pub fn render(&mut self, events: &[(u64, SpeakerState)], until: u64, out: &mut Vec<i16>) {
        let mut ev = events.iter().peekable();
        while self.next + self.step <= until as f64 {
            let start = self.next;
            let mut sum = 0.0f32;
            for k in 0..SUBSAMPLES {
                let t = (start + self.step * k as f64 / SUBSAMPLES as f64) as u64;
                while let Some(&&(et, s)) = ev.peek() {
                    if et > t {
                        break;
                    }
                    self.state = s;
                    ev.next();
                }
                if self.state.level(t / MASTER_PER_PIT) {
                    sum += 1.0;
                }
            }
            let x = sum / SUBSAMPLES as f32;
            // One-pole DC blocker.
            let y = x - self.dc_in + 0.995 * self.dc_out;
            self.dc_in = x;
            self.dc_out = y;
            out.push((y * AMPLITUDE).clamp(-32767.0, 32767.0) as i16);
            self.next += self.step;
        }
        for &(_, s) in ev {
            self.state = s;
        }
    }

    /// Skips ahead without producing sound (turbo, pauses).
    pub fn skip_to(&mut self, now: u64, state: SpeakerState) {
        self.next = now as f64;
        self.state = state;
    }
}
