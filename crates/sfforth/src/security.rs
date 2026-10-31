// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! The launch security check (the code wheel), as overlay STP-OV implements
//! it while resident at 0xEFC0.
//!
//! Three variables hold the challenge: a planet (0-23), an artifact (0-23)
//! and a race (0-8). Word F046 computes the answer:
//!
//! ```text
//! d = (planet - artifact) mod 24
//! code = (d*(d+1) + race*(race-1) + 37) mod 307
//! ```
//!
//! and F48D accepts a typed number congruent to it mod 307. Read from the
//! overlay with this project's decompiler.
//!
//! While the prompt waits for typing, another overlay occupies the same
//! address range, so the challenge must be read while STP-OV is resident:
//! when its name printer (word F2E5) runs.

use crate::rd16;

/// The word that prints the challenge; it runs with STP-OV resident.
pub const NAME_PRINTER: u16 = 0xf2e5;
/// Start of word F046 (DOCOL, F030 @ F034 @ 2DUP < 0BRANCH), identifying STP-OV.
/// MISC-OV has the same size and load address.
const SIGNATURE_AT: u16 = 0xf046;
const SIGNATURE: [u8; 16] =
    [0x4c, 0x22, 0x30, 0xf0, 0xae, 0x0b, 0x34, 0xf0, 0xae, 0x0b, 0xf0, 0x0d, 0x46, 0x12, 0xfa, 0x15];
/// Values of the three challenge variables (their CFA + 2).
const PLANET: u16 = 0xf032;
const ARTIFACT: u16 = 0xf036;
const RACE: u16 = 0xf03a;
/// Tables of name words: planet, race (CFA + 2 of the table words).
const PLANET_NAMES: u16 = 0xf29b;
const RACE_NAMES: u16 = 0xf287;
/// Artifact index to record number in the ARTIFACT file (FILE# 28).
pub const ARTIFACT_RECORDS: u16 = 0xf2cd;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    pub planet: u16,
    pub artifact: u16,
    pub race: u16,
    pub planet_name: String,
    pub race_name: String,
    /// Record number of the artifact in the ARTIFACT file.
    pub artifact_record: u8,
    pub code: u16,
}

pub fn code(planet: u16, artifact: u16, race: u16) -> u16 {
    let d = (planet as i32 - artifact as i32).rem_euclid(24);
    let r = race as i32;
    ((d * (d + 1) + r * (r - 1) + 37).rem_euclid(307)) as u16
}

/// A name word's counted string (code field, count byte, characters).
fn name_at(seg: &[u8], word: u16) -> String {
    let len = seg[word.wrapping_add(2) as usize] as u16;
    (0..len.min(32)).map(|i| seg[word.wrapping_add(3 + i) as usize] as char).collect()
}

/// The challenge in STP-OV's variables, if STP-OV is resident and they are in range.
pub fn read(seg: &[u8]) -> Option<Challenge> {
    let at = SIGNATURE_AT as usize;
    if seg[at..at + SIGNATURE.len()] != SIGNATURE {
        return None;
    }
    let (planet, artifact, race) = (rd16(seg, PLANET), rd16(seg, ARTIFACT), rd16(seg, RACE));
    if planet > 23 || artifact > 23 || race > 8 {
        return None;
    }
    Some(Challenge {
        planet,
        artifact,
        race,
        planet_name: name_at(seg, rd16(seg, PLANET_NAMES + 2 * planet)),
        race_name: name_at(seg, rd16(seg, RACE_NAMES + 2 * race)),
        artifact_record: seg[(ARTIFACT_RECORDS + artifact) as usize],
        code: code(planet, artifact, race),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_answer() {
        // AKTERON / AMAZING ARTIFACT / GAZURTOID, accepted by the game.
        assert_eq!(code(12, 19, 5), 56);
    }
}
