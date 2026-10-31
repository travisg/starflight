// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Save slots: directories holding working copies of the game files, which
//! the game reads and rewrites as it did on DOS. The original game files are
//! only ever copied from, never opened for writing.
//!
//! Layout: `<saves>/<slot>/{STARFLT,STARA,STARB}.COM`,
//! `<saves>/<slot>/backups/<timestamp>/` holding earlier copies (the
//! equivalent of the game's REMEMBER/GOBACK batch files), and
//! `<saves>/<slot>/states/<name>/` holding emulator snapshots: the machine
//! (`machine.bin.gz`), the STARA/STARB.COM the game had at that moment, and
//! `info.txt`.

use anyhow::{Context, Result, anyhow, bail};
use pc::Machine;
use sfforth::consts::STARFLT_COM_SIZE;
use std::path::{Path, PathBuf};

pub const GAME_FILES: [&str; 3] = ["STARFLT.COM", "STARA.COM", "STARB.COM"];
/// Backups kept per slot; older ones are removed.
const KEEP_BACKUPS: usize = 20;
/// The game files a state keeps: the ones the game rewrites.
const STATE_FILES: [&str; 2] = ["STARA.COM", "STARB.COM"];
const STATE_MACHINE: &str = "machine.bin.gz";
const STATE_INFO: &str = "info.txt";

/// A state as `info.txt` describes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StateInfo {
    pub name: String,
    /// `YYYY-MM-DD HH:MM:SS UTC`; sorts chronologically.
    pub created: String,
    pub emulated_s: f64,
    pub instructions: u64,
    pub description: Option<String>,
}

impl StateInfo {
    fn to_text(&self) -> String {
        let mut t = format!(
            "created: {}\nemulated: {:.3} s\ninstructions: {}\n",
            self.created, self.emulated_s, self.instructions
        );
        if let Some(d) = &self.description {
            t.push_str(&format!("description: {d}\n"));
        }
        t
    }

    /// Reads the fields it knows; others and malformed values are left at
    /// their defaults.
    fn parse(name: &str, text: &str) -> StateInfo {
        let mut info = StateInfo { name: name.to_string(), ..Default::default() };
        for line in text.lines() {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "created" => info.created = v.to_string(),
                "emulated" => info.emulated_s = v.trim_end_matches('s').trim().parse().unwrap_or(0.0),
                "instructions" => info.instructions = v.parse().unwrap_or(0),
                "description" => info.description = Some(v.to_string()),
                _ => {}
            }
        }
        info
    }
}

/// A state load that failed partway and could not put the slot's files back:
/// they no longer match the machine.
#[derive(Debug)]
pub struct SlotInconsistent {
    pub slot: String,
    /// The slot's files from before the load.
    pub backup: PathBuf,
    pub cause: String,
}

impl std::fmt::Display for SlotInconsistent {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "save {:?} no longer matches the running game ({}); copy STARA.COM and STARB.COM back from {}",
            self.slot,
            self.cause,
            self.backup.display()
        )
    }
}

impl std::error::Error for SlotInconsistent {}

pub struct Saves {
    pub root: PathBuf,
}

impl Saves {
    pub fn new(root: PathBuf) -> Self {
        Saves { root }
    }

    pub fn slot_dir(&self, name: &str) -> Result<PathBuf> {
        check_name("save", name)?;
        Ok(self.root.join(name))
    }

    pub fn exists(&self, name: &str) -> bool {
        self.slot_dir(name).map(|d| d.join("STARA.COM").exists()).unwrap_or(false)
    }

