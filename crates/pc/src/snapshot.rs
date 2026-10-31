// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Machine snapshots: a gzip stream of a header (`MAGIC`, then `VERSION` as
//! a little-endian u32) and a bincode body holding the `Machine` as serde
//! sees it.
//!
//! Fields marked `#[serde(skip)]` are host-side plumbing or diagnostics and
//! are not in the body. A restore keeps the current machine's values for
//! them: the hook, the file log, the DOS drive directory and logging switch,
//! the absent-port counts, and speaker recording (on or off, emptied). Open
//! host files are dropped; DOS reopens them by name. Any new machine state
//! must be serialisable and not skipped, or it is lost on restore.

use crate::Machine;
use bincode::Options;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use std::io::{Read, Write};

pub const MAGIC: &[u8; 6] = b"SFSNAP";
/// Changes whenever the serialised types change.
pub const VERSION: u32 = 1;
const HEADER_LEN: usize = MAGIC.len() + 4;
/// Largest uncompressed snapshot accepted; real ones are about 1.4 MB.
const MAX_LEN: u64 = 64 << 20;
const RAM_LEN: usize = 1 << 20;

/// The one bincode configuration used in both directions.
fn options() -> impl Options {
    bincode::DefaultOptions::new().with_fixint_encoding().with_limit(MAX_LEN)
}

impl Machine {
    /// The machine's state as a compressed snapshot.
    pub fn save_state(&self) -> Vec<u8> {
        let body = options().serialize(self).expect("machine state serialises");
        let mut z = GzEncoder::new(Vec::with_capacity(body.len() / 4), Compression::default());
        z.write_all(MAGIC).expect("writing to memory");
        z.write_all(&VERSION.to_le_bytes()).expect("writing to memory");
        z.write_all(&body).expect("writing to memory");
        z.finish().expect("writing to memory")
    }

    /// Replaces the machine's state with a snapshot's. On error the machine
    /// is unchanged.
    pub fn restore_state(&mut self, data: &[u8]) -> Result<(), String> {
        let mut raw = Vec::new();
        GzDecoder::new(data)
            .take(MAX_LEN + 1)
            .read_to_end(&mut raw)
            .map_err(|e| format!("not a machine snapshot (gzip: {e})"))?;
        if raw.len() as u64 > MAX_LEN {
            return Err(format!("not a machine snapshot (over {MAX_LEN} bytes uncompressed)"));
        }
        if raw.len() < HEADER_LEN || &raw[..MAGIC.len()] != MAGIC {
            return Err("not a machine snapshot (no SFSNAP header)".to_string());
        }
        let version = u32::from_le_bytes(raw[MAGIC.len()..HEADER_LEN].try_into().unwrap());
        if version != VERSION {
            return Err(format!("snapshot format version {version}; this build reads version {VERSION}"));
        }
        let mut m: Machine = options().deserialize(&raw[HEADER_LEN..]).map_err(|e| format!("corrupt snapshot: {e}"))?;
        if m.bus.ram.len() != RAM_LEN || !m.bus.video.sizes_ok() {
            return Err("corrupt snapshot: memory of the wrong size".to_string());
        }
        m.hook = self.hook.take();
        m.file_log = self.file_log.take();
        m.dos.drive_dir = std::mem::take(&mut self.dos.drive_dir);
        m.dos.log_io = self.dos.log_io;
        m.bus.absent_ports = std::mem::take(&mut self.bus.absent_ports);
        m.bus.speaker_log = self.bus.speaker_log.as_ref().map(|_| Vec::new());
        *self = m;
        Ok(())
    }
}
