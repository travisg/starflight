// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! The machine: CPU, bus and DOS, and the run loop that ties instructions,
//! traps, interrupts and device timing together.

use crate::bus::{FP_SHIFT, MASTER_HZ, PcBus};
use crate::dos::{Dos, FileIo};
use crate::hle::Hle;
use crate::pit::MASTER_PER_PIT;
use crate::video::{Frame, MASTER_PER_FRAME, MASTER_PER_LINE, VRETRACE_LINE};
use crate::{Stop, bios};
use cpu8086::{Cpu, Step, flags, phys};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    /// Emulated CPU clock in MHz; 4.77 is the original IBM PC.
    pub cpu_mhz: f64,
    /// Segment of the program's PSP.
    pub psp_seg: u16,
    /// Command tail passed to the program.
    pub cmdline: String,
}

impl Default for Config {
    fn default() -> Self {
        Config { cpu_mhz: 4.77, psp_seg: 0x0800, cmdline: String::new() }
    }
}

/// Called before every instruction when set.
pub type Hook = Box<dyn FnMut(&Cpu, &PcBus)>;

pub struct Machine {
    pub cpu: Cpu,
    pub bus: PcBus,
    pub dos: Dos,
    /// Cost of one CPU cycle in 1/256 master ticks.
    cycle_cost_fp: u64,
    next_vretrace: u64,
    next_event: u64,
    pub frames: u64,
    pub instructions: u64,
    pub hook: Option<Hook>,
    /// DOS file transfers, kept when `Some`.
    pub file_log: Option<Vec<FileIo>>,
}

impl Machine {
    /// Powers on the PC, boots "DOS" with `drive_dir` as drive C: and loads
    /// `com` as the running program.
    pub fn new(cfg: &Config, drive_dir: PathBuf, com: &[u8]) -> Machine {
        let mut bus = PcBus::new();
        bios::install(&mut bus.ram);
        bus.video.set_mode(3, &mut bus.ram);
        let mut cpu = Cpu::new();
        let mut dos = Dos::new(drive_dir);
        dos.log_io = true;
        dos.load_com(&mut cpu, &mut bus.ram, cfg.psp_seg, com, &cfg.cmdline);
        let mut m = Machine {
            cpu,
            bus,
            dos,
            cycle_cost_fp: 0,
            next_vretrace: VRETRACE_LINE * MASTER_PER_LINE,
            next_event: 0,
            frames: 0,
            instructions: 0,
            hook: None,
            file_log: None,
        };
        m.set_speed(cfg.cpu_mhz);
        m.update_next_event();
        m
    }

    pub fn set_speed(&mut self, mhz: f64) {
        // 4.77 MHz is 3 master ticks per cycle.
        let cost = 3.0 * (1u64 << FP_SHIFT) as f64 * (MASTER_HZ as f64 / 3.0 / 1e6) / mhz;
        self.cycle_cost_fp = (cost.round() as u64).max(1);
    }

    /// Current emulated time in master ticks.
    pub fn now(&self) -> u64 {
        self.bus.now()
    }

    pub fn ms_to_master(ms: f64) -> u64 {
        (ms * MASTER_HZ as f64 / 1000.0) as u64
    }

    pub fn master_to_ms(t: u64) -> f64 {
        t as f64 * 1000.0 / MASTER_HZ as f64
    }

    /// Queues a set-1 scan code on the keyboard.
    pub fn key_code(&mut self, code: u8) {
        self.bus.kbd.push(code);
        self.update_next_event();
    }

    pub fn render(&self, frame: &mut Frame) {
        self.bus.video.render(frame);
    }

    fn update_next_event(&mut self) {
        let mut t = self.next_vretrace;
        if let Some(p) = self.bus.pit.next_irq0 {
            t = t.min(p * MASTER_PER_PIT);
        }
        if let Some(k) = self.bus.kbd.next_event() {
            t = t.min(k);
        }
        self.next_event = t;
    }

    fn process_events(&mut self) {
        let now = self.bus.now();
        if self.bus.pit.irq0_due(now / MASTER_PER_PIT) {
            self.bus.pic.raise(0);
        }
        if self.bus.kbd.poll(now) {
            self.bus.pic.raise(1);
        }
        while now >= self.next_vretrace {
            self.bus.video.vretrace();
            self.frames += 1;
            self.next_vretrace += MASTER_PER_FRAME;
        }
        self.update_next_event();
    }