    /// Creates (or with `replace`, overwrites) a slot from a game directory.
    pub fn create(&self, name: &str, from: &Path, replace: bool) -> Result<PathBuf> {
        let dir = self.slot_dir(name)?;
        if self.exists(name) && !replace {
            bail!("save {name:?} already exists at {} (use --replace to overwrite it)", dir.display());
        }
        for f in GAME_FILES {
            let src = from.join(f);
            if !src.is_file() {
                bail!("{} is missing; {} does not look like a Starflight directory", src.display(), from.display());
            }
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        if same_dir(&dir, from) {
            bail!("refusing to use {} as both source and save slot", dir.display());
        }
        for f in GAME_FILES {
            copy_writable(&from.join(f), &dir.join(f))?;
        }
        Ok(dir)
    }

    /// Copies the slot's game files, with their times, into a new timestamped
    /// backup.
    pub fn backup(&self, name: &str) -> Result<PathBuf> {
        self.backup_keeping(name, None)
    }

    /// `backup`, never pruning the backup named `keep`.
    fn backup_keeping(&self, name: &str, keep: Option<&str>) -> Result<PathBuf> {
        let dir = self.slot_dir(name)?;
        let stamp = timestamp();
        let mut dest = dir.join("backups").join(&stamp);
        let mut n = 1;
        while dest.exists() {
            dest = dir.join("backups").join(format!("{stamp}-{n}"));
            n += 1;
        }
        std::fs::create_dir_all(&dest)?;
        for f in GAME_FILES {
            copy_with_mtime(&dir.join(f), &dest.join(f))?;
        }
        self.prune_backups(name, keep)?;
        Ok(dest)
    }

    pub fn backups(&self, name: &str) -> Result<Vec<String>> {
        let dir = self.slot_dir(name)?.join("backups");
        let mut v: Vec<String> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
            Err(_) => Vec::new(),
        };
        v.sort();
        Ok(v)
    }

    /// Removes the oldest backups beyond `KEEP_BACKUPS`, skipping `keep`.
    fn prune_backups(&self, name: &str, keep: Option<&str>) -> Result<()> {
        let all = self.backups(name)?;
        let dir = self.slot_dir(name)?.join("backups");
        let excess = all.len().saturating_sub(KEEP_BACKUPS);
        for old in all.iter().filter(|b| Some(b.as_str()) != keep).take(excess) {
            std::fs::remove_dir_all(dir.join(old))?;
        }
        Ok(())
    }

    /// Puts a backup's files back into the slot (backing up the current ones
    /// first). The backup itself is kept.
    pub fn restore(&self, name: &str, backup: &str) -> Result<()> {
        let dir = self.slot_dir(name)?;
        let src = dir.join("backups").join(backup);
        if !src.join("STARA.COM").is_file() {
            bail!("no backup {backup:?} in save {name:?}; see `starflight saves {name}`");
        }
        self.backup_keeping(name, Some(backup))?;
        for f in GAME_FILES {
            copy_writable(&src.join(f), &dir.join(f))?;
        }
        Ok(())
    }

    fn states_dir(&self, slot: &str) -> Result<PathBuf> {
        Ok(self.slot_dir(slot)?.join("states"))
    }

    /// Fails unless `name` is a valid state name not yet used in `slot`.
    pub fn check_new_state(&self, slot: &str, name: &str) -> Result<()> {
        check_name("state", name)?;
        let dir = self.states_dir(slot)?.join(name);
        if dir.exists() {
            bail!("save {slot:?} already has a state {name:?}; remove {} to reuse the name", dir.display());
        }
        Ok(())
    }

