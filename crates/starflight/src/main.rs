// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

mod audio;
mod credits;
mod saves;
mod sdl;
mod session;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use saves::Saves;
use session::Session;
use sfforth::{consts, dict, directory};
use std::path::{Path, PathBuf};

/// The repo-local, gitignored copy of the original game files.
const DEFAULT_GAME_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/START");
/// Save slots live in the repo's gitignored saves/ directory by default.
const DEFAULT_SAVES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../saves");

#[derive(Parser)]
#[command(name = "starflight", version, about = credits::BANNER)]
struct Cli {
    /// Directory holding save slots.
    #[arg(long, global = true, default_value = DEFAULT_SAVES_DIR)]
    saves_dir: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the words in STARFLT.COM's dictionary.
    Words {
        /// Directory holding the original game files (only read).
        #[arg(long, default_value = DEFAULT_GAME_DIR)]
        game_dir: PathBuf,
    },
    /// List a colon definition's threaded code. WORD is a name or a hex CFA.
    Decompile {
        word: String,
        #[arg(long, default_value = DEFAULT_GAME_DIR)]
        game_dir: PathBuf,
    },
    /// List the STARA/STARB file directory.
    Dir {
        #[arg(long, default_value = DEFAULT_GAME_DIR)]
        game_dir: PathBuf,
    },
    /// Create a save slot from a game directory (an unplayed copy, or a played one).
    NewSave {
        name: String,
        #[arg(long, default_value = DEFAULT_GAME_DIR)]
        from: PathBuf,
        /// Overwrite an existing slot.
        #[arg(long)]
        replace: bool,
    },
    /// List save slots, or one slot's backups.
    Saves { name: Option<String> },
    /// Put one of a slot's backups back (the current files are backed up first).
    Restore { name: String, backup: String },
    /// List a slot's emulator states (snapshots), oldest first.
    States { name: String },
    /// Play in an SDL window. The save slot is created from the game directory
    /// if it does not exist, and backed up before every session.
    Play {
        #[arg(long, default_value = "default")]
        save: String,
        #[arg(long, default_value = DEFAULT_GAME_DIR)]
        game_dir: PathBuf,
        /// Emulated CPU speed in MHz (F12 toggles turbo while playing).
        #[arg(long, default_value_t = 4.77)]
        mhz: f64,
        /// Initial window size as a multiple of 320x240.
        #[arg(long, default_value_t = 3)]
        scale: u32,
        /// Start from this state of the slot (see `starflight states`).
        #[arg(long)]
        load_state: Option<String>,
    },
    /// Run the game without a window: scripted keys, text screen on stdout.
    Headless {
        /// Save slot to run; recreated from the game directory first unless
        /// --keep or --load-state.
        #[arg(long, default_value = "headless")]
        save: String,
        /// Use the slot as it is instead of recreating it.
        #[arg(long)]
        keep: bool,
        /// Start from this state of the slot (implies --keep).
        #[arg(long)]
        load_state: Option<String>,
        /// Save a state with this name when the run finishes without stopping.
        #[arg(long)]
        save_state: Option<String>,
        /// Description stored with --save-state's state.
        #[arg(long, requires = "save_state")]
        description: Option<String>,
        #[arg(long, default_value = DEFAULT_GAME_DIR)]
        game_dir: PathBuf,
        /// Key script: characters, {enter} {esc} {space} {up} {f1}..., {wait MS}.
        #[arg(long, default_value = "")]
        keys: String,
        /// Emulated time to run after the key script, in milliseconds.
        #[arg(long, default_value_t = 5000.0)]
        run_ms: f64,
        /// Emulated CPU speed in MHz.
        #[arg(long, default_value_t = 4.77)]
        mhz: f64,
        /// Print the text screen whenever it changes.
        #[arg(long)]
        text: bool,
        /// Write a PNG of the final screen.
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// Also write a PNG every N ms of emulated time (after the key script)
        /// into --shots-dir.
        #[arg(long)]
        shot_every: Option<f64>,
        #[arg(long, default_value = "screenshots")]
        shots_dir: PathBuf,
        /// Write every Forth word entered (return-stack pointer, CFA, name) to a file.
        #[arg(long)]
        trace_forth: Option<PathBuf>,
        /// Print the game's file transfers at the end.
        #[arg(long)]
        files: bool,
    },
    /// Show who made the game and whose work this emulator builds on.
    Credits,
}

