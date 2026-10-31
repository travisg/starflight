// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Runs the SingleStepTests 8088 v2 suite (Daniel Balsom, MIT,
//! https://github.com/SingleStepTests/8088) against the core.
//!
//! Only tests whose metadata status is "normal" run. The data directory is
//! `SST_8088_DIR` (default `target/sst-8088/v2` in the workspace, where
//! `scripts/fetch-sst.sh` puts it); if it is missing the test passes with a
//! skip message.
//!
//! - `SST_FILTER=F6,D0.4` runs only files whose names start with one of the
//!   comma-separated prefixes.
//! - `SST_FAILS=path` writes every failing test (file, idx, name) to `path`.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use cpu8086::{AX, BP, BX, Bus, CS, CX, Cpu, DI, DS, DX, ES, SI, SP, SS, Step};
use serde::Deserialize;

const DEFAULT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/sst-8088/v2");
const MAX_STEPS: usize = 0x20000;
const SHOW_FAILS: usize = 3;

#[derive(Deserialize, Default, Clone, Copy)]
struct Regs {
    ax: Option<u16>,
    bx: Option<u16>,
    cx: Option<u16>,
    dx: Option<u16>,
    cs: Option<u16>,
    ss: Option<u16>,
    ds: Option<u16>,
    es: Option<u16>,
    sp: Option<u16>,
    bp: Option<u16>,
    si: Option<u16>,
    di: Option<u16>,
    ip: Option<u16>,
    flags: Option<u16>,
}

#[derive(Deserialize)]
struct State {
    regs: Regs,
    ram: Vec<(u32, u8)>,
}

#[derive(Deserialize)]
struct Test {
    name: String,
    bytes: Vec<u8>,
    initial: State,
    #[serde(rename = "final")]
    fin: State,
    idx: usize,
}

#[derive(Deserialize)]
struct OpMeta {
    status: Option<String>,
    #[serde(rename = "flags-mask")]
    flags_mask: Option<u16>,
    reg: Option<HashMap<String, OpMeta>>,
}

#[derive(Deserialize)]
struct Metadata {
    opcodes: HashMap<String, OpMeta>,
}

/// Flat 1 MiB of RAM; I/O reads float high and writes are dropped.
struct TestBus {
    mem: Vec<u8>,
    dirty: Vec<u32>,
}

impl TestBus {
    fn new() -> Self {
        TestBus { mem: vec![0; 1 << 20], dirty: Vec::new() }
    }

    fn load(&mut self, addr: u32, v: u8) {
        let a = addr & 0xfffff;
        self.mem[a as usize] = v;
        self.dirty.push(a);
    }

    fn reset(&mut self) {
        for &a in &self.dirty {
            self.mem[a as usize] = 0;
        }
        self.dirty.clear();
    }
}

impl Bus for TestBus {
    fn read8(&mut self, addr: u32) -> u8 {
        self.mem[(addr & 0xfffff) as usize]
    }

    fn write8(&mut self, addr: u32, val: u8) {
        self.load(addr, val);
    }

    fn io_read8(&mut self, _port: u16) -> u8 {
        0xff
    }

    fn io_write8(&mut self, _port: u16, _val: u8) {}
}

