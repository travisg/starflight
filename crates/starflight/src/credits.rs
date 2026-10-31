// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! Credits shown by `starflight credits` and the startup banner. Keep in step
//! with CREDITS.md.

pub const BANNER: &str = "Starflight (1986) by Binary Systems, published by Electronic Arts. \
Emulator built on reverse engineering by Sebastian Macke and Jeremy Penner; see `starflight credits`.";

pub const CREDITS: &str = "\
Starflight (1986) was created by Binary Systems and published by Electronic Arts:
  Rod McConnell (founder), Greg Johnson (lead designer, artist, story),
  Tim Lee (low-level architecture, planet rendering), Alec Kercso (starbase),
  Bob Gonsalves (planetside), Dave Boulton (fractal-planet concept),
  with Paul Reiche III (conversation engine) and Joe Ybarra (EA producer).
  Team roles per The Digital Antiquarian: https://www.filfre.net/2014/10/starflight/

This emulator stands on the work of:
  Tim Lee - published partial source code and design documents in the early 2000s
            (preserved in starflight-reverse/webarchive).
  Sebastian Macke - starflight-reverse: disassembly, Forth-to-C transpiler, word names,
            data formats, code addresses, DOS interrupt usage, interpreter-enable patch.
            https://github.com/s-macke/starflight-reverse
  Jeremy Penner (SpindleyQ) - StarflightVM: boot and interrupt-handler analysis,
            Tandy detection detail, debugging-tool ideas.
            https://git.information-superhighway.net/SpindleyQ/starflightvm
  The anonymous migblog author - early reverse-engineering notes (archived by s-macke).
  Daniel Balsom - SingleStepTests 8088, the hardware-generated CPU test suite,
            and MartyPC, the model for the 8088 division microcode.
            https://github.com/SingleStepTests/8088  https://github.com/dbalsom/martypc
  Microsoft - the MS-DOS 4.0 source (MIT), the reference for FCB file I/O semantics.
  Ralf Brown's Interrupt List; Howard Hinnant's civil-date algorithms.
  The DOSBox team - the reference emulator used for comparison.
  Daniel Hepper, Marcel Sondaar and Joaquin Rosales - the public-domain 8x8 font
            and the font8x8 crate used for text screens.

See CREDITS.md for which facts come from where.
";
