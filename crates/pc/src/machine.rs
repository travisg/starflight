// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! The machine: CPU, bus and DOS, and the run loop that ties instructions,
//! traps, interrupts and device timing together.

use crate::bus::{FP_SHIFT, MASTER_HZ, PcBus};
use crate::dos::{Dos, FileIo};
use crate::hle::{Hle, Mem};
use crate::pit::MASTER_PER_PIT;
use crate::video::{Frame, MASTER_PER_FRAME, MASTER_PER_LINE, VRETRACE_LINE};
use crate::{Stop, bios};
use cpu8086::{AX, Cpu, Step, flags, phys};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Slowest and fastest CPU clocks `Machine::set_speed` accepts, in MHz.
pub const MIN_CPU_MHZ: f64 = 0.1;
pub const MAX_CPU_MHZ: f64 = 1000.0;

/// Cost of one CPU cycle in 1/256 master ticks at `mhz`: 4.77 MHz is 3
/// master ticks per cycle, MAX_CPU_MHZ 4/256 of one.
pub(crate) fn cycle_cost_fp(mhz: f64) -> u64 {
    let cost = 3.0 * (1u64 << FP_SHIFT) as f64 * (MASTER_HZ as f64 / 3.0 / 1e6) / mhz;
    cost.round() as u64
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Emulated CPU clock in MHz (MIN_CPU_MHZ..=MAX_CPU_MHZ); 4.77 is the
    /// original IBM PC.
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

/// Largest clock a restored machine may have, in 1/256 master ticks (about
/// 40 years), so adding to it cannot overflow.
const MAX_NOW_FP: u64 = 1 << 62;

/// Called before every instruction when set.
pub type Hook = Box<dyn FnMut(&Cpu, &PcBus)>;

/// Snapshots (`save_state`, `restore_state` in `snapshot.rs`) hold every
/// field that affects execution; `hook` and `file_log` stay with the host.
#[derive(Serialize, Deserialize)]
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
    #[serde(skip)]
    pub hook: Option<Hook>,
    /// DOS file transfers, kept when `Some`.
    #[serde(skip)]
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

    /// Sets the emulated CPU clock. Panics unless `mhz` is in
    /// MIN_CPU_MHZ..=MAX_CPU_MHZ; callers validate user input.
    pub fn set_speed(&mut self, mhz: f64) {
        assert!(
            (MIN_CPU_MHZ..=MAX_CPU_MHZ).contains(&mhz),
            "CPU speed {mhz} MHz is outside {MIN_CPU_MHZ}..={MAX_CPU_MHZ} MHz"
        );
        self.cycle_cost_fp = cycle_cost_fp(mhz);
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
            if self.cpu.irq_allowed()
                && let Some(v) = self.bus.pic.acknowledge()
            {
                self.cpu.interrupt(&mut self.bus, v);
            }
            if let Some(h) = &mut self.hook {
                h(&self.cpu, &self.bus);
            }
            match self.cpu.step(&mut self.bus) {
                Step::Executed { cycles } => {
                    self.bus.now_fp += cycles as u64 * self.cycle_cost_fp;
                    self.instructions += 1;
                    if self.cpu.flag(flags::TF) {
                        let (cs, ip) = (self.cpu.instr_cs, self.cpu.instr_ip);
                        return Some(Stop::Unsupported(format!(
                            "the instruction at {cs:04X}:{ip:04X} set the trap flag; single-step (INT 1) is not implemented"
                        )));
                    }
                }
                Step::Halted => {
                    if !self.cpu.flag(flags::IF) {
                        return Some(Stop::HaltedWithInterruptsOff {
                            cs: self.cpu.sregs[cpu8086::CS],
                            ip: self.cpu.ip,
                        });
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

    /// Checks a restored machine for values the run loop and devices cannot
    /// work with (a snapshot is outside data): a cycle cost `set_speed` does
    /// not produce, event times out of step with the clock, counters the
    /// clock does not allow, and the PIT and PIC checks.
    pub(crate) fn check(&self) -> Result<(), String> {
        let costs = cycle_cost_fp(MAX_CPU_MHZ)..=cycle_cost_fp(MIN_CPU_MHZ);
        if !costs.contains(&self.cycle_cost_fp) {
            return Err(format!("CPU cycle cost {} is outside {costs:?}", self.cycle_cost_fp));
        }
        if self.bus.now_fp > MAX_NOW_FP {
            return Err(format!("clock at {} is past {MAX_NOW_FP}", self.bus.now_fp));
        }
        let now = self.bus.now();
        // The next retrace is at most a frame ahead, and at most one
        // instruction behind when a run ended before handling it.
        if self.next_vretrace.abs_diff(now) > MASTER_PER_FRAME || self.next_event > self.next_vretrace {
            return Err(format!(
                "event times out of step with the clock: now {now}, retrace {}, next event {}",
                self.next_vretrace, self.next_event
            ));
        }
        // Each instruction takes at least one 1/256 tick, each frame a frame.
        if self.instructions > self.bus.now_fp || self.frames > now / MASTER_PER_FRAME + 1 {
            return Err(format!("{} instructions and {} frames by clock {now}", self.instructions, self.frames));
        }
        self.bus.pit.check(now / MASTER_PER_PIT)?;
        self.bus.pic.check()
    }

    pub fn run_for_ms(&mut self, ms: f64) -> Option<Stop> {
        let until = self.bus.now() + Self::ms_to_master(ms);
        self.run_until(until)
    }

    fn hle(&mut self, number: u8) -> Hle {
        let now = self.bus.now();
        let ax = self.cpu.regs[AX];
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
            0x16 => guarded(number, ax, bus, |mem| bios::int16(cpu, mem)),
            0x1a => bios::int1a(cpu, &mut bus.ram),
            0x20 => Hle::Exit(0),
            0x21 => {
                self.dos.log_io = self.file_log.is_some();
                let r = guarded(number, ax, bus, |mem| self.dos.int21(cpu, mem));
                if let Some(log) = &mut self.file_log {
                    log.append(&mut self.dos.io_log);
                }
                r
            }
            // Spurious hardware interrupts.
            0x0a..=0x0f => {
                bus.pic.eoi();
                Hle::Done
            }
            v => guarded(number, ax, bus, |mem| {
                bios::simple(v, cpu, mem).unwrap_or_else(|| Hle::Unsupported(format!("trap {v:02X} has no handler")))
            }),
        }
    }
}

/// Runs handler `f` of trap `number` over guest memory; an access `Mem`
/// refuses stops the machine. `ax` is AX at the trap.
fn guarded(number: u8, ax: u16, bus: &mut PcBus, f: impl FnOnce(&mut Mem) -> Hle) -> Hle {
    let mut mem = Mem::new(&mut bus.ram, &mut bus.video);
    let r = f(&mut mem);
    match mem.into_fault() {
        Some(what) => Hle::Unsupported(format!("INT {number:02X}h with AX={ax:04X}: {what}")),
        None => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine running `com`; the drive directory is never touched.
    fn machine(com: &[u8]) -> Machine {
        Machine::new(&Config::default(), std::env::temp_dir().join("pc-machine-unused"), com)
    }

    #[test]
    fn speed_limits() {
        let mut m = machine(&[0xeb, 0xfe]);
        assert_eq!(m.cycle_cost_fp, 768, "4.77 MHz: 3 master ticks per cycle");
        m.set_speed(MAX_CPU_MHZ);
        assert!(m.cycle_cost_fp >= 1);
        let at = m.now();
        assert_eq!(m.run_until(at + 1000), None);
        assert!(m.now() >= at + 1000, "time advances at the fastest speed");
        m.set_speed(MIN_CPU_MHZ);
        assert_eq!(m.cycle_cost_fp, cycle_cost_fp(MIN_CPU_MHZ));
    }

    #[test]
    fn speeds_outside_the_limits_panic() {
        for mhz in [0.0, -1.0, 0.09, 1000.5, f64::NAN, f64::INFINITY] {
            let r = std::panic::catch_unwind(|| machine(&[0xeb, 0xfe]).set_speed(mhz));
            let msg = *r.expect_err(&format!("{mhz} MHz")).downcast::<String>().unwrap();
            assert!(msg.contains("outside"), "{msg}");
        }
    }

    #[test]
    fn restore_refuses_broken_invariants() {
        /// Part of the expected message, and the corruption.
        type Corruption = (&'static str, fn(&mut Machine));
        let corruptions: &[Corruption] = &[
            ("PIT channel 0", |m| m.bus.pit.ch[0].reload = 0),
            ("PIT channel 2", |m| m.bus.pit.ch[2].reload = 0x10001),
            ("PIT channel 1", |m| m.bus.pit.ch[1].load_time = u64::MAX),
            ("next IRQ0", |m| m.bus.pit.next_irq0 = Some(u64::MAX)),
            ("cycle cost 0", |m| m.cycle_cost_fp = 0),
            ("cycle cost 18446744073709551615", |m| m.cycle_cost_fp = u64::MAX),
            ("PIC vector base F9h", |m| m.bus.pic.base = 0xf9),
            ("clock at", |m| m.bus.now_fp = u64::MAX - 1),
            ("retrace 0", |m| m.next_vretrace = 0),
            ("next event 18446744073709551615", |m| m.next_event = u64::MAX),
            ("18446744073709551615 instructions", |m| m.instructions = u64::MAX),
        ];
        let mut target = machine(&[0xeb, 0xfe]);
        assert_eq!(target.run_for_ms(10.0), None);
        let before = target.save_state();
        let mut m = machine(&[0xeb, 0xfe]);
        assert_eq!(m.run_for_ms(50.0), None);
        let good = m.save_state();
        for (what, corrupt) in corruptions {
            m.restore_state(&good).unwrap();
            corrupt(&mut m);
            let err = target.restore_state(&m.save_state()).unwrap_err();
            assert!(err.starts_with("corrupt snapshot: ") && err.contains(what), "{what}: {err}");
            assert!(target.save_state() == before, "{what}: the machine is unchanged");
        }
        target.restore_state(&good).unwrap();
    }

    #[test]
    fn trap_flag_stops_the_machine() {
        // PUSHF; POP AX; OR AH,1; PUSH AX; POPF; NOP; JMP $
        let mut m = machine(&[0x9c, 0x58, 0x80, 0xcc, 0x01, 0x50, 0x9d, 0x90, 0xeb, 0xfe]);
        let Some(Stop::Unsupported(msg)) = m.run_for_ms(1.0) else { panic!("no stop") };
        assert!(msg.contains("0800:0106") && msg.contains("single-step"), "{msg}");
        assert_eq!(m.cpu.ip, 0x107, "stopped before the next instruction");
    }
}