    /// Saves the machine running `slot` and the slot's STARA/STARB as a new
    /// state, named `name` or `quick-<timestamp>`. Returns the name.
    pub fn save_state(&self, slot: &str, name: Option<&str>, m: &Machine, description: Option<&str>) -> Result<String> {
        let slot_dir = self.slot_dir(slot)?;
        if !self.exists(slot) {
            bail!("no save {slot:?}");
        }
        check_running(m, &slot_dir, slot)?;
        let states = self.states_dir(slot)?;
        let name = match name {
            Some(n) => {
                self.check_new_state(slot, n)?;
                n.to_string()
            }
            None => {
                let base = format!("quick-{}", timestamp());
                let mut n = base.clone();
                let mut i = 1;
                while states.join(&n).exists() {
                    n = format!("{base}-{i}");
                    i += 1;
                }
                n
            }
        };
        // Built under a hidden name (which listings skip), then renamed.
        let tmp = states.join(format!(".{name}.tmp"));
        if tmp.exists() {
            std::fs::remove_dir_all(&tmp).with_context(|| format!("removing {}", tmp.display()))?;
        }
        std::fs::create_dir_all(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        std::fs::write(tmp.join(STATE_MACHINE), m.save_state())?;
        for f in STATE_FILES {
            copy_with_mtime(&slot_dir.join(f), &tmp.join(f))?;
        }
        let info = StateInfo {
            name: name.clone(),
            created: created_stamp(),
            emulated_s: Machine::master_to_ms(m.now()) / 1000.0,
            instructions: m.instructions,
            description: description.map(|d| d.split_whitespace().collect::<Vec<_>>().join(" ")),
        };
        std::fs::write(tmp.join(STATE_INFO), info.to_text())?;
        let dest = states.join(&name);
        std::fs::rename(&tmp, &dest).with_context(|| format!("renaming {} to {}", tmp.display(), dest.display()))?;
        Ok(name)
    }

    /// Restores state `name` of `slot` into `m`, which must be running that
    /// slot: backs up the slot's files, puts the state's STARA/STARB in
    /// their place and restores the machine. Returns the backup's directory.
    /// On error the machine and the slot's files are as they were, unless
    /// putting the files back fails too: then the error is a
    /// `SlotInconsistent`, and the machine must not run on.
    pub fn load_state(&self, slot: &str, name: &str, m: &mut Machine) -> Result<PathBuf> {
        let slot_dir = self.slot_dir(slot)?;
        check_name("state", name)?;
        let dir = self.states_dir(slot)?.join(name);
        if !dir.join(STATE_MACHINE).is_file() {
            bail!("no state {name:?} in save {slot:?}; see `starflight states {slot}`");
        }
        for f in STATE_FILES {
            if !dir.join(f).is_file() {
                bail!("state {name:?} of save {slot:?} lacks {f}");
            }
        }
        let com = slot_dir.join("STARFLT.COM");
        let len = std::fs::metadata(&com).with_context(|| format!("reading {}", com.display()))?.len();
        if len != STARFLT_COM_SIZE as u64 {
            bail!("{} is {len} bytes, not the known {STARFLT_COM_SIZE}-byte build", com.display());
        }
        check_running(m, &slot_dir, slot)?;
        let data = std::fs::read(dir.join(STATE_MACHINE))?;
        let before = m.save_state();
        m.restore_state(&data).map_err(|e| anyhow!("state {name:?} of save {slot:?}: {e}"))?;
        let undo = |m: &mut Machine| m.restore_state(&before).expect("restoring the machine's own snapshot");
        let backup = self.backup(slot).inspect_err(|_| undo(m))?;
        let Err(e) = STATE_FILES.iter().try_for_each(|f| copy_with_mtime(&dir.join(f), &slot_dir.join(f))) else {
            return Ok(backup);
        };
        undo(m);
        match STATE_FILES.iter().try_for_each(|f| put_back(&backup.join(f), &slot_dir.join(f))) {
            Ok(()) => Err(e.context(format!("loading state {name:?} of save {slot:?}; the slot is as it was"))),
            Err(r) => Err(SlotInconsistent { slot: slot.to_string(), backup, cause: format!("{e:#}; {r:#}") }.into()),
        }
    }

    /// A slot's states, oldest first.
    pub fn states(&self, slot: &str) -> Result<Vec<StateInfo>> {
        let dir = self.states_dir(slot)?;
        let mut v: Vec<StateInfo> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| e.path().join(STATE_MACHINE).is_file())
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| !n.starts_with('.'))
                .map(|n| {
                    let text = std::fs::read_to_string(dir.join(&n).join(STATE_INFO)).unwrap_or_default();
                    StateInfo::parse(&n, &text)
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        v.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.name.cmp(&b.name)));
        Ok(v)
    }

    pub fn list(&self) -> Vec<String> {
        let mut v: Vec<String> = match std::fs::read_dir(&self.root) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| e.path().join("STARA.COM").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        };
        v.sort();
        v
    }
}

fn check_name(what: &str, name: &str) -> Result<()> {
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        bail!("invalid {what} name {name:?}");
    }
    Ok(())
}