fn read_game_file(dir: &Path, name: &str) -> Result<Vec<u8>> {
    let path = dir.join(name);
    std::fs::read(&path).with_context(|| format!("reading {}", path.display()))
}

fn cmd_words(game_dir: &Path) -> Result<()> {
    let com = read_game_file(game_dir, "STARFLT.COM")?;
    if com.len() != consts::STARFLT_COM_SIZE {
        bail!(
            "STARFLT.COM is {} bytes; the known build is {} bytes, so its addresses would not apply",
            com.len(),
            consts::STARFLT_COM_SIZE
        );
    }
    let seg = sfforth::segment_image(&com);
    let mut words = dict::decode_dictionary(&seg);
    words.sort_by_key(|w| w.cfa);
    println!("{:>4}  {:<10} {:<3} {:<6} name", "cfa", "kind", "voc", "source");
    for w in &words {
        let mut name = w.display_name();
        if w.truncated() {
            name.push_str(&format!("~{}", w.declared_len));
        }
        if w.immediate() {
            name.push_str(" (immediate)");
        }
        println!("{:04X}  {:<10} {:<3} {:<6} {}", w.cfa, w.kind.label(), w.vocab, w.provenance.label(), name);
    }
    let count = |k: fn(&dict::Kind) -> bool| words.iter().filter(|w| k(&w.kind)).count();
    eprintln!(
        "{} words: {} colon, {} code, {} variable, {} constant, {} other",
        words.len(),
        count(|k| *k == dict::Kind::Colon),
        count(|k| *k == dict::Kind::Code),
        count(|k| *k == dict::Kind::Variable),
        count(|k| *k == dict::Kind::Constant),
        count(|k| matches!(k, dict::Kind::Does(_))),
    );
    Ok(())
}

fn cmd_decompile(game_dir: &Path, word: &str) -> Result<()> {
    let com = read_game_file(game_dir, "STARFLT.COM")?;
    let seg = sfforth::segment_image(&com);
    let words = dict::decode_dictionary(&seg);
    let names = sfforth::trace::Names::from_words(&words);
    let cfa = match words.iter().find(|w| w.name.eq_ignore_ascii_case(word)) {
        Some(w) => w.cfa,
        None => u16::from_str_radix(word.trim_start_matches("0x"), 16)
            .with_context(|| format!("no word named {word:?} (and not a hex CFA)"))?,
    };
    let Some(lines) = sfforth::decompile::decompile(&seg, cfa, &names, 4000) else {
        bail!("{} at {cfa:04X} is not a colon definition", names.label(cfa));
    };
    println!(": {}  ( CFA {cfa:04X} )", names.label(cfa));
    for l in lines {
        println!("  {:04X}  {}", l.addr, l.text);
    }
    Ok(())
}

fn cmd_dir(game_dir: &Path) -> Result<()> {
    let stara = read_game_file(game_dir, "STARA.COM")?;
    let entries = directory::parse(&stara);
    println!("{:>5}  {:<12} {:>4} {:>8} {:>7} {:>6} {:>4}", "file#", "name", "file", "offset", "span", "nrec", "rlen");
    for e in &entries {
        let file = match (e.span(), e.location().0) {
            (0, _) => "-",
            (_, 1) => "A",
            _ => "B",
        };
        println!(
            "{:>5}  {:<12} {:>4} {:>8X} {:>7} {:>6} {:>4}",
            e.index,
            e.name,
            file,
            e.location().1,
            e.span(),
            e.nrec,
            e.reclen
        );
    }
    Ok(())
}

