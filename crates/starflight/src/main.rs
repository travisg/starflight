// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

mod credits;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use sfforth::{consts, dict, directory};
use std::path::{Path, PathBuf};

const DEFAULT_GAME_DIR: &str = "/storage/games/starflt/START";

#[derive(Parser)]
#[command(name = "starflight", version, about = credits::BANNER)]
struct Cli {
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
    /// List the STARA/STARB file directory.
    Dir {
        #[arg(long, default_value = DEFAULT_GAME_DIR)]
        game_dir: PathBuf,
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
        println!(
            "{:04X}  {:<10} {:<3} {:<6} {}",
            w.cfa,
            w.kind.label(),
            w.vocab,
            w.provenance.label(),
            name
        );
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

fn cmd_dir(game_dir: &Path) -> Result<()> {
    let stara = read_game_file(game_dir, "STARA.COM")?;
    let entries = directory::parse(&stara);
    println!("{:>5}  {:<12} {:>4} {:>8} {:>7} {:>6} {:>4}", "file#", "name", "file", "offset", "span", "nrec", "rlen");
    for e in &entries {
        let file = match e.fileno {
            1 => "A".to_string(),
            2 => "B".to_string(),
            n => n.to_string(),
        };
        println!(
            "{:>5}  {:<12} {:>4} {:>8X} {:>7} {:>6} {:>4}",
            e.index,
            e.name,
            file,
            e.offset(),
            e.span(),
            e.nrec,
            e.reclen
        );
    }
    Ok(())
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
    match cli.cmd {
        Cmd::Words { game_dir } => cmd_words(&game_dir),
        Cmd::Dir { game_dir } => cmd_dir(&game_dir),
        Cmd::Credits => {
            print!("{}", credits::CREDITS);
            Ok(())
        }
    }
}
