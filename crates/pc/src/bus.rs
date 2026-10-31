// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! The system bus: memory map, I/O port decoding and the master clock.
//!
//! Memory: 640 KiB RAM, video memory A0000-BFFFF (the adapter decodes it),
//! the EGA BIOS signature at C0000 and the system BIOS ROM at F0000 (both
//! read-only). Ports of hardware this PC does not have (mono adapter,
//! printers, serial ports, game port, DMA) read 0xFF and ignore writes, as an
//! empty bus would, and are counted. Any other unknown port stops the machine.

use crate::keyboard::KeyboardCtl;
use crate::pic::Pic;
use crate::pit::{MASTER_PER_PIT, Pit, SpeakerState};
use crate::video::Video;
use crate::Stop;
use std::collections::BTreeMap;

/// The PC's 14.31818 MHz crystal; all timing is in its ticks.
pub const MASTER_HZ: u64 = 14_318_180;
/// The clock is kept in 1/256 master ticks so CPU speed scales finely.
pub const FP_SHIFT: u32 = 8;

pub struct PcBus {
    /// The whole 1 MiB address space; A0000-BFFFF is unused (video decodes it).
    pub ram: Vec<u8>,
    pub video: Video,
    pub pic: Pic,
    pub pit: Pit,
    pub kbd: KeyboardCtl,
    /// Last value written to port 61h (PPI port B).
    pub port61: u8,
    /// Time in 1/256 master ticks.
    pub now_fp: u64,
    /// Set by a device access the machine must stop for.
    pub stop: Option<Stop>,
    /// Speaker changes with their master tick, when recording is on.
    pub speaker_log: Option<Vec<(u64, SpeakerState)>>,
    /// Accesses to ports of absent hardware, by port.
    pub absent_ports: BTreeMap<u16, u64>,
    /// A device's next event time may have changed (PIT reprogrammed,
    /// keyboard acknowledged); the machine reschedules.
    pub events_dirty: bool,
}

fn absent_hardware(port: u16) -> bool {
    matches!(port,
        0x00..=0x0f | 0x80..=0x8f | 0xa0 | 0x201 | 0x278..=0x27f | 0x2f8..=0x2ff
        | 0x378..=0x37f | 0x3b0..=0x3bf | 0x3f8..=0x3ff)
}

impl PcBus {
    pub fn new() -> Self {
        PcBus {
            ram: vec![0; 1 << 20],
            video: Video::new(),
            pic: Pic::new(),
            pit: Pit::new(),
            kbd: KeyboardCtl::default(),
            port61: 0,
            now_fp: 0,
            stop: None,
            speaker_log: None,
            absent_ports: BTreeMap::new(),
            events_dirty: false,
        }
    }

    /// Current time in master ticks.
    #[inline]
    pub fn now(&self) -> u64 {
        self.now_fp >> FP_SHIFT
    }

    #[inline]
    pub fn pit_now(&self) -> u64 {
        self.now() / MASTER_PER_PIT
    }

    pub fn speaker_state(&self) -> SpeakerState {
        SpeakerState { ch2: self.pit.ch[2], enable: self.port61 & 2 != 0 }
    }

    fn log_speaker(&mut self) {
        let t = self.now();
        let s = self.speaker_state();
        if let Some(log) = &mut self.speaker_log {
            if log.last().map(|(_, last)| *last != s).unwrap_or(true) {
                log.push((t, s));
            }
        }
    }

    fn unknown_port(&mut self, port: u16, write: Option<u8>) {
        if self.stop.is_none() {
            self.stop = Some(Stop::UnknownPort { port, write });
        }
    }
}

impl Default for PcBus {
    fn default() -> Self {
        Self::new()
    }
}

impl cpu8086::Bus for PcBus {
    #[inline]
    fn read8(&mut self, addr: u32) -> u8 {
        if (0xa0000..0xc0000).contains(&addr) {
            self.video.mem_read(addr)
        } else {
            self.ram[addr as usize]
        }
    }

    #[inline]
    fn write8(&mut self, addr: u32, v: u8) {
        if addr < 0xa0000 {
            self.ram[addr as usize] = v;
        } else if addr < 0xc0000 {
            self.video.mem_write(addr, v);
        }
        // C0000 and up: ROM or nothing.
    }

    fn io_read8(&mut self, port: u16) -> u8 {
        let now = self.now();
        match port {
            0x20 | 0x21 => self.pic.read(port),
            0x40..=0x42 => {
                let t = self.pit_now();
                self.pit.read(port, t)
            }
            0x43 => 0xff,
            0x60 => self.kbd.latch,
            0x61 => self.port61,
            // PPI port C: switches and channel 2 output (bit 5).
            0x62 => {
                let t = self.pit_now();
                if self.pit.ch[2].output(t) { 0x20 } else { 0 }
            }
            0x3c0..=0x3df => match self.video.port_read(port, now) {
                Some(v) => v,
                None => {
                    self.unknown_port(port, None);
                    0xff
                }
            },
            _ if absent_hardware(port) => {
                *self.absent_ports.entry(port).or_default() += 1;
                0xff
            }
            _ => {
                self.unknown_port(port, None);
                0xff
            }
        }
    }

    fn io_write8(&mut self, port: u16, v: u8) {
        match port {
            0x20 | 0x21 => self.pic.write(port, v),
            0x40..=0x43 => {
                let t = self.pit_now();
                self.pit.write(port, v, t);
                self.events_dirty = true;
                if port == 0x42 || (port == 0x43 && v >> 6 == 2) {
                    self.log_speaker();
                }
            }
            0x61 => {
                let old = self.port61;
                self.port61 = v;
                self.pit.ch[2].gate = v & 1 != 0;
                // Bit 7 pulsed high acknowledges the keyboard.
                if v & 0x80 != 0 && old & 0x80 == 0 {
                    let now = self.now();
                    self.kbd.ack(now);
                    self.events_dirty = true;
                }
                if (old ^ v) & 3 != 0 {
                    self.log_speaker();
                }
            }
            0x63 => {}
            0x3c0..=0x3df => {
                if !self.video.port_write(port, v) {
                    self.unknown_port(port, Some(v));
                }
            }
            _ if absent_hardware(port) => *self.absent_ports.entry(port).or_default() += 1,
            _ => self.unknown_port(port, Some(v)),
        }
    }
}