fn write_png(path: &Path, frame: &pc::video::Frame) -> Result<()> {
    if frame.width == 0 || frame.height == 0 {
        bail!("the video adapter produced no picture to save");
    }
    let file = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), frame.width as u32, frame.height as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header()?;
    let data: Vec<u8> = frame.pixels.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    w.write_image_data(&data)?;
    Ok(())
}

/// Emulator states to start from and to save at the end of a headless run.
struct HeadlessStates<'a> {
    load: Option<&'a str>,
    save: Option<&'a str>,
    description: Option<&'a str>,
}

#[allow(clippy::too_many_arguments)]
fn cmd_headless(
    saves: &Saves,
    save: &str,
    keep: bool,
    game_dir: &Path,
    keys: &str,
    run_ms: f64,
    mhz: f64,
    text: bool,
    screenshot: Option<&Path>,
    shots: Option<(f64, &Path)>,
    trace_forth: Option<&Path>,
    files: bool,
    states: HeadlessStates,
) -> Result<()> {
    let script = session::parse_keys(keys)?;
    if let Some(name) = states.save {
        saves.check_new_state(save, name)?;
    }
    let slot = if keep || states.load.is_some() {
        if !saves.exists(save) {
            bail!("no save {save:?}; create it with `starflight new-save {save}`");
        }
        saves.slot_dir(save)?
    } else {
        saves.create(save, game_dir, true)?
    };
    let trace: Option<Box<dyn std::io::Write>> = match trace_forth {
        Some(p) => Some(Box::new(std::io::BufWriter::new(
            std::fs::File::create(p).with_context(|| format!("creating {}", p.display()))?,
        ))),
        None => None,
    };
    eprintln!("{}", credits::BANNER);
    let mut s = Session::new(&slot, mhz, trace)?;
    s.show_text = text;
    if let Some(name) = states.load {
        let backup = s.load_state(saves, save, name, &[])?;
        eprintln!(
            "loaded state {name:?} ({:.2}s emulated); save {save:?} backed up to {}",
            pc::Machine::master_to_ms(s.machine.now()) / 1000.0,
            backup.display()
        );
    }
    let result = s.play_script(&script).and_then(|_| match shots {
        None => s.run_ms(run_ms),
        Some((every, dir)) => {
            std::fs::create_dir_all(dir).map_err(|e| pc::Stop::Unsupported(format!("creating {}: {e}", dir.display())))?;
            let mut done = 0.0;
            let mut n = 0;
            while done < run_ms {
                let step = every.min(run_ms - done);
                s.run_ms(step)?;
                done += step;
                n += 1;
                let mut frame = pc::video::Frame::default();
                s.machine.render(&mut frame);
                let t = pc::Machine::master_to_ms(s.machine.now()) / 1000.0;
                let p = dir.join(format!("shot-{n:04}-{t:07.2}s.png"));
                if let Err(e) = write_png(&p, &frame) {
                    eprintln!("{}: {e}", p.display());
                }
            }
            Ok(())
        }
    });
    if !text {
        if let Some(lines) = s.machine.bus.video.text_lines() {
            let last = lines.iter().rposition(|l| !l.trim().is_empty()).map_or(0, |i| i + 1);
            for l in &lines[..last] {
                println!("{}", l.trim_end());
            }
        }
    }
    if files {
        print!("{}", s.file_summary());
    }
    if let Some(p) = screenshot {
        let mut frame = pc::video::Frame::default();
        s.machine.render(&mut frame);
        write_png(p, &frame)?;
        eprintln!("wrote {} ({}x{})", p.display(), frame.width, frame.height);
    }
    let m = &s.machine;
    eprintln!(
        "ran {:.2}s emulated, {} instructions, {} frames",
        pc::Machine::master_to_ms(m.now()) / 1000.0,
        m.instructions,
        m.frames
    );
    if !m.bus.absent_ports.is_empty() {
        let ports: Vec<String> = m.bus.absent_ports.iter().map(|(p, n)| format!("{p:03X}x{n}")).collect();
        eprintln!("absent-hardware port accesses: {}", ports.join(" "));
    }
    if let Some(name) = states.save {
        if result.is_ok() {
            saves.save_state(save, Some(name), m, states.description)?;
            eprintln!("saved state {name:?} of save {save:?}");
        } else {
            eprintln!("not saving state {name:?}: the run did not finish");
        }
    }
    match result {
        Ok(()) => Ok(()),
        Err(pc::Stop::Exit(code)) => {
            eprintln!("game exited to DOS (code {code})");
            Ok(())
        }
        Err(stop) => {
            eprint!("{}", s.report(&stop));
            bail!("machine stopped: {stop}")
        }
    }
}

