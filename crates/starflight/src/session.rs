// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! A running game: the machine plus the Forth-level view of it, scripted
//! keyboard input and stop reports. Used by the headless runner and the SDL
//! front end alike.

use crate::saves::Saves;
use anyhow::{Context, Result, bail};
use cpu8086::{BP, BX, CS, phys};
use pc::hle::bda;
use pc::{Config, Machine, Stop, bios};
use sfforth::consts::STARFLT_COM_SIZE;
use sfforth::directory::{self, DirEntry};
use sfforth::trace::{Entry, JMP_BX, Names, Recent};
use sfforth::{dict, segment_image};
use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// A step of a key script.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyAction {
    /// Press and release a key (set-1 make code), holding Shift if asked.
    Press { code: u8, shift: bool },
    Wait(f64),
}

/// Parses a key script: plain characters, `{name}` for special keys
/// (enter esc space tab bs up down left right home end pgup pgdn ins del
/// f1-f10 kp0-kp9) and `{wait MS}`.
pub fn parse_keys(s: &str) -> Result<Vec<KeyAction>> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '{' {
            let Some((code, shift)) = pc::keyboard::scan_for_char(c) else {
                bail!("no key types {c:?}");
            };
            out.push(KeyAction::Press { code, shift });
            continue;
        }
        let name: String = chars.by_ref().take_while(|&c| c != '}').collect();
        let lower = name.to_ascii_lowercase();
        if let Some(ms) = lower.strip_prefix("wait") {
            let ms: f64 = ms.trim().parse().with_context(|| format!("bad wait in {{{name}}}"))?;
            out.push(KeyAction::Wait(ms));
            continue;
        }
        let code = match lower.as_str() {
            "enter" => 0x1c,
            "esc" => 0x01,
            "space" => 0x39,
            "tab" => 0x0f,
            "bs" => 0x0e,
            "up" | "kp8" => 0x48,
            "down" | "kp2" => 0x50,
            "left" | "kp4" => 0x4b,
            "right" | "kp6" => 0x4d,
            "home" | "kp7" => 0x47,
            "end" | "kp1" => 0x4f,
            "pgup" | "kp9" => 0x49,
            "pgdn" | "kp3" => 0x51,
            "ins" | "kp0" => 0x52,
            "del" => 0x53,
            "kp5" => 0x4c,
            f if f.starts_with('f') && f[1..].parse::<u8>().is_ok_and(|n| (1..=10).contains(&n)) => {
                0x3a + f[1..].parse::<u8>().unwrap()
            }
            _ => bail!("unknown key {{{name}}}"),
        };
        out.push(KeyAction::Press { code, shift: false });
    }
    Ok(out)
}

pub struct Session {
    pub machine: Machine,
    pub names: Rc<Names>,
    pub recent: Rc<RefCell<Recent>>,
    pub game_cs: u16,
    pub directory: Vec<DirEntry>,
    /// Print the text screen whenever it changes.
    pub show_text: bool,
    last_text: Option<Vec<String>>,
    cpu_mhz: f64,
}

/// Longest wait for the game to take a scripted key before giving up on it.
const KEY_TIMEOUT_MS: f64 = 20_000.0;
/// Forth words kept for stop reports.
const RECENT_WORDS: usize = 64;

impl Session {
    pub fn new(slot_dir: &Path, cpu_mhz: f64, mut trace: Option<Box<dyn Write>>) -> Result<Session> {
        let com_path = slot_dir.join("STARFLT.COM");
        let com = std::fs::read(&com_path).with_context(|| format!("reading {}", com_path.display()))?;
        if com.len() != STARFLT_COM_SIZE {
            bail!("{} is {} bytes, not the known {STARFLT_COM_SIZE}-byte build", com_path.display(), com.len());
        }
        let stara = std::fs::read(slot_dir.join("STARA.COM"))?;
        let cfg = Config { cpu_mhz, ..Default::default() };
        let game_cs = cfg.psp_seg;
        let mut machine = Machine::new(&cfg, slot_dir.to_path_buf(), &com);
        machine.file_log = Some(Vec::new());
        let names = Rc::new(Names::from_words(&dict::decode_dictionary(&segment_image(&com))));
        let recent = Rc::new(RefCell::new(Recent::new(RECENT_WORDS)));
        let (r, n) = (recent.clone(), names.clone());
        machine.hook = Some(Box::new(move |cpu, bus| {
            if cpu.sregs[CS] != game_cs {
                return;
            }
            let p = phys(game_cs, cpu.ip) as usize;
            if bus.ram[p] == JMP_BX[0] && bus.ram[p + 1] == JMP_BX[1] {
                let e = Entry { cfa: cpu.regs[BX], rp: cpu.regs[BP] };
                r.borrow_mut().push(e);
                if let Some(t) = &mut trace {
                    let _ = writeln!(t, "{:04X} {:04X} {}", e.rp, e.cfa, n.label(e.cfa));
                }
            }
        }));
        Ok(Session {
            machine,
            names,
            recent,
            game_cs,
            directory: directory::parse(&stara),
            show_text: false,
            last_text: None,
            cpu_mhz,
        })
    }

