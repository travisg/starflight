# Credits

## The game

**Starflight** (1986) was created by Binary Systems and published by
Electronic Arts. Team roles, per
[The Digital Antiquarian](https://www.filfre.net/2014/10/starflight/):

- Rod McConnell: founder of Binary Systems, kept the project funded and alive.
- Greg Johnson: lead designer and the team's only artist; plot, aliens, scenario.
- Tim Lee: most of the low-level architecture; fractal terrain on spinning planets.
- Alec Kercso: the starbase module.
- Bob Gonsalves: the planetside exploration module.
- Dave Boulton: the fractal-planet concept and early prototype code.
- Paul Reiche III: helped build the conversation engine.
- Joe Ybarra: Electronic Arts producer and the team's advocate at EA.

## Prior work this project uses

Facts, addresses and techniques taken from these sources are marked at the
point of use in the code (for example `// BOOT-HOOK; s-macke patch.c`).

### Sebastian Macke: starflight-reverse

<https://github.com/s-macke/starflight-reverse>

Used here:

- The fixed addresses in `crates/sfforth/src/consts.rs` (DOCOL, DOVAR, DOCON,
  EXIT, LIT, 2LIT, OPERATOR, the vocabulary heads), from
  `src/disasOV/global.h`.
- The dictionary name encoding (XOR 0x7F, bit 7 on the last character),
  from `src/patch/patch.c`.
- The STARA/STARB directory entry layout, from `src/disasOV/extract.c`.
- The list of DOS and BIOS interrupt functions the game calls, from
  `src/emul/call.c`.

The repository has no licence, so this project uses its facts only and copies
no code or tables.

### Jeremy Penner (SpindleyQ): StarflightVM

<https://git.information-superhighway.net/SpindleyQ/starflightvm>

Used here:

- The analysis of the boot code, the timer interrupt handler that rewrites its
  own increment, and the ctrl-break hook (`notes.txt`).
- The Tandy model-byte check at F000:C000 (`gamevm.lua`).
- Ideas for the trace and decompiler tooling.

No licence file, so facts only; no code is copied.

### Tim Lee: source code and design documents

Published at `sonic.net/~sage/sf` and as SFFiles in the early 2000s, now
preserved in starflight-reverse's `webarchive/`. Partial (roughly half the
source), and invaluable for word names and intent.

### The migblog author

Early partial reverse engineering of the binaries, posted at
`migblog.blog.com` by an author whose name is not known; archived in
starflight-reverse's `webarchive/`.

### Daniel Balsom: SingleStepTests 8088

<https://github.com/SingleStepTests/8088> (MIT)

The hardware-generated per-opcode test suite, produced with Arduino8088 and
MartyPC, that the CPU core is checked against. The test data is downloaded
locally and not included in this repository.

### Daniel Balsom: MartyPC

<https://github.com/dbalsom/martypc> (MIT)

The 8088 division microcode model (CORD, PREIDIV/POSTIDIV): the flags a
divide error leaves on the stack, IDIV's REP-prefix negation, and the DAA/DAS
high-digit threshold of 9Fh when AF is set. Reimplemented in
`crates/cpu8086/src/alu.rs` (marked there); no code copied.

### Microsoft: MS-DOS 4.0 source

<https://github.com/microsoft/MS-DOS> (MIT)

The FCB file-I/O, filename-parsing and search behaviour in `crates/pc/src/dos/`
(record size defaults, 3- versus 4-byte random record, read result codes,
INT 21h AH=29h's separator and field rules) follows the kernel source, which
kept DOS 3.3's FCB code. Cited at the point of use; no code copied.

### Ralf Brown's Interrupt List

Details of DOS and BIOS function results not covered by the DOS source (for
example INT 21h AH=02h with a tab).

### Howard Hinnant: civil-date algorithms

<https://howardhinnant.github.io/date_algorithms.html>

`civil_from_days`, used for backup timestamps (`crates/starflight/src/saves.rs`)
and the DOS clock's UTC fallback (`crates/pc/src/dos/clock.rs`).

### Joaquin Rosales: font8x8

<https://gitlab.com/saibatizoku/font8x8-rs> (MIT)

The 8x8 glyphs for text output (`crates/pc/src/video/font.rs`): Daniel
Hepper's public-domain font8x8 (<https://github.com/dhepper/font8x8>), from
Marcel Sondaar's public-domain font8_8.asm. The glyphs for CP437 codes 01-13,
16-1F, 7F, EC, EF, F0, F4, F5, F7, F9, FB, FC and FE, which it lacks, were
drawn for this project.

### IBM: EGA Technical Reference

The EGA register values for each BIOS mode follow the IBM EGA BIOS video
parameter tables, reconstructed rather than copied
(`crates/pc/src/video/modes.rs`).

### The DOSBox team

<https://www.dosbox.com/>

The reference emulator used to compare screens, timing and save files. Used
here:

- INT 10h AH=0Bh also setting palette registers 0-3 in graphics modes, after
  `INT10_SetBackgroundBorder`/`INT10_SetColorSelect`
  (`crates/pc/src/video/int10.rs`).

### This repository's author, 2002

`scanstarflt.cpp`, a directory scanner written in 2002 that had already
worked out the 48-entries-per-kilobyte layout of the STARA/STARB directory.
