# AGENTS.md

Ground rules and a map for anyone (human or agent) working in this repo.

## What this is

A Rust + SDL program that runs the original Starflight (1986) DOS game by
emulating an 8086 PC. The game's code runs unmodified on the emulated CPU;
Rust supplies the hardware, BIOS and DOS it expects, plus tooling that
understands the game's Forth. We do not reimplement game words in Rust except
as optional, clearly separated fast paths.

## Ground rules

- **Never write the original game files.** The default copy is
  `game/START/` (gitignored, read-only; copied from
  `/storage/games/starflt/START`). They are opened read-only. The game writes
  only to working copies in a save directory.
- **Never commit game files or game-derived images** (`.gitignore` covers
  `*.COM`, saves, traces, screenshots).
- **Credit prior work.** Any fact, address, table or technique taken from
  someone else's work gets a terse provenance comment where it is used (for
  example `// BOOT-HOOK; s-macke patch.c`) and an entry in CREDITS.md, in the
  same commit. s-macke/starflight-reverse and StarflightVM have no licence:
  use their facts, never copy their code or tables.
- **Fail loudly.** An unknown opcode, interrupt function or I/O port stops the
  machine with a report (what it was, the x86 registers, the recent Forth
  words). Do not stub things silently to get further.
- Every source file starts with the MIT copyright header used in the existing
  files.
- Update README.md in the same commit as any change to behaviour, commands or
  files.

## Code map

| Path | Contents |
| --- | --- |
| `crates/cpu8086` | 8086 interpreter core. No dependencies. |
| `crates/cpu8086/tests/sst.rs` | SingleStepTests 8088 harness. |
| `crates/pc` | Machine: memory map, PIC, PIT, keyboard, speaker, CGA/EGA, BIOS and DOS HLE. No SDL. |
| `crates/sfforth/src/consts.rs` | Fixed addresses in STARFLT.COM. |
| `crates/sfforth/src/dict.rs` | Dictionary header decoding. |
| `crates/sfforth/src/directory.rs` | STARA/STARB directory parsing. |
| `crates/starflight/src/main.rs` | The `starflight` CLI. |
| `crates/starflight/src/credits.rs` | Credits text; keep in step with CREDITS.md. |

## Invariants that are easy to break

- The addresses in `consts.rs` belong to the 54183-byte STARFLT.COM dated
  1988-01-04. Check the file size before relying on them.
- Dictionary names are XOR 0x7F with bit 7 on the last stored character;
  one-character names and the null word (NFA 0x253A) are stored plain.
  Headers can store fewer characters than their declared length.
- The four vocabulary chains are disjoint and together hold 1703 headers.
- Directory entries do not cross 1 KiB block boundaries: 48 entries of 21
  bytes per block, four blocks. An entry's position is its FILE#.

## How to test

```sh
cargo test
cargo run -p starflight -- words | head     # expect 1703 words on stderr summary
scripts/fetch-sst.sh                        # once: SingleStepTests 8088 data (~730 MB)
cargo test --release -p cpu8086 --test sst  # ~2.56M hardware tests, ~1 s
```

The SST harness reads `SST_8088_DIR` (default
`/storage/scratch/claude/sst-8088/v2`) and skips if the data is absent.
`SST_FILTER=F6,D0.4` runs a subset; `SST_FAILS=path` saves failing test names.
Every test with status "normal" in the suite's metadata must pass.