    /// Advances idle time (HLT, or a BIOS call waiting for input) to the next
    /// device event, but not past `until`.
    fn idle(&mut self, until: u64) {
        let t = self.next_event.min(until);
        if t > self.bus.now() {
            self.bus.now_fp = t << FP_SHIFT;
        } else {
            self.bus.now_fp += 4 * self.cycle_cost_fp;
        }
    }

    /// Runs until emulated time reaches `until` (master ticks) or the machine
    /// stops.
    pub fn run_until(&mut self, until: u64) -> Option<Stop> {
        loop {
            let now = self.bus.now();
            if now >= until {
                return None;
            }
            if now >= self.next_event || self.bus.events_dirty {
                self.bus.events_dirty = false;
                self.process_events();
            }
            if self.cpu.irq_allowed() {
                if let Some(v) = self.bus.pic.acknowledge() {
                    self.cpu.interrupt(&mut self.bus, v);
                }
            }
            if let Some(h) = &mut self.hook {
                h(&self.cpu, &self.bus);
            }
            match self.cpu.step(&mut self.bus) {
                Step::Executed { cycles } => {
                    self.bus.now_fp += cycles as u64 * self.cycle_cost_fp;
                    self.instructions += 1;
                }
                Step::Halted => {
                    if !self.cpu.flag(flags::IF) {
                        return Some(Stop::HaltedWithInterruptsOff { cs: self.cpu.sregs[cpu8086::CS], ip: self.cpu.ip });
                    }
                    self.idle(until);
                }
                Step::Trap { number, cycles } => {
                    self.bus.now_fp += cycles as u64 * self.cycle_cost_fp;
                    let (cs, ip) = (self.cpu.instr_cs, self.cpu.instr_ip);
                    if phys(cs, ip) < 0xf0000 {
                        return Some(Stop::StrayTrap { number, cs, ip });
                    }
                    match self.hle(number) {
                        Hle::Done => {}
                        Hle::Retry => {
                            self.cpu.ip = ip;
                            self.idle(until);
                        }
                        Hle::Exit(code) => return Some(Stop::Exit(code)),
                        Hle::Unsupported(what) => return Some(Stop::Unsupported(what)),
                    }
                    self.update_next_event();
                }
                Step::Invalid { cs, ip, bytes } => return Some(Stop::InvalidOpcode { cs, ip, bytes }),
            }
            if let Some(s) = self.bus.stop.take() {
                return Some(s);
            }
        }
    }

    pub fn run_for_ms(&mut self, ms: f64) -> Option<Stop> {
        let until = self.bus.now() + Self::ms_to_master(ms);
        self.run_until(until)
    }

    fn hle(&mut self, number: u8) -> Hle {
        let now = self.bus.now();
        let bus = &mut self.bus;
        let cpu = &mut self.cpu;
        match number {
            0x08 => {
                bios::int08(&mut bus.ram);
                Hle::Done
            }
            0x09 => {
                let code = bus.kbd.latch;
                bios::int09(&mut bus.ram, code);
                bus.kbd.ack(now);
                bus.pic.eoi();
                Hle::Done
            }
            0x10 => bus.video.int10(cpu, &mut bus.ram),
            0x16 => bios::int16(cpu, &mut bus.ram),
            0x1a => bios::int1a(cpu, &mut bus.ram),
            0x20 => Hle::Exit(0),
            0x21 => {
                let r = self.dos.int21(cpu, &mut bus.ram);
                for io in self.dos.io_log.drain(..) {
                    // DOS wrote into the RAM array; route any part that lands in
                    // video memory through the adapter.
                    if !io.write {
                        let (lo, hi) = (io.addr.max(0xa0000), (io.addr + io.len).min(0xc0000));
                        for a in lo..hi {
                            let v = bus.ram[a as usize];
                            bus.video.mem_write(a, v);
                        }
                    }
                    if let Some(log) = &mut self.file_log {
                        log.push(io);
                    }
                }
                r
            }
            // Spurious hardware interrupts.
            0x0a..=0x0f => {
                bus.pic.eoi();
                Hle::Done
            }
            v => bios::simple(v, cpu, &mut bus.ram)
                .unwrap_or_else(|| Hle::Unsupported(format!("trap {v:02X} has no handler"))),
        }
    }
}
