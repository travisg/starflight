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

### The DOSBox team

<https://www.dosbox.com/>

The reference emulator used to compare screens, timing and save files.

### This repository's author, 2002

`scanstarflt.cpp`, a directory scanner written in 2002 that had already
worked out the 48-entries-per-kilobyte layout of the STARA/STARB directory.