/// A state belongs to the slot whose files the machine's DOS uses.
fn check_running(m: &Machine, slot_dir: &Path, slot: &str) -> Result<()> {
    if !same_dir(&m.dos.drive_dir, slot_dir) {
        bail!("the machine runs {}, not save {slot:?}", m.dos.drive_dir.display());
    }
    Ok(())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Copies a file and makes the copy writable (the originals may be read-only).
fn copy_writable(src: &Path, dst: &Path) -> Result<()> {
    std::fs::copy(src, dst).with_context(|| format!("copying {} to {}", src.display(), dst.display()))?;
    let mut perm = std::fs::metadata(dst)?.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(dst, perm)?;
    Ok(())
}

/// Copies a file writable, keeping its modification time: DOS shows it to
/// the game in FCBs and directory entries, so a restored state sees the
/// times it saw when saved.
fn copy_with_mtime(src: &Path, dst: &Path) -> Result<()> {
    copy_writable(src, dst)?;
    let mtime = std::fs::metadata(src)?.modified()?;
    std::fs::File::options()
        .write(true)
        .open(dst)
        .and_then(|f| f.set_modified(mtime))
        .with_context(|| format!("setting the time of {}", dst.display()))
}

/// `copy_with_mtime`, skipped when `dst` already has `src`'s contents and
/// time (it may be a file that could not be written at all).
fn put_back(src: &Path, dst: &Path) -> Result<()> {
    let same = || -> Option<bool> {
        let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        Some(mtime(src)? == mtime(dst)? && std::fs::read(src).ok()? == std::fs::read(dst).ok()?)
    };
    if same() == Some(true) {
        return Ok(());
    }
    copy_with_mtime(src, dst)
}

/// The current UTC time as (year, month, day, hour, minute, second).
fn utc_now() -> (i64, u32, u32, u64, u64, u64) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    (y, m, d, rem / 3600, rem / 60 % 60, rem % 60)
}

/// UTC timestamp for backup and quick state names, e.g. 20261030-231502.
fn timestamp() -> String {
    let (y, m, d, h, min, s) = utc_now();
    format!("{y:04}{m:02}{d:02}-{h:02}{min:02}{s:02}")
}

/// UTC time for `info.txt`, e.g. 2026-10-30 23:15:02 UTC.
fn created_stamp() -> String {
    let (y, m, d, h, min, s) = utc_now();
    format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02}:{s:02} UTC")
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