/// Lets `starflight words | head` end quietly instead of panicking on EPIPE.
#[cfg(unix)]
fn default_sigpipe() {
    unsafe extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

fn main() -> Result<()> {
    #[cfg(unix)]
    default_sigpipe();
    let cli = Cli::parse();
    let saves = Saves::new(cli.saves_dir);
    match cli.cmd {
        Cmd::Words { game_dir } => cmd_words(&game_dir),
        Cmd::Decompile { word, game_dir } => cmd_decompile(&game_dir, &word),
        Cmd::Dir { game_dir } => cmd_dir(&game_dir),
        Cmd::NewSave { name, from, replace } => {
            let dir = saves.create(&name, &from, replace)?;
            println!("created save {name:?} in {} from {}", dir.display(), from.display());
            Ok(())
        }
        Cmd::Saves { name: None } => {
            for s in saves.list() {
                println!("{s}");
            }
            Ok(())
        }
        Cmd::Saves { name: Some(name) } => {
            for b in saves.backups(&name)? {
                println!("{b}");
            }
            Ok(())
        }
        Cmd::Restore { name, backup } => {
            saves.restore(&name, &backup)?;
            println!("restored save {name:?} from backup {backup}");
            Ok(())
        }
        Cmd::States { name } => {
            if !saves.exists(&name) {
                bail!("no save {name:?}");
            }
            let states = saves.states(&name)?;
            let width = states.iter().map(|s| s.name.len()).max().unwrap_or(0).max(4);
            println!("{:<width$}  {:<23}  {:>10}  description", "name", "created", "emulated");
            for s in &states {
                let emulated = format!("{:.1}s", s.emulated_s);
                let desc = s.description.as_deref().unwrap_or("");
                println!("{:<width$}  {:<23}  {emulated:>10}  {desc}", s.name, s.created);
            }
            Ok(())
        }
        Cmd::Headless {
            save,
            keep,
            load_state,
            save_state,
            description,
            game_dir,
            keys,
            run_ms,
            mhz,
            text,
            screenshot,
            shot_every,
            shots_dir,
            trace_forth,
            files,
        } => cmd_headless(
            &saves,
            &save,
            keep,
            &game_dir,
            &keys,
            run_ms,
            mhz,
            text,
            screenshot.as_deref(),
            shot_every.map(|e| (e, shots_dir.as_path())),
            trace_forth.as_deref(),
            files,
            HeadlessStates { load: load_state.as_deref(), save: save_state.as_deref(), description: description.as_deref() },
        ),
        Cmd::Play { save, game_dir, mhz, scale, load_state } => {
            if load_state.is_some() {
                // Loading the state backs the slot up.
                if !saves.exists(&save) {
                    bail!("no save {save:?} to load a state of");
                }
            } else if saves.exists(&save) {
                let b = saves.backup(&save)?;
                eprintln!("backed up save {save:?} to {}", b.display());
            } else {
                let dir = saves.create(&save, &game_dir, false)?;
                eprintln!("created save {save:?} in {} from {}", dir.display(), game_dir.display());
            }
            eprintln!("{}", credits::BANNER);
            let mut session = Session::new(&saves.slot_dir(&save)?, mhz, None)?;
            if let Some(name) = &load_state {
                let b = session.load_state(&saves, &save, name, &[])?;
                eprintln!("loaded state {name:?}; save {save:?} backed up to {}", b.display());
            }
            sdl::run(session, scale, &saves, &save)
        }
        Cmd::Credits => {
            print!("{}", credits::CREDITS);
            Ok(())
        }
    }
}
