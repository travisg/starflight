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

The game boots and runs: the display menu (Black/white, RGB, Composite,
Hercules, EGA), and with EGA the Binary Systems credits, the title screen,
the Interstel Police warning and the starport. Only EGA is emulated so far.
The 8086 core passes all 2,559,500 documented-instruction tests of the
SingleStepTests 8088 suite. Remaining milestones:

1. M4: playability across the game (flight, planets, encounters, combat);
   SAVE GAME and resume, checked against DOSBox.
2. M5: save management polish, speed control, CGA.

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
starflight play --load-state NAME # start from a snapshot (see below)
```

The machine is paced to real time; the game's own clock runs off the
emulated timer, so timed delays keep their original length at any `--mhz`.
In the window: F11 toggles fullscreen, F12 toggles turbo, Pause pauses,
Ctrl+F11 saves a snapshot and Ctrl+F12 loads the latest one. The arrow keys,
Home/End/PgUp/PgDn and the numeric keypad are the XT keypad.

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

### Snapshots

A snapshot (state) holds the whole emulated PC (CPU, memory, video adapter,
timers, keyboard, DOS) together with the slot's STARA.COM and STARB.COM at
that instant, so a game can be resumed from any moment rather than only from
SAVE GAME. Each state belongs to a slot and lives in
`saves/<slot>/states/<name>/`: `machine.bin.gz`, the two game files, and
`info.txt` (UTC creation time, emulated time, instructions executed, and an
optional description).

In the window, Ctrl+F11 saves a state named `quick-YYYYMMDD-HHMMSS` (UTC)
and shows the name in the title bar for a few seconds; Ctrl+F12 loads the
slot's most recent state. Ctrl itself still reaches the game.

```sh
starflight states mygame                                      # list a slot's states
starflight play --save mygame --load-state quick-20261030-231502
starflight headless --keys '{wait 2000}5{enter}' --save-state credits
starflight headless --load-state credits --keys '{space}' --run-ms 20000
```

Loading a state first backs up the slot's files (each load makes a backup,
and a slot keeps its last 20), then puts the state's STARA.COM and STARB.COM
in their place with the times they had. The slot's STARFLT.COM must be the
known 54183-byte build. The CPU speed is the session's `--mhz`, not the one
the state was saved at. Ctrl, Shift and Alt that were held down when the
state was saved (a Ctrl+F11 state has Ctrl down) are released on load,
unless they are held on the host. A name already in use is refused; remove
the state's directory to reuse it. A state written by a build with another
snapshot format is refused with a message naming both versions.

### Without a window

`headless` runs the game for a while with scripted keys and prints the text
screen; it recreates its slot (`headless`) from the game directory each run
unless `--keep` or `--load-state` is given (recreating leaves the slot's
states alone).

```sh
starflight headless --run-ms 8000 --text
starflight headless --keys '1{wait 2000}{enter}' --screenshot out.png
starflight headless --trace-forth trace.txt --files
starflight headless --keys '{wait 3000}5{enter}' --run-ms 60000 --shot-every 4000
```

`{hold KEY MS}` holds a key with XT typematic repeats (steering in flight
needs held keys). `{code}` types the answer to the launch security check (the code wheel) on
screen. When playing, the answer appears in the window title while the
prompt is up and is printed in the terminal.
The game derives the code from the three wheel positions it shows, so the
emulator reads them from memory and applies the game's own formula
(`crates/sfforth/src/security.rs`).

Keys are typed one at a time, each after the game has taken the previous one
(its keyboard handler drops buffered keys). `--trace-forth` writes every Forth
word the game executes (return-stack pointer, code field address, name);
`--files` lists the game's file transfers by directory entry; `--shot-every
MS` writes a PNG every MS of emulated time into `screenshots/` (gitignored). If the machine
stops (an unknown opcode, port or DOS function), the report shows where, the
registers and the most recent Forth words. `--load-state NAME` starts from a
state of the slot; `--save-state NAME` (with an optional `--description`)
saves one when the run finishes, but not if the machine stopped or the game
exited. The name is checked before the run starts.

### Tools

```sh
starflight words      # every word in STARFLT.COM's dictionary, with its type
starflight decompile COLD   # a colon definition's threaded code (name or hex CFA)
starflight dir        # the STARA/STARB file directory (FILE# table)
starflight credits    # who made the game and whose work this builds on
```

`words` prints each word's code field address, kind (CODE, COLON, VAR, CONST,
or `DOES:xxxx` for other defining words), vocabulary chain, where the name came
from, its declared length, and the name. FORTHOUGHT kept only a prefix of
some names (usually 7 characters) while recording the full length; those end
in `…`, so `LET-TH…` with length 23 is a 23-character name of which "LET-TH"
survives. 1375 of the 1703 names are complete.

## Layout

| Path | What it is |
| --- | --- |
| `crates/cpu8086` | Real-mode 8086 interpreter core (no dependencies by default; `serde` feature for snapshots). |
| `crates/pc` | The PC/XT-class machine: devices, BIOS and DOS emulation. |
| `crates/sfforth` | Starflight's Forth seen from outside: dictionary, directory, tracing. |
| `crates/starflight` | The `starflight` command: CLI, SDL front end, headless runner. |

## License

MIT; see [LICENSE](LICENSE). Starflight itself is not included and remains
the property of its rights holders.