/// A fresh scratch directory under `$SF_TEST_DIR/snap-tests/` (by default
/// the system temp dir).
#[cfg(test)]
pub(crate) fn scratch_dir(name: &str) -> PathBuf {
    let root = std::env::var_os("SF_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = root.join("snap-tests").join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc::Config;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_756), (2026, 10, 30));
    }

    #[test]
    fn info_text() {
        let info = StateInfo {
            name: "x".into(),
            created: "2026-10-30 23:15:02 UTC".into(),
            emulated_s: 12.5,
            instructions: 42,
            description: Some("a: b".into()),
        };
        assert_eq!(StateInfo::parse("x", &info.to_text()), info);
        assert_eq!(StateInfo::parse("y", "junk\nemulated: nope\n").name, "y");
    }

    /// A slot whose STARFLT.COM has the known size, and a machine running
    /// `jmp $` on it.
    fn fake_slot(saves: &Saves, dir: &Path) -> Machine {
        let game = dir.join("game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("STARFLT.COM"), vec![0u8; STARFLT_COM_SIZE]).unwrap();
        std::fs::write(game.join("STARA.COM"), b"A at save").unwrap();
        std::fs::write(game.join("STARB.COM"), b"B at save").unwrap();
        let slot = saves.create("s", &game, false).unwrap();
        Machine::new(&Config::default(), slot, &[0xeb, 0xfe])
    }

    #[test]
    fn states_save_list_and_load() {
        let dir = scratch_dir("states");
        let saves = Saves::new(dir.join("saves"));
        let mut m = fake_slot(&saves, &dir);
        let slot = saves.slot_dir("s").unwrap();
        assert_eq!(m.run_for_ms(50.0), None);
        let at = m.now();

        assert_eq!(saves.save_state("s", Some("one"), &m, Some("first\nline")).unwrap(), "one");
        assert!(saves.check_new_state("s", "one").is_err());
        assert!(saves.save_state("s", Some("one"), &m, None).is_err());
        assert!(saves.save_state("s", Some(".hidden"), &m, None).is_err());
        let quick = saves.save_state("s", None, &m, None).unwrap();
        assert!(quick.starts_with("quick-"), "{quick}");
        let states = saves.states("s").unwrap();
        let mut names: Vec<&str> = states.iter().map(|s| s.name.as_str()).collect();
        names.sort();
        assert_eq!(names, ["one", quick.as_str()]);
        let one = states.iter().find(|s| s.name == "one").unwrap();
        assert_eq!(one.description.as_deref(), Some("first line"));
        assert_eq!(one.instructions, m.instructions);

        // The game moves on and rewrites STARA; loading puts both back.
        std::fs::write(slot.join("STARA.COM"), b"A later").unwrap();
        assert_eq!(m.run_for_ms(50.0), None);
        let backup = saves.load_state("s", "one", &mut m).unwrap();
        assert_eq!(m.now(), at);
        assert_eq!(m.dos.drive_dir, slot);
        assert_eq!(std::fs::read(slot.join("STARA.COM")).unwrap(), b"A at save");
        assert_eq!(std::fs::read(backup.join("STARA.COM")).unwrap(), b"A later");
        let mtime = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();
        let state = slot.join("states/one");
        assert_eq!(mtime(&slot.join("STARA.COM")), mtime(&state.join("STARA.COM")));
        assert!(!std::fs::metadata(slot.join("STARB.COM")).unwrap().permissions().readonly());

        assert!(saves.load_state("s", "two", &mut m).unwrap_err().to_string().contains("no state"));
        let backups = saves.backups("s").unwrap().len();
        std::fs::write(state.join(STATE_MACHINE), b"not a snapshot").unwrap();
        assert!(saves.load_state("s", "one", &mut m).is_err());
        assert_eq!(saves.backups("s").unwrap().len(), backups, "nothing is touched for a bad snapshot");
        assert_eq!(m.now(), at);

        let elsewhere = Machine::new(&Config::default(), dir.clone(), &[0xeb, 0xfe]);
        assert!(saves.save_state("s", Some("x"), &elsewhere, None).unwrap_err().to_string().contains("runs"));

        std::fs::write(slot.join("STARFLT.COM"), b"short").unwrap();
        assert!(saves.load_state("s", &quick, &mut m).unwrap_err().to_string().contains("54183"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn restoring_the_oldest_backup_keeps_it() {
        let dir = scratch_dir("restore-oldest");
        let saves = Saves::new(dir.join("saves"));
        fake_slot(&saves, &dir);
        let slot = saves.slot_dir("s").unwrap();
        let names: Vec<String> = (0..KEEP_BACKUPS).map(|i| format!("20260101-0000{i:02}")).collect();
        for n in &names {
            let b = slot.join("backups").join(n);
            std::fs::create_dir_all(&b).unwrap();
            for f in GAME_FILES {
                std::fs::write(b.join(f), format!("{f} {n}")).unwrap();
            }
        }
        saves.restore("s", &names[0]).unwrap();
        assert_eq!(std::fs::read_to_string(slot.join("STARA.COM")).unwrap(), format!("STARA.COM {}", names[0]));
        let after = saves.backups("s").unwrap();
        assert_eq!(after.len(), KEEP_BACKUPS);
        assert!(after.contains(&names[0]), "the restored backup is kept");
        assert!(!after.contains(&names[1]), "the next oldest goes instead");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_state_load_puts_the_files_back() {
        let dir = scratch_dir("failed-load");
        let saves = Saves::new(dir.join("saves"));
        let mut m = fake_slot(&saves, &dir);
        let slot = saves.slot_dir("s").unwrap();
        saves.save_state("s", Some("one"), &m, None).unwrap();
        assert_eq!(m.run_for_ms(50.0), None);
        let at = m.now();

        let stara = slot.join("STARA.COM");
        let starb = slot.join("STARB.COM");
        std::fs::write(&stara, b"A later").unwrap();
        let then = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        std::fs::File::options().write(true).open(&stara).unwrap().set_modified(then).unwrap();
        let mut perm = std::fs::metadata(&starb).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&starb, perm).unwrap();
        if std::fs::File::options().write(true).open(&starb).is_ok() {
            eprintln!("skipping: read-only files are writable here (root?)");
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }

        // STARA is replaced, then STARB cannot be written.
        let e = saves.load_state("s", "one", &mut m).unwrap_err();
        assert!(e.downcast_ref::<SlotInconsistent>().is_none(), "{e:#}");
        assert!(e.to_string().contains("as it was"), "{e:#}");
        assert_eq!(m.now(), at);
        assert_eq!(std::fs::read(&stara).unwrap(), b"A later");
        assert_eq!(std::fs::metadata(&stara).unwrap().modified().unwrap(), then);
        assert_eq!(std::fs::read(&starb).unwrap(), b"B at save");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
