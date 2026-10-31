// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Machine snapshots: format checks, and a determinism test on the real game
//! (skipped when `game/START/STARFLT.COM` is absent).
//!
//! Scratch files go under `$SF_TEST_DIR/snap-tests/` (by default the
//! target directory's tmp dir) and are removed when a test passes.

use flate2::Compression;
use flate2::write::GzEncoder;
use pc::video::Frame;
use pc::{Config, Machine};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

const GAME_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/START");
const GAME_FILES: [&str; 3] = ["STARFLT.COM", "STARA.COM", "STARB.COM"];
/// The files the game rewrites, which a snapshot's slot keeps beside it.
const DATA_FILES: [&str; 2] = ["STARA.COM", "STARB.COM"];

/// A fresh scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let root = std::env::var_os("SF_TEST_DIR").map(PathBuf::from).unwrap_or_else(|| env!("CARGO_TARGET_TMPDIR").into());
    let dir = root.join("snap-tests").join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Copies a file, writable, with the source's modification time (DOS shows
/// it to the game in FCBs and directory entries).
fn copy(src: &Path, dst: &Path) {
    std::fs::copy(src, dst).unwrap();
    let mut perm = std::fs::metadata(dst).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(dst, perm).unwrap();
    let mtime = std::fs::metadata(src).unwrap().modified().unwrap();
    std::fs::File::options().write(true).open(dst).unwrap().set_modified(mtime).unwrap();
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut z = GzEncoder::new(Vec::new(), Compression::default());
    z.write_all(data).unwrap();
    z.finish().unwrap()
}

/// A machine running `jmp $` with interrupts on.
fn spin_machine(dir: &Path) -> Machine {
    Machine::new(&Config::default(), dir.to_path_buf(), &[0xeb, 0xfe])
}

#[test]
fn restore_rejects_other_data_and_keeps_host_side() {
    let dir = scratch("format");
    let mut m = spin_machine(&dir);
    assert_eq!(m.run_for_ms(100.0), None);
    let snap = m.save_state();
    let at = m.now();

    let err = m.restore_state(b"plain bytes").unwrap_err();
    assert!(err.contains("gzip"), "{err}");
    let err = m.restore_state(&gzip(b"NOTSNAP\0\0\0\0")).unwrap_err();
    assert!(err.contains("SFSNAP"), "{err}");
    let mut other = pc::snapshot::MAGIC.to_vec();
    other.extend_from_slice(&99u32.to_le_bytes());
    let err = m.restore_state(&gzip(&other)).unwrap_err();
    assert!(err.contains("version 99"), "{err}");
    let mut truncated = pc::snapshot::MAGIC.to_vec();
    truncated.extend_from_slice(&pc::snapshot::VERSION.to_le_bytes());
    truncated.extend_from_slice(&[0; 100]);
    assert!(m.restore_state(&gzip(&truncated)).unwrap_err().contains("corrupt"));
    assert_eq!(m.now(), at, "a failed restore leaves the machine alone");

    assert_eq!(m.run_for_ms(100.0), None);
    m.file_log = Some(Vec::new());
    m.hook = Some(Box::new(|_, _| {}));
    m.bus.speaker_log = Some(vec![(1, m.bus.speaker_state())]);
    let other_dir = dir.join("elsewhere");
    m.dos.drive_dir = other_dir.clone();
    m.restore_state(&snap).unwrap();
    assert_eq!(m.now(), at);
    assert!(m.file_log.is_some() && m.hook.is_some());
    assert_eq!(m.bus.speaker_log.as_deref(), Some(&[][..]), "recording stays on, emptied");
    assert_eq!(m.dos.drive_dir, other_dir);
    assert_eq!(m.save_state(), snap);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Key events at emulated times (ms): (time, set-1 code).
type Keys = [(f64, u8)];

/// Runs to `to` ms of emulated time, queueing the keys due on the way.
fn run_to(m: &mut Machine, to: f64, keys: &Keys) {
    for &(t, code) in keys.iter().filter(|(t, _)| *t < to) {
        if Machine::master_to_ms(m.now()) <= t {
            assert_eq!(m.run_until(Machine::ms_to_master(t)), None);
            m.key_code(code);
        }
    }
    assert_eq!(m.run_until(Machine::ms_to_master(to)), None);
}

fn screen_hash(m: &Machine) -> u64 {
    let mut frame = Frame::default();
    m.render(&mut frame);
    let mut h = DefaultHasher::new();
    m.bus.ram.hash(&mut h);
    (frame.width, frame.height, &frame.pixels).hash(&mut h);
    h.finish()
}

#[test]
fn restore_replays_identically() {
    let game = Path::new(GAME_DIR);
    if !game.join("STARFLT.COM").is_file() {
        eprintln!("skipping: {} is absent", game.join("STARFLT.COM").display());
        return;
    }
    let dir = scratch("determinism");
    let slot = dir.join("slot");
    let saved = dir.join("saved");
    std::fs::create_dir_all(&slot).unwrap();
    std::fs::create_dir_all(&saved).unwrap();
    for f in GAME_FILES {
        copy(&game.join(f), &slot.join(f));
    }
    let com = std::fs::read(slot.join("STARFLT.COM")).unwrap();

    // 5 (EGA) and Enter at the display menu, then Space during the credits.
    // The game writes STARA once, about 0.3 s after Enter, then reads the
    // credits' data for another 0.4 s or so: the replayed window has file
    // reads and a key press, and no writes (a write would stamp host time
    // into the FCB in guest memory).
    let keys: &Keys = &[(4100.0, 0x06), (4200.0, 0x86), (4500.0, 0x1c), (4600.0, 0x9c), (7000.0, 0x39), (7100.0, 0xb9)];
    const SAVE_MS: f64 = 5000.0;
    const END_MS: f64 = 10_000.0;

    let mut a = Machine::new(&Config::default(), slot.clone(), &com);
    a.file_log = Some(Vec::new());
    run_to(&mut a, SAVE_MS, keys);
    let snap = a.save_state();
    for f in DATA_FILES {
        copy(&slot.join(f), &saved.join(f));
    }
    let before = a.file_log.as_ref().unwrap().len();
    run_to(&mut a, END_MS, keys);
    let log_a = a.file_log.take().unwrap();
    let window = &log_a[before..];
    assert!(window.iter().any(|io| !io.write), "the replayed window reads game files");
    assert!(window.iter().all(|io| !io.write), "the replayed window writes no files");
    eprintln!("snapshot {} bytes; {} file reads after the save point", snap.len(), window.len());
    let hash_a = screen_hash(&a);
    let end_a = a.save_state();
    let files_a: Vec<Vec<u8>> = DATA_FILES.iter().map(|f| std::fs::read(slot.join(f)).unwrap()).collect();
    drop(a);

    for f in DATA_FILES {
        copy(&saved.join(f), &slot.join(f));
    }
    let mut b = Machine::new(&Config::default(), slot.clone(), &com);
    b.restore_state(&snap).unwrap();
    assert_eq!(b.dos.drive_dir, slot);
    b.file_log = Some(Vec::new());
    run_to(&mut b, END_MS, keys);
    let log_b = b.file_log.take().unwrap();
    assert_eq!(window, &log_b[..], "the same file transfers after the save point");
    assert_eq!(screen_hash(&b), hash_a, "RAM and picture after replay");
    assert!(end_a == b.save_state(), "machine state after replay");
    let files_b: Vec<Vec<u8>> = DATA_FILES.iter().map(|f| std::fs::read(slot.join(f)).unwrap()).collect();
    assert!(files_a == files_b, "slot files after replay");
    std::fs::remove_dir_all(&dir).unwrap();
}
