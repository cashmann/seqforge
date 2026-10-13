//! Zelda64 (OoT / MM) music sequence tools: opcode tables, a control-flow
//! tracing disassembler, and an assembler for its text output.
//!
//! The text format is a small assembly language in the spirit of the zeldaret
//! decomp's `aseq.h`: section directives (`.sequence`, `.channel`,
//! `.layer large|short`, `.envelope`, `.table`, `.data`), `label:` lines, instructions
//! with comma-separated operands, and `.byte` data. `$n` writes an address
//! operand's raw encoded value; `long(n)` forces the 2-byte form of a
//! variable-length value.

pub mod abi;
mod asm;
mod disasm;

pub use abi::Game;
pub use asm::{AsmError, assemble};
pub use disasm::{Disassembly, Stats, disassemble};

/// The kind of content at a position in the sequence, which also decides how
/// mnemonics are resolved when assembling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Sequence,
    Channel,
    /// Layer script; `large` selects the note encoding (channel `noshort`).
    Layer {
        large: bool,
    },
    Envelope,
    /// Dyntable: `entry <label>` lines, one u16 pointer each.
    Table,
    Data,
}

impl Section {
    pub fn directive(self) -> &'static str {
        match self {
            Section::Sequence => ".sequence",
            Section::Channel => ".channel",
            Section::Layer { large: true } => ".layer large",
            Section::Layer { large: false } => ".layer short",
            Section::Envelope => ".envelope",
            Section::Table => ".table",
            Section::Data => ".data",
        }
    }

    fn level(self) -> Option<abi::Level> {
        match self {
            Section::Sequence => Some(abi::Level::Seq),
            Section::Channel => Some(abi::Level::Chan),
            Section::Layer { .. } => Some(abi::Level::Layer),
            Section::Envelope | Section::Table | Section::Data => None,
        }
    }
}