    /// Restores state `name` of save `slot` (see `Saves::load_state`), keeping
    /// this session's CPU speed, and releases the modifier keys the restored
    /// machine holds but the host does not (`host_held`: their make codes).
    /// Returns the backup of the slot's files.
    pub fn load_state(&mut self, saves: &Saves, slot: &str, name: &str, host_held: &[u8]) -> Result<PathBuf> {
        let backup = saves.load_state(slot, name, &mut self.machine)?;
        self.machine.set_speed(self.cpu_mhz);
        let stara = self.machine.dos.drive_dir.join("STARA.COM");
        self.directory = directory::parse(&std::fs::read(&stara).with_context(|| format!("reading {}", stara.display()))?);
        *self.recent.borrow_mut() = Recent::new(RECENT_WORDS);
        self.last_text = None;
        self.release_modifiers(host_held);
        Ok(backup)
    }

    /// Queues a break code for each of Ctrl, Shift and Alt that the BIOS will
    /// have down once the pending scan codes are handled, unless `host_held`
    /// lists it. A state saved with Ctrl+F11 has Ctrl down.
    fn release_modifiers(&mut self, host_held: &[u8]) {
        use pc::keyboard::shift::{ALT, CTRL, LSHIFT, RSHIFT};
        let mut flags = self.machine.bus.ram[bda::KBD_FLAGS as usize];
        for code in self.machine.bus.kbd.pending() {
            pc::keyboard::translate(code, &mut flags);
        }
        for (make, bit) in [(0x1d, CTRL), (0x2a, LSHIFT), (0x36, RSHIFT), (0x38, ALT)] {
            if flags & bit != 0 && !host_held.contains(&make) {
                self.machine.key_code(make | 0x80);
            }
        }
    }

    /// Runs `ms` of emulated time in slices, printing text-screen changes.
    pub fn run_ms(&mut self, ms: f64) -> Result<(), Stop> {
        let end = self.machine.now() + Machine::ms_to_master(ms);
        while self.machine.now() < end {
            let slice = (self.machine.now() + Machine::ms_to_master(20.0)).min(end);
            let stop = self.machine.run_until(slice);
            self.after_slice();
            if let Some(s) = stop {
                return Err(s);
            }
        }
        Ok(())
    }

    fn after_slice(&mut self) {
        if !self.show_text {
            return;
        }
        let text = self.machine.bus.video.text_lines();
        if text != self.last_text {
            if let Some(lines) = &text {
                let t = Machine::master_to_ms(self.machine.now()) / 1000.0;
                println!("---- text screen at {t:.2}s ----");
                let last = lines.iter().rposition(|l| !l.trim().is_empty()).map_or(0, |i| i + 1);
                for l in &lines[..last] {
                    println!("{}", l.trim_end());
                }
            }
            self.last_text = text;
        }
    }

    fn keys_idle(&self) -> bool {
        self.machine.bus.kbd.idle() && bios::key_buffer_empty(&self.machine.bus.ram)
    }

    /// Types one key, waiting first until the game has taken the previous one:
    /// its keyboard handler empties the BIOS buffer on every key press, so
    /// keys sent back to back would be lost.
    pub fn press(&mut self, code: u8, shift: bool) -> Result<(), Stop> {
        let mut waited = 0.0;
        while !self.keys_idle() && waited < KEY_TIMEOUT_MS {
            self.run_ms(10.0)?;
            waited += 10.0;
        }
        if waited >= KEY_TIMEOUT_MS {
            eprintln!("warning: the game did not read the previous key within {KEY_TIMEOUT_MS} ms");
        }
        self.run_ms(30.0)?;
        if shift {
            self.machine.key_code(0x2a);
        }
        self.machine.key_code(code);
        self.run_ms(50.0)?;
        self.machine.key_code(code | 0x80);
        if shift {
            self.machine.key_code(0xaa);
        }
        self.run_ms(20.0)
    }

    pub fn play_script(&mut self, script: &[KeyAction]) -> Result<(), Stop> {
        for a in script {
            match *a {
                KeyAction::Press { code, shift } => self.press(code, shift)?,
                KeyAction::Wait(ms) => self.run_ms(ms)?,
            }
        }
        Ok(())
    }