/// Name, getter and setter for each register in the test format.
type RegField = (&'static str, fn(&Regs) -> Option<u16>, fn(&Cpu) -> u16, fn(&mut Cpu, u16));

const REGS: [RegField; 13] = [
    ("ax", |r| r.ax, |c| c.regs[AX], |c, v| c.regs[AX] = v),
    ("bx", |r| r.bx, |c| c.regs[BX], |c, v| c.regs[BX] = v),
    ("cx", |r| r.cx, |c| c.regs[CX], |c, v| c.regs[CX] = v),
    ("dx", |r| r.dx, |c| c.regs[DX], |c, v| c.regs[DX] = v),
    ("cs", |r| r.cs, |c| c.sregs[CS], |c, v| c.sregs[CS] = v),
    ("ss", |r| r.ss, |c| c.sregs[SS], |c, v| c.sregs[SS] = v),
    ("ds", |r| r.ds, |c| c.sregs[DS], |c, v| c.sregs[DS] = v),
    ("es", |r| r.es, |c| c.sregs[ES], |c, v| c.sregs[ES] = v),
    ("sp", |r| r.sp, |c| c.regs[SP], |c, v| c.regs[SP] = v),
    ("bp", |r| r.bp, |c| c.regs[BP], |c, v| c.regs[BP] = v),
    ("si", |r| r.si, |c| c.regs[SI], |c, v| c.regs[SI] = v),
    ("di", |r| r.di, |c| c.regs[DI], |c, v| c.regs[DI] = v),
    ("ip", |r| r.ip, |c| c.ip, |c, v| c.ip = v),
];

fn is_prefix(b: u8) -> bool {
    matches!(b, 0x26 | 0x2e | 0x36 | 0x3e | 0xf0 | 0xf2 | 0xf3)
}

/// ModRM reg field of the test's instruction.
fn modrm_reg(bytes: &[u8]) -> Option<u8> {
    let i = bytes.iter().position(|&b| !is_prefix(b))?;
    bytes.get(i + 1).map(|m| (m >> 3) & 7)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

/// Runs one test. Returns the differing fields (empty on a pass) and a
/// human-readable report.
fn run_test(bus: &mut TestBus, t: &Test, flags_mask: u16) -> (Vec<String>, String) {
    bus.reset();
    let mut cpu = Cpu::new();
    let init = &t.initial.regs;
    for (_, get, _, set) in REGS {
        set(&mut cpu, get(init).unwrap_or(0));
    }
    cpu.set_flags(init.flags.unwrap_or(0));
    for &(a, v) in &t.initial.ram {
        bus.load(a, v);
    }

    let mut diffs = Vec::new();
    let mut report = String::new();
    let mut steps = 0;
    loop {
        match cpu.step(bus) {
            Step::Executed { .. } | Step::Halted => {}
            s @ (Step::Invalid { .. } | Step::Trap { .. }) => {
                diffs.push("step".to_string());
                let _ = writeln!(report, "    step returned {s:?}");
                break;
            }
        }
        steps += 1;
        if !cpu.rep_pending {
            break;
        }
        if steps >= MAX_STEPS {
            diffs.push("steps".to_string());
            let _ = writeln!(report, "    REP still pending after {steps} steps");
            break;
        }
    }

    let fin = &t.fin.regs;
    for (name, get, cpu_get, _) in REGS {
        let want = get(fin).or(get(init)).unwrap_or(0);
        let got = cpu_get(&cpu);
        if want != got {
            diffs.push(name.to_string());
            let _ = writeln!(report, "    {name}: want {want:04X} got {got:04X}");
        }
    }
    let want = fin.flags.or(init.flags).unwrap_or(0);
    let got = cpu.flags();
    if want & flags_mask != got & flags_mask {
        diffs.push("flags".to_string());
        let x = (want ^ got) & flags_mask;
        let _ = writeln!(
            report,
            "    flags: want {want:04X} got {got:04X} xor {x:04X} [{}] (mask {flags_mask:04X})",
            flag_names(x)
        );
    }
    let mut ram_bad = false;
    for &(a, v) in &t.fin.ram {
        let got = bus.mem[(a & 0xfffff) as usize];
        if got != v {
            ram_bad = true;
            let _ = writeln!(report, "    ram[{a:05X}]: want {v:02X} got {got:02X} xor {:02X}", v ^ got);
        }
    }
    if ram_bad {
        diffs.push("ram".to_string());
    }
    (diffs, report)
}

fn flag_names(f: u16) -> String {
    const NAMES: [(u16, char); 9] = [
        (0x800, 'o'),
        (0x400, 'd'),
        (0x200, 'i'),
        (0x100, 't'),
        (0x80, 's'),
        (0x40, 'z'),
        (0x10, 'a'),
        (0x4, 'p'),
        (0x1, 'c'),
    ];
    NAMES.iter().filter(|(m, _)| f & m != 0).map(|(_, c)| *c).collect()
}

fn init_regs_line(r: &Regs) -> String {
    let mut s = String::new();
    for (name, get, _, _) in REGS {
        let _ = write!(s, "{name}={:04X} ", get(r).unwrap_or(0));
    }
    let _ = write!(s, "flags={:04X}", r.flags.unwrap_or(0));
    s
}

struct FileResult {
    name: String,
    passed: usize,
    total: usize,
    skipped: usize,
    /// Failure count per set of differing fields.
    kinds: BTreeMap<String, usize>,
    shown: String,
    all_fails: Vec<String>,
}

/// Status and flags mask for a test, from the file's opcode and ModRM reg.
fn test_meta<'a>(meta: &'a Metadata, op: &str, file_reg: Option<u8>, bytes: &[u8]) -> Option<&'a OpMeta> {
    let m = meta.opcodes.get(op)?;
    match &m.reg {
        None => Some(m),
        Some(regs) => {
            let r = file_reg.or_else(|| modrm_reg(bytes))?;
            regs.get(&r.to_string())
        }
    }
}

