# starflight

A Rust + SDL program that runs the original **Starflight** (Binary Systems /
Electronic Arts, 1986) from its DOS files by emulating the 8086 PC it was
written for. The game's own code runs unmodified: its Forth system, its
machine-code primitives and its CGA/EGA drivers. A Forth-aware layer on top
names, traces and decompiles what the game is doing.

## Acknowledgements

Starflight was created by **Binary Systems** and published by Electronic Arts:
Rod McConnell, Greg Johnson, Tim Lee, Alec Kercso, Bob Gonsalves and Dave
Boulton, with help from Paul Reiche III and Joe Ybarra
([The Digital Antiquarian](https://www.filfre.net/2014/10/starflight/) tells
the story).

This project builds directly on other people's work:

- **Sebastian Macke**, [starflight-reverse](https://github.com/s-macke/starflight-reverse):
  the disassembly, Forth-to-C transpiler, word names and data formats of the
  game. The code addresses, DOS interrupt usage and the patch that enables
  the game's built-in Forth interpreter all come from that work.
- **Jeremy Penner (SpindleyQ)**, [StarflightVM](https://git.information-superhighway.net/SpindleyQ/starflightvm):
  analysis of the boot code and interrupt handlers, the Tandy detection
  detail, and ideas for the debugging tools.
- **Tim Lee**, one of the original programmers, who published partial source
  code and design documents in the early 2000s (preserved in
  starflight-reverse's `webarchive/`).
- The anonymous author of the **migblog** reverse-engineering notes (also
  archived by starflight-reverse).
- **Daniel Balsom**, [SingleStepTests/8088](https://github.com/SingleStepTests/8088):
  the hardware-generated test suite the CPU core is checked against.
- The **DOSBox** team: the reference emulator used for comparison.

[CREDITS.md](CREDITS.md) lists which facts come from where. `starflight credits`
prints the same list.

## Status

The dictionary and directory tools work, and the 8086 core passes all
2,559,500 documented-instruction tests of the SingleStepTests 8088 suite. The
emulator does not run the game yet. The plan, in order:

1. M1 (done): 8086 core, checked against SingleStepTests.
2. M2: machine, BIOS and DOS emulation; boots headless to the display menu.
3. M3: SDL window, EGA graphics, keyboard and PC speaker; title screens.
4. M4: playability across the game; SAVE GAME and resume.
5. M5: save management, speed control, CGA.

## Building

Requires stable Rust. Later milestones also need the SDL2 development
libraries (`libsdl2-dev` on Debian/Ubuntu).

```sh
cargo build --release
cargo test
```

The CPU core's hardware test suite needs ~730 MB of test data:

```sh
scripts/fetch-sst.sh                        # downloads to /storage/scratch/claude/sst-8088
cargo test --release -p cpu8086 --test sst  # set SST_8088_DIR if you put it elsewhere
```

## Usage

The original game files are only ever read, never written. By default they
are taken from `game/START/` in this repo, a gitignored local copy of an
unplayed installation (STARA.COM, STARB.COM, STARFLT.COM, dated 1988-01-04).
Create it once from your own copy of the game:

```sh
mkdir -p game/START
cp -p /path/to/starflt/{STARA,STARB,STARFLT}.COM game/START/
chmod a-w game/START/*.COM
```

Pass `--game-dir` to use another copy.

### Playing

```sh
starflight play                   # save slot "default", created from game/START
starflight play --save mygame     # another slot
starflight play --mhz 8           # emulated CPU speed (default 4.77, the IBM PC)
```

In the window: F11 toggles fullscreen, F12 toggles turbo, Pause pauses. The
arrow keys, Home/End/PgUp/PgDn and the numeric keypad are the XT keypad.

### Save slots

The game rewrites STARA.COM and STARB.COM as it runs and when you SAVE GAME,
just as it did on DOS. Each save slot is a directory under `saves/`
(gitignored) holding working copies of the three files; the originals are
never touched. `play` backs the slot up before every session, which replaces
the game's REMEMBER/GOBACK batch files:

```sh
starflight new-save mygame --from game/START    # a new game
starflight new-save old --from /path/to/PLAY    # continue an existing game
starflight saves                                # list slots
starflight saves mygame                         # list a slot's backups
starflight restore mygame 20261030-231502       # put a backup back
```

### Without a window

`headless` runs the game for a while with scripted keys and prints the text
screen; it recreates its slot (`headless`) from the game directory each run
unless `--keep` is given.

```sh
starflight headless --run-ms 8000 --text
starflight headless --keys '1{wait 2000}{enter}' --screenshot out.png
starflight headless --trace-forth trace.txt --files
```

Keys are typed one at a time, each after the game has taken the previous one
(its keyboard handler drops buffered keys). `--trace-forth` writes every Forth
word the game executes (return-stack pointer, code field address, name);
`--files` lists the game's file transfers by directory entry. If the machine
stops (an unknown opcode, port or DOS function), the report shows where, the
registers and the most recent Forth words.

### Tools

```sh
starflight words      # every word in STARFLT.COM's dictionary, with its type
starflight dir        # the STARA/STARB file directory (FILE# table)
starflight credits    # who made the game and whose work this builds on
```

`words` prints each word's code field address, kind (CODE, COLON, VAR, CONST,
or `DOES:xxxx` for other defining words), vocabulary chain, where the name came
from, and the name. Truncated names show their declared length as `~N`.

## Layout

| Path | What it is |
| --- | --- |
| `crates/cpu8086` | Real-mode 8086 interpreter core (no dependencies). |
| `crates/pc` | The PC/XT-class machine: devices, BIOS and DOS emulation. |
| `crates/sfforth` | Starflight's Forth seen from outside: dictionary, directory, tracing. |
| `crates/starflight` | The `starflight` command: CLI, SDL front end, headless runner. |

## License

MIT; see [LICENSE](LICENSE). Starflight itself is not included and remains
the property of its rights holders.