    /// Which directory entry a byte offset of STARA (1) or STARB (2) falls in.
    pub fn entry_at(&self, fileno: u8, offset: u64) -> Option<&DirEntry> {
        self.directory.iter().find(|e| e.contains(fileno, offset as u32))
    }

    /// File transfers so far, summarised by directory entry.
    pub fn file_summary(&self) -> String {
        let mut out = String::new();
        let Some(log) = &self.machine.file_log else { return out };
        for io in log {
            let fileno = match io.file.as_str() {
                "STARA.COM" => 1,
                "STARB.COM" => 2,
                _ => 0,
            };
            let entry = self.entry_at(fileno, io.offset).map(|e| e.name.as_str()).unwrap_or("-");
            out.push_str(&format!(
                "{} {} {:>6X}+{:<5} -> {:05X}  {}\n",
                if io.write { "W" } else { "R" },
                io.file,
                io.offset,
                io.len,
                io.addr,
                entry
            ));
        }
        out
    }

    /// What stopped the machine, where, and the Forth words leading up to it.
    pub fn report(&self, stop: &Stop) -> String {
        let cpu = &self.machine.cpu;
        let r = &cpu.regs;
        let s = &cpu.sregs;
        let at = phys(cpu.instr_cs, cpu.instr_ip) as usize;
        let bytes: Vec<String> = (0..8).map(|i| format!("{:02X}", self.machine.bus.ram[(at + i) & 0xfffff])).collect();
        let mut out = format!(
            "machine stopped: {stop}\n\
             at {:04X}:{:04X}  bytes {}\n\
             AX={:04X} BX={:04X} CX={:04X} DX={:04X} SP={:04X} BP={:04X} SI={:04X} DI={:04X}\n\
             CS={:04X} DS={:04X} ES={:04X} SS={:04X} FLAGS={:04X}\n\
             emulated time {:.3}s, {} instructions\n",
            cpu.instr_cs,
            cpu.instr_ip,
            bytes.join(" "),
            r[0],
            r[3],
            r[1],
            r[2],
            r[4],
            r[5],
            r[6],
            r[7],
            s[1],
            s[3],
            s[0],
            s[2],
            cpu.flags(),
            Machine::master_to_ms(self.machine.now()) / 1000.0,
            self.machine.instructions,
        );
        if cpu.sregs[CS] == self.game_cs {
            out.push_str(&format!("Forth IP (SI) = {:04X}\n", r[6]));
        }
        out.push_str("recent Forth words (oldest first):\n");
        out.push_str(&self.recent.borrow().format(&self.names));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::saves::scratch_dir;
    use pc::keyboard::shift::CTRL;

    fn ctrl_down(s: &Session) -> bool {
        s.machine.bus.ram[bda::KBD_FLAGS as usize] & CTRL != 0
    }

    /// Needs the game; skipped without it.
    #[test]
    fn load_releases_modifiers_the_host_does_not_hold() {
        let game = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/START"));
        if !game.join("STARFLT.COM").is_file() {
            eprintln!("skipping: {} is absent", game.join("STARFLT.COM").display());
            return;
        }
        let dir = scratch_dir("modifiers");
        let saves = Saves::new(dir.clone());
        let slot = saves.create("s", game, false).unwrap();
        let mut s = Session::new(&slot, 4.77, None).unwrap();
        s.run_ms(2000.0).unwrap();
        s.machine.key_code(0x1d);
        s.run_ms(100.0).unwrap();
        assert!(ctrl_down(&s));
        saves.save_state("s", Some("held"), &s.machine, None).unwrap();
        s.machine.key_code(0x9d);
        s.run_ms(100.0).unwrap();
        s.machine.key_code(0x1d);
        saves.save_state("s", Some("queued"), &s.machine, None).unwrap();

        let mut t = Session::new(&slot, 4.77, None).unwrap();
        for state in ["held", "queued"] {
            t.load_state(&saves, "s", state, &[0x1d]).unwrap();
            t.run_ms(100.0).unwrap();
            assert!(ctrl_down(&t), "{state}: the host holds Ctrl");
            t.load_state(&saves, "s", state, &[]).unwrap();
            t.run_ms(100.0).unwrap();
            assert!(!ctrl_down(&t), "{state}: Ctrl released");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn key_scripts() {
        let k = parse_keys("aB{enter}{wait 250}{f2}").unwrap();
        assert_eq!(
            k,
            vec![
                KeyAction::Press { code: 0x1e, shift: false },
                KeyAction::Press { code: 0x30, shift: true },
                KeyAction::Press { code: 0x1c, shift: false },
                KeyAction::Wait(250.0),
                KeyAction::Press { code: 0x3c, shift: false },
            ]
        );
        assert!(parse_keys("{bogus}").is_err());
    }
}