fn run_file(meta: &Metadata, path: &Path, bus: &mut TestBus) -> FileResult {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let stem = name.trim_end_matches(".json.gz");
    let mut parts = stem.split('.');
    let op = parts.next().unwrap().to_string();
    let file_reg = parts.next().and_then(|r| r.parse::<u8>().ok());

    let mut res = FileResult {
        name: name.clone(),
        passed: 0,
        total: 0,
        skipped: 0,
        kinds: BTreeMap::new(),
        shown: String::new(),
        all_fails: Vec::new(),
    };

    let mut raw = Vec::new();
    let f = std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    flate2::read::GzDecoder::new(std::io::BufReader::new(f))
        .read_to_end(&mut raw)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let tests: Vec<Test> = serde_json::from_slice(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    drop(raw);

    let mut shown = 0;
    for t in &tests {
        let Some(m) = test_meta(meta, &op, file_reg, &t.bytes) else {
            res.skipped += 1;
            continue;
        };
        if m.status.as_deref() != Some("normal") {
            res.skipped += 1;
            continue;
        }
        res.total += 1;
        let mask = m.flags_mask.unwrap_or(0xffff);
        let (diffs, report) = run_test(bus, t, mask);
        if diffs.is_empty() {
            res.passed += 1;
            continue;
        }
        let kind = diffs.join(",");
        *res.kinds.entry(kind.clone()).or_default() += 1;
        res.all_fails.push(format!("{name}\t{}\t{}\t{}\t{kind}", t.idx, t.name, hex(&t.bytes)));
        if shown < SHOW_FAILS {
            shown += 1;
            let _ = writeln!(res.shown, "  FAIL #{} \"{}\" [{}]", t.idx, t.name, hex(&t.bytes));
            let _ = writeln!(res.shown, "    initial: {}", init_regs_line(&t.initial.regs));
            res.shown.push_str(&report);
        }
    }
    res
}

#[test]
fn single_step_tests_8088() {
    let dir = PathBuf::from(std::env::var("SST_8088_DIR").unwrap_or_else(|_| DEFAULT_DIR.to_string()));
    if !dir.join("metadata.json").is_file() {
        println!("SKIP: SingleStepTests 8088 data not found at {} (set SST_8088_DIR)", dir.display());
        return;
    }
    let meta: Metadata = serde_json::from_slice(&std::fs::read(dir.join("metadata.json")).unwrap()).unwrap();

    let filters: Vec<String> = std::env::var("SST_FILTER")
        .ok()
        .map(|s| s.split(',').map(|p| p.trim().to_ascii_uppercase()).filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().to_ascii_uppercase();
            n.ends_with(".JSON.GZ") && (filters.is_empty() || filters.iter().any(|f| n.starts_with(f.as_str())))
        })
        .collect();
    files.sort();

    // Big files first so the parallel tail is short.
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(std::fs::metadata(&files[i]).map(|m| m.len()).unwrap_or(0)));

    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<FileResult>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let mut bus = TestBus::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&fi) = order.get(i) else { break };
                    let r = run_file(&meta, &files[fi], &mut bus);
                    results.lock().unwrap().push(r);
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by(|a, b| a.name.cmp(&b.name));

    let (mut passed, mut total, mut skipped) = (0, 0, 0);
    let mut failing = Vec::new();
    for r in &results {
        passed += r.passed;
        total += r.total;
        skipped += r.skipped;
        let skip = if r.skipped > 0 { format!(" ({} skipped)", r.skipped) } else { String::new() };
        if r.passed == r.total {
            println!("{}: {}/{}{skip}", r.name, r.passed, r.total);
            continue;
        }
        failing.push(format!("{} {}/{}", r.name, r.passed, r.total));
        let kinds: Vec<String> = r.kinds.iter().map(|(k, n)| format!("{{{k}}}x{n}")).collect();
        println!("{}: {}/{}{skip} FAILED {}", r.name, r.passed, r.total, kinds.join(" "));
        print!("{}", r.shown);
    }
    if let Ok(p) = std::env::var("SST_FAILS") {
        let all: Vec<&str> = results.iter().flat_map(|r| r.all_fails.iter().map(String::as_str)).collect();
        std::fs::write(&p, all.join("\n") + "\n").unwrap();
        println!("wrote {} failing tests to {p}", all.len());
    }
    println!("total: {passed}/{total} passed, {skipped} non-normal skipped, {} files", results.len());
    assert!(failing.is_empty(), "failing files: {}", failing.join(", "));
}
