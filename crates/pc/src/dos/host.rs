// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! The host directory behind drive C:: 8.3 names for its entries, and the
//! host files the program has open.
//!
//! Files are only ever reached through a listing of the directory, matched
//! by their 8.3 names, so FCB bytes never become path components. Symlinks
//! and other special entries are left out of listings, so nothing outside
//! the directory can be reached.

use super::clock::LocalTime;
use super::fcb::is_terminator;
use std::collections::HashMap;
use std::collections::hash_map::Entry as Slot;
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// An FCB-style name: 8 + 3 bytes, upper case, blank padded.
pub(super) type Name = [u8; 11];

pub(super) const READ_ONLY: u8 = 0x01;
pub(super) const HIDDEN: u8 = 0x02;
pub(super) const SYSTEM: u8 = 0x04;
pub(super) const VOLUME: u8 = 0x08;
pub(super) const DIRECTORY: u8 = 0x10;
pub(super) const ARCHIVE: u8 = 0x20;

pub(super) struct Entry {
    pub name: Name,
    pub path: PathBuf,
    pub attr: u8,
    pub size: u64,
    pub mtime: SystemTime,
}

impl Entry {
    /// The 32-byte FAT directory entry (no cluster number).
    pub fn dir_entry(&self) -> [u8; 32] {
        let mut d = [0u8; 32];
        d[..11].copy_from_slice(&self.name);
        d[11] = self.attr;
        let t = LocalTime::of(self.mtime);
        d[0x16..0x18].copy_from_slice(&t.dos_time().to_le_bytes());
        d[0x18..0x1a].copy_from_slice(&t.dos_date().to_le_bytes());
        d[0x1c..0x20].copy_from_slice(&(self.size.min(u32::MAX as u64) as u32).to_le_bytes());
        d
    }
}

/// Characters a host name may use to have an 8.3 name.
fn name_char(c: u8) -> bool {
    c.is_ascii_graphic() && !is_terminator(c) && c != b'?' && c != b'*'
}

/// The 8.3 name of a host entry, if it has one: ASCII, at most one dot,
/// 1-8 + 0-3 characters DOS allows in names. Case is folded.
pub(super) fn dos_name(host: &str) -> Option<Name> {
    let (base, ext) = match host.split_once('.') {
        Some((b, e)) if !e.is_empty() => (b, e),
        Some(_) => return None,
        None => (host, ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return None;
    }
    let mut n = [b' '; 11];
    let (nb, ne) = n.split_at_mut(8);
    for (dst, c) in nb.iter_mut().zip(base.bytes()).chain(ne.iter_mut().zip(ext.bytes())) {
        if !name_char(c) {
            return None;
        }
        *dst = c.to_ascii_uppercase();
    }
    Some(n)
}

/// "NAME.EXT", or "NAME" with a blank extension.
pub(super) fn display(name: &Name) -> String {
    let base = String::from_utf8_lossy(name[..8].trim_ascii_end());
    let ext = String::from_utf8_lossy(name[8..].trim_ascii_end());
    if ext.is_empty() { base.into_owned() } else { format!("{base}.{ext}") }
}

/// `?` in the pattern matches any character, blanks included.
pub(super) fn matches(pattern: &Name, name: &Name) -> bool {
    pattern.iter().zip(name).all(|(&p, &n)| p == b'?' || p == n)
}

/// Search attribute rules (Search, MS-DOS 4.0 DIR.ASM): read-only and archive
/// never matter; hidden, system and directory entries need their bit in the
/// search attribute; a search for the volume label alone finds only labels,
/// and this drive has none.
pub(super) fn attr_matches(search: u8, entry: u8) -> bool {
    let s = search & !(READ_ONLY | ARCHIVE);
    s != VOLUME && entry & (HIDDEN | SYSTEM | DIRECTORY) & !s == 0
}

/// Entries of `root` that have 8.3 names, sorted by name (then host name, so
/// of two names differing only in case the same one always wins).
pub(super) fn list(root: &Path) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for de in std::fs::read_dir(root)? {
        let de = de?;
        let ty = de.file_type()?;
        if !ty.is_file() && !ty.is_dir() {
            continue;
        }
        let Some(name) = de.file_name().to_str().and_then(dos_name) else { continue };
        let meta = de.metadata()?;
        let attr = if ty.is_dir() {
            DIRECTORY
        } else if meta.permissions().readonly() {
            ARCHIVE | READ_ONLY
        } else {
            ARCHIVE
        };
        out.push(Entry { name, path: de.path(), attr, size: meta.len(), mtime: meta.modified().unwrap_or(UNIX_EPOCH) });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    Ok(out)
}

/// The first regular file matching `pattern`.
fn find_file(root: &Path, pattern: &Name) -> io::Result<Option<Entry>> {
    Ok(list(root)?.into_iter().find(|e| e.attr & DIRECTORY == 0 && matches(pattern, &e.name)))
}

/// Host files the program has open, by 8.3 name.
#[derive(Default)]
pub(super) struct OpenFiles {
    files: HashMap<Name, File>,
}

impl OpenFiles {
    /// The host file for an FCB name (`?` takes the first match) and its
    /// actual name, opening it if needed; None if there is no such file.
    /// Opened read-write, or read-only if the host file does not allow
    /// writing (writes then fail on the host).
    pub fn get(&mut self, root: &Path, name: &Name) -> io::Result<Option<(&mut File, Name)>> {
        let key = if !name.contains(&b'?') && self.files.contains_key(name) {
            *name
        } else {
            let Some(e) = find_file(root, name)? else { return Ok(None) };
            if let Slot::Vacant(v) = self.files.entry(e.name) {
                v.insert(match OpenOptions::new().read(true).write(true).open(&e.path) {
                    Err(err) if err.kind() == ErrorKind::PermissionDenied => File::open(&e.path)?,
                    r => r?,
                });
            }
            e.name
        };
        Ok(self.files.get_mut(&key).map(|f| (f, key)))
    }

    /// Forgets the open file for an FCB name.
    pub fn close(&mut self, root: &Path, name: &Name) -> io::Result<()> {
        if name.contains(&b'?') {
            if let Some(e) = find_file(root, name)? {
                self.files.remove(&e.name);
            }
        } else {
            self.files.remove(name);
        }
        Ok(())
    }
}

/// Up to `len` bytes at `pos`; fewer at the end of the file.
pub(super) fn read_at(f: &mut File, pos: u64, len: u32) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len as usize];
    f.seek(SeekFrom::Start(pos))?;
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    buf.truncate(got);
    Ok(buf)
}

/// Writes through to the host file at once, so nothing is lost if the
/// emulator stops.
pub(super) fn write_at(f: &mut File, pos: u64, data: &[u8]) -> io::Result<()> {
    f.seek(SeekFrom::Start(pos))?;
    f.write_all(data)
}
