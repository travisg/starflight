// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Save slots: directories holding working copies of the game files, which
//! the game reads and rewrites as it did on DOS. The original game files are
//! only ever copied from, never opened for writing.
//!
//! Layout: `<saves>/<slot>/{STARFLT,STARA,STARB}.COM`, and
//! `<saves>/<slot>/backups/<timestamp>/` holding earlier copies (the
//! equivalent of the game's REMEMBER/GOBACK batch files).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub const GAME_FILES: [&str; 3] = ["STARFLT.COM", "STARA.COM", "STARB.COM"];
/// Backups kept per slot; older ones are removed.
const KEEP_BACKUPS: usize = 20;

pub struct Saves {
    pub root: PathBuf,
}

impl Saves {
    pub fn new(root: PathBuf) -> Self {
        Saves { root }
    }

    pub fn slot_dir(&self, name: &str) -> Result<PathBuf> {
        if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
            bail!("invalid save name {name:?}");
        }
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

    /// Copies the slot's game files into a new timestamped backup.
    pub fn backup(&self, name: &str) -> Result<PathBuf> {
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
            copy_writable(&dir.join(f), &dest.join(f))?;
        }
        self.prune_backups(name)?;
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

    fn prune_backups(&self, name: &str) -> Result<()> {
        let all = self.backups(name)?;
        let dir = self.slot_dir(name)?.join("backups");
        for old in all.iter().take(all.len().saturating_sub(KEEP_BACKUPS)) {
            std::fs::remove_dir_all(dir.join(old))?;
        }
        Ok(())
    }

    /// Puts a backup's files back into the slot (backing up the current ones first).
    pub fn restore(&self, name: &str, backup: &str) -> Result<()> {
        let dir = self.slot_dir(name)?;
        let src = dir.join("backups").join(backup);
        if !src.join("STARA.COM").is_file() {
            bail!("no backup {backup:?} in save {name:?}; see `starflight saves {name}`");
        }
        self.backup(name)?;
        for f in GAME_FILES {
            copy_writable(&src.join(f), &dir.join(f))?;
        }
        Ok(())
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

/// UTC timestamp for backup names, e.g. 20261030-231502.
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem / 60 % 60, rem % 60)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_756), (2026, 10, 30));
    }
}
