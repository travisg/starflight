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
  `game/START/` (gitignored, read-only; copied from an unplayed
  installation). They are opened read-only. The game writes
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
  words). Do not stub things silently to get further. Ports of hardware such
  a PC may lack (MDA/Hercules, CGA-only registers, serial, parallel) read
  FFh and are counted in `absent_ports`, listed at the end of a run.
- Every source file starts with the MIT copyright header used in the existing
  files.
- Update README.md in the same commit as any change to behaviour, commands or
  files.

## Code map

| Path | Contents |
| --- | --- |
| `crates/cpu8086` | 8086 interpreter core. No dependencies by default; the `serde` feature derives (de)serialisation of `Cpu` for snapshots. |
| `crates/cpu8086/tests/sst.rs` | SingleStepTests 8088 harness. |
| `crates/pc/src/machine.rs` | Run loop: interrupts, traps, device events, idle skipping. |
| `crates/pc/src/snapshot.rs` | Machine snapshots: `Machine::save_state`/`restore_state` (gzip, SFSNAP header, bincode body). |
| `crates/pc/tests/snapshot.rs` | Snapshot format checks; save/restore/replay determinism on the real game. |
| `crates/pc/src/bus.rs` | Memory map, port decoding, master clock (1/256 of 14.318 MHz ticks). |
| `crates/pc/src/bios.rs` | ROM image with trap stubs at XT addresses, IVT, BDA, INT 08/09/16/1A. |
| `crates/pc/src/dos/` | COM loader and INT 21h FCB file I/O over a save-slot directory. |
| `crates/pc/src/video/` | EGA: planes and latches, text and mode 0Dh rendering, INT 10h. |
| `crates/pc/src/{pic,pit,keyboard}.rs` | 8259, 8253 (+ speaker state), XT keyboard and scan-code translation. |
| `crates/pc/src/hle.rs` | Helpers for trap handlers (`Mem`: guest memory at program-chosen addresses); BDA addresses. |
| `crates/sfforth/src/consts.rs` | Fixed addresses in STARFLT.COM. |
| `crates/sfforth/src/dict.rs` | Dictionary header decoding. |
| `crates/sfforth/src/directory.rs` | STARA/STARB directory parsing. |
| `crates/sfforth/src/security.rs` | Launch security check: STP-OV signature, variables, code formula. |
| `crates/sfforth/src/{trace,decompile}.rs` | Word-entry tracing (NEXT's `jmp [bx]`), colon-definition listing. |
| `crates/starflight/src/main.rs` | The `starflight` CLI. |
| `crates/starflight/src/session.rs` | Machine + Forth view, paced key scripts, stop reports. |
| `crates/starflight/src/{sdl,audio}.rs` | SDL window, input mapping, PC-speaker synthesis. |
| `crates/starflight/src/saves.rs` | Save slots, backups, and states (snapshot + STARA/STARB per slot). |
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
- Overlays share load addresses (STP-OV and MISC-OV are both 1440 bytes at
  EFC0) and swap while a word waits for input; identify an overlay by a code
  signature, not its header, and read its variables while its words run.
- Directory start/end paragraphs address STARA.COM then STARB.COM as one
  space (STARB from offset 256000); use `DirEntry::location()`, not the
  file-number byte. Data records also never cross a 1 KiB block boundary.
- No CPU decode cache: the game rewrites its own code (0x193, 0x16C9).
- XKEYINT empties the BIOS key buffer on every key press, so scripted keys
  must wait until the previous one was taken (`Session::press`).
- Trap stubs must stay at the IBM XT BIOS addresses; the game jumps to
  F000:E987 directly.
- One clock drives PIT readback, IRQ0, retrace bits and pacing; stubbing any
  of them changes game speed or hangs polling loops.
- Snapshots serialise `Machine` with serde. Any new machine state (a field of
  `Machine`, `PcBus`, `Cpu`, `Dos` or a device) must be serialised, not
  `#[serde(skip)]`, or it is lost on restore; skip only host-side plumbing,
  and say in `snapshot.rs` what a restore keeps for it. Changing a serialised
  type changes the format: bump `snapshot::VERSION`. The BIOS ROM with its
  trap stubs lives in `bus.ram`, so a state brings back the ROM it was saved
  with: changing the ROM image or what a trap number does needs a bump too.
  The determinism test catches state that is missed, if the game exercises
  it.
- A state's STARA/STARB copies keep their modification times: DOS puts file
  times into FCBs in guest memory, so a restore must see the times the
  machine saw.
- Trap handlers reach addresses the program chose (FCBs, the DTA, buffers,
  the caller's stack) through `hle::Mem`, never the raw RAM array: display
  memory must go through the adapter, and the ROM area holds the trap stubs.
- Change a PIT gate with `Pit::set_gate`, not the `gate` field: edges
  restart or suspend counting.
- Save states through `Session::save_state`, not `Saves::save_state`
  directly: the launch security check lives in the session, not the
  machine, and only the session's call stores it in `info.txt`.

## How to test

```sh
cargo test
cargo run -p starflight -- words | head     # expect 1703 words on stderr summary
scripts/fetch-sst.sh                        # once: SingleStepTests 8088 data (~730 MB)
cargo test --release -p cpu8086 --test sst  # ~2.56M hardware tests, ~1 s
```

Tests that need the game (the snapshot determinism test, the state-load
test in `session.rs`) skip with a message when `game/START/STARFLT.COM` is
absent. They work on scratch copies under `$SF_TEST_DIR/snap-tests/`
(default: the target dir's tmp dir for integration tests, the system temp
dir for unit tests) and remove them when they pass.

The SST harness reads `SST_8088_DIR` (default `target/sst-8088/v2`, where
`scripts/fetch-sst.sh` downloads it) and skips if the data is absent.
`SST_FILTER=F6,D0.4` runs a subset; `SST_FAILS=path` saves failing test names.
Every test with status "normal" in the suite's metadata must pass.
