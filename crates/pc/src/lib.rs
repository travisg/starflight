// Copyright (c) 2026 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//! The PC/XT-class machine: memory map, devices, BIOS and DOS HLE.

pub mod bios;
pub mod bus;
pub mod dos;
pub mod hle;
pub mod keyboard;
pub mod machine;
pub mod pic;
pub mod pit;
pub mod snapshot;
pub mod video;

pub use machine::{Config, Machine};

use std::fmt;

/// Why the machine stopped running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// An opcode the CPU core does not execute.
    InvalidOpcode { cs: u16, ip: u16, bytes: [u8; 2] },
    /// An I/O port no device answers.
    UnknownPort { port: u16, write: Option<u8> },
    /// A BIOS or DOS function this machine does not implement.
    Unsupported(String),
    /// The emulator trap opcode outside the BIOS ROM.
    StrayTrap { number: u8, cs: u16, ip: u16 },
    /// HLT with interrupts disabled: the PC would hang here.
    HaltedWithInterruptsOff { cs: u16, ip: u16 },
    /// The program terminated through DOS.
    Exit(u8),
}

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stop::InvalidOpcode { cs, ip, bytes } => {
                write!(f, "invalid opcode {:02X} {:02X} at {cs:04X}:{ip:04X}", bytes[0], bytes[1])
            }
            Stop::UnknownPort { port, write: Some(v) } => write!(f, "write of {v:02X} to unknown I/O port {port:04X}"),
            Stop::UnknownPort { port, write: None } => write!(f, "read of unknown I/O port {port:04X}"),
            Stop::Unsupported(what) => write!(f, "unsupported: {what}"),
            Stop::StrayTrap { number, cs, ip } => {
                write!(f, "emulator trap {number:02X} outside the ROM at {cs:04X}:{ip:04X}")
            }
            Stop::HaltedWithInterruptsOff { cs, ip } => write!(f, "HLT with interrupts disabled at {cs:04X}:{ip:04X}"),
            Stop::Exit(code) => write!(f, "program exited with code {code}"),
        }
    }
}
