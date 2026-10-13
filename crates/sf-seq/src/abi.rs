//! Opcode tables for the Zelda64 (OoT / MM) sequence language ("MML").
//!
//! Source of truth is the game's own decoder (`seqplayer.c` in the zeldaret
//! decompilations, `sSeqInstructionArgsTable` and the per-level dispatch), with
//! mnemonics taken from the decomp's `aseq.h` assembler. Where the two
//! disagree, the decoder wins (e.g. MM channel `0xBD` takes an s16, not a u8).
//!
//! Note commands (layer `0x00..=0xBF`) and `portamento` are encoded
//! statefully and are handled in code rather than in these tables.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Game {
    Oot,
    Mm,
}

impl Game {
    pub fn name(self) -> &'static str {
        match self {
            Game::Oot => "oot",
            Game::Mm => "mm",
        }
    }

    pub fn from_name(s: &str) -> Option<Game> {
        match s.to_ascii_lowercase().as_str() {
            "oot" => Some(Game::Oot),
            "mm" => Some(Game::Mm),
            _ => None,
        }
    }
}

impl fmt::Display for Game {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Which script level an instruction executes at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Seq,
    Chan,
    Layer,
}

/// What an address operand points at. Code targets are followed by the
/// disassembler; data targets are claimed as fixed-size blocks; `Ref` targets
/// only get a label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Code at the same level as the referencing instruction.
    Same,
    Chan,
    Layer,
    Envelope,
    /// Fixed-size data block (filter = 16, params = 8, short-note arrays = 16).
    Block(u16),
    /// Dyntable: a list of u16 code pointers. Sequence-level `dyncall` /
    /// `dynjump` index it directly; a channel's `dyntbl` is used later by
    /// `dynldlayer` (layer entries) or `dyncall` (channel entries).
    DynTable,
    Ref,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    U8,
    S8,
    U16,
    S16,
    /// Variable-length u16: 1 byte if < 0x80, else 2 bytes with the top bit set.
    Var,
    /// Absolute u16 offset into the sequence.
    Abs(Target),
    /// s8 displacement from the end of the instruction.
    Rel8(Target),
    /// s16 displacement from the end of the instruction.
    Rel16(Target),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Next,
    /// Execution does not fall through (end, jump, rjump, stop).
    Stop,
}

/// Effect an instruction has on the channel's note-encoding mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteMode {
    Keep,
    Short,
    Large,
}

#[derive(Clone, Copy, Debug)]
pub struct OpDef {
    pub name: &'static str,
    /// First opcode covered by this definition.
    pub op: u8,
    /// Number of low opcode bits used as the first operand (0 = none).
    pub low_bits: u8,
    pub args: &'static [Arg],
    pub flow: Flow,
    pub mode: NoteMode,
    /// For channel `dynldlayer` / `dyncall`: what the current dyntable's entries point at.
    pub dyn_use: Option<Target>,
}

impl OpDef {
    pub fn op_count(&self) -> u16 {
        1u16 << self.low_bits
    }

    pub fn covers(&self, byte: u8) -> bool {
        let b = byte as u16;
        let lo = self.op as u16;
        b >= lo && b < lo + self.op_count()
    }
}

const fn op(name: &'static str, op: u8, args: &'static [Arg]) -> OpDef {
    OpDef {
        name,
        op,
        low_bits: 0,
        args,
        flow: Flow::Next,
        mode: NoteMode::Keep,
        dyn_use: None,
    }
}

const fn low(name: &'static str, op: u8, low_bits: u8, args: &'static [Arg]) -> OpDef {
    OpDef {
        name,
        op,
        low_bits,
        args,
        flow: Flow::Next,
        mode: NoteMode::Keep,
        dyn_use: None,
    }
}

const fn stop(name: &'static str, op: u8, args: &'static [Arg]) -> OpDef {
    OpDef {
        name,
        op,
        low_bits: 0,
        args,
        flow: Flow::Stop,
        mode: NoteMode::Keep,
        dyn_use: None,
    }
}

const fn dyn_use(name: &'static str, op: u8, low_bits: u8, target: Target) -> OpDef {
    OpDef {
        name,
        op,
        low_bits,
        args: &[],
        flow: Flow::Next,
        mode: NoteMode::Keep,
        dyn_use: Some(target),
    }
}

const fn mode(name: &'static str, op: u8, mode: NoteMode) -> OpDef {
    OpDef {
        name,
        op,
        low_bits: 0,
        args: &[],
        flow: Flow::Next,
        mode,
        dyn_use: None,
    }
}

use Arg::*;

/// Control flow, shared by all three levels (`0xF2..=0xFF`).
const FLOW: &[OpDef] = &[
    op("rbltz", 0xF2, &[Rel8(Target::Same)]),
    op("rbeqz", 0xF3, &[Rel8(Target::Same)]),
    stop("rjump", 0xF4, &[Rel8(Target::Same)]),
    op("bgez", 0xF5, &[Abs(Target::Same)]),
    op("break", 0xF6, &[]),
    op("loopend", 0xF7, &[]),
    op("loop", 0xF8, &[U8]),
    op("bltz", 0xF9, &[Abs(Target::Same)]),
    op("beqz", 0xFA, &[Abs(Target::Same)]),
    stop("jump", 0xFB, &[Abs(Target::Same)]),
    op("call", 0xFC, &[Abs(Target::Same)]),
    op("delay", 0xFD, &[Var]),
    op("delay1", 0xFE, &[]),
    stop("end", 0xFF, &[]),
];

const SEQ_COMMON: &[OpDef] = &[
    low("testchan", 0x00, 4, &[]),
    low("stopchan", 0x40, 4, &[]),
    low("subio", 0x50, 4, &[]),
    low("ldres", 0x60, 4, &[U8, U8]),
    low("stio", 0x70, 4, &[]),
    low("ldio", 0x80, 4, &[]),
    low("ldchan", 0x90, 4, &[Abs(Target::Chan)]),
    low("rldchan", 0xA0, 4, &[Rel16(Target::Chan)]),
    low("ldseq", 0xB0, 4, &[U8, Abs(Target::Ref)]),
    op("runseq", 0xC4, &[U8, U8]),
    op("scriptctr", 0xC5, &[U16]),
    stop("stop", 0xC6, &[]),
    op("stseq", 0xC7, &[U8, Abs(Target::Ref)]),
    op("sub", 0xC8, &[U8]),
    op("and", 0xC9, &[U8]),
    op("ldi", 0xCC, &[U8]),
    op("dyncall", 0xCD, &[Abs(Target::DynTable)]),
    op("rand", 0xCE, &[U8]),
    op("notealloc", 0xD0, &[U8]),
    op("ldshortgatearr", 0xD1, &[Abs(Target::Block(16))]),
    op("ldshortvelarr", 0xD2, &[Abs(Target::Block(16))]),
    op("mutebhv", 0xD3, &[U8]),
    op("mute", 0xD4, &[]),
    op("mutescale", 0xD5, &[S8]),
    op("freechan", 0xD6, &[U16]),
    op("initchan", 0xD7, &[U16]),
    op("volscale", 0xD9, &[S8]),
    op("volmode", 0xDA, &[U8, U16]),
    op("vol", 0xDB, &[U8]),
    op("tempochg", 0xDC, &[S8]),
    op("tempo", 0xDD, &[U8]),
    op("rtranspose", 0xDE, &[S8]),
    op("transpose", 0xDF, &[S8]),
    op("unk_EF", 0xEF, &[S16, U8]),
    op("freenotelist", 0xF0, &[]),
    op("allocnotelist", 0xF1, &[U8]),
];

const SEQ_MM: &[OpDef] = &[
    op("dynjump", 0xC2, &[Abs(Target::DynTable)]),
    op("mutechan", 0xC3, &[Abs(Target::Ref)]),
];

const CHAN_COMMON: &[OpDef] = &[
    low("cdelay", 0x00, 4, &[]),
    // 0x10..=0x17 = instrument sample, 0x18..=0x1F = sfx sample (aseq `ldsample type, port`)
    low("ldsample", 0x10, 4, &[]),
    low("ldchan", 0x20, 4, &[Abs(Target::Chan)]),
    low("stcio", 0x30, 4, &[U8]),
    low("ldcio", 0x40, 4, &[U8]),
    low("subio", 0x50, 4, &[]),
    low("ldio", 0x60, 4, &[]),
    low("stio", 0x70, 3, &[]),
    low("rldlayer", 0x78, 3, &[Rel16(Target::Layer)]),
    low("testlayer", 0x80, 3, &[]),
    low("ldlayer", 0x88, 3, &[Abs(Target::Layer)]),
    low("dellayer", 0x90, 3, &[]),
    dyn_use("dynldlayer", 0x98, 3, Target::Layer),
    op("ldfilter", 0xB0, &[Abs(Target::Block(16))]),
    op("freefilter", 0xB1, &[]),
    op("ldseqtoptr", 0xB2, &[Abs(Target::Ref)]),
    op("filter", 0xB3, &[U8]),
    op("ptrtodyntbl", 0xB4, &[]),
    op("dyntbltoptr", 0xB5, &[]),
    op("dyntblv", 0xB6, &[]),
    op("randtoptr", 0xB7, &[U16]),
    op("rand", 0xB8, &[U8]),
    op("randvel", 0xB9, &[U8]),
    op("randgate", 0xBA, &[U8]),
    op("combfilter", 0xBB, &[U8, U16]),
    op("ptradd", 0xBC, &[S16]),
    op("unk_BF", 0xBF, &[]),
    op("unk_C0", 0xC0, &[]),
    op("instr", 0xC1, &[U8]),
    op("dyntbl", 0xC2, &[Abs(Target::DynTable)]),
    mode("short", 0xC3, NoteMode::Short),
    mode("noshort", 0xC4, NoteMode::Large),
    op("dyntbllookup", 0xC5, &[]),
    op("font", 0xC6, &[U8]),
    op("stseq", 0xC7, &[U8, Abs(Target::Ref)]),
    op("sub", 0xC8, &[U8]),
    op("and", 0xC9, &[U8]),
    op("mutebhv", 0xCA, &[U8]),
    op("ldseq", 0xCB, &[Abs(Target::Ref)]),
    op("ldi", 0xCC, &[U8]),
    op("stopchan", 0xCD, &[U8]),
    op("ldptr", 0xCE, &[U16]),
    op("stptrtoseq", 0xCF, &[Abs(Target::Ref)]),
    op("effects", 0xD0, &[U8]),
    op("notealloc", 0xD1, &[U8]),
    op("sustain", 0xD2, &[U8]),
    op("bend", 0xD3, &[S8]),
    op("reverb", 0xD4, &[U8]),
    op("unk_D5", 0xD5, &[U8]),
    op("unk_D6", 0xD6, &[U8]),
    op("vibfreq", 0xD7, &[U8]),
    op("vibdepth", 0xD8, &[U8]),
    op("releaserate", 0xD9, &[U8]),
    op("env", 0xDA, &[Abs(Target::Envelope)]),
    op("transpose", 0xDB, &[S8]),
    op("panweight", 0xDC, &[U8]),
    op("pan", 0xDD, &[U8]),
    op("freqscale", 0xDE, &[S16]),
    op("vol", 0xDF, &[U8]),
    op("volexp", 0xE0, &[U8]),
    op("vibfreqgrad", 0xE1, &[U8, U8, U8]),
    op("vibdepthgrad", 0xE2, &[U8, U8, U8]),
    op("vibdelay", 0xE3, &[U8]),
    dyn_use("dyncall", 0xE4, 0, Target::Same),
    op("reverbidx", 0xE5, &[U8]),
    op("samplebook", 0xE6, &[U8]),
    op("ldparams", 0xE7, &[Abs(Target::Block(8))]),
    op("params", 0xE8, &[U8, U8, U8, S8, U8, U8, U8, U8]),
    op("notepri", 0xE9, &[U8]),
    stop("stop", 0xEA, &[]),
    op("fontinstr", 0xEB, &[U8, U8]),
    op("vibreset", 0xEC, &[]),
    op("gain", 0xED, &[U8]),
    op("bendfine", 0xEE, &[S8]),
    op("unk_EF", 0xEF, &[S16, U8]),
    op("freenotelist", 0xF0, &[]),
    op("allocnotelist", 0xF1, &[U8]),
];

const CHAN_OOT: &[OpDef] = &[op("randptr", 0xBD, &[U16, U16]), op("unk_BE", 0xBE, &[])];

const CHAN_MM: &[OpDef] = &[
    op("unk_A0", 0xA0, &[S16]),
    op("unk_A1", 0xA1, &[]),
    op("unk_A2", 0xA2, &[S16]),
    op("unk_A3", 0xA3, &[]),
    op("unk_A4", 0xA4, &[U8]),
    op("unk_A5", 0xA5, &[]),
    op("unk_A6", 0xA6, &[U8, S16]),
    op("unk_A7", 0xA7, &[U8]),
    op("randptr", 0xA8, &[U16, U16]),
    op("unk_A9", 0xA9, &[]),
    op("unk_AA", 0xAA, &[]),
    op("unk_AB", 0xAB, &[]),
    op("unk_AC", 0xAC, &[]),
    op("unk_AD", 0xAD, &[]),
    op("unk_AE", 0xAE, &[]),
    op("unk_AF", 0xAF, &[]),
    // The decoder reads an s16 here; aseq.h's macro writes a u8.
    op("samplestart", 0xBD, &[S16]),
    op("unk_BE", 0xBE, &[U8]),
];

/// Layer commands other than notes (`0x00..=0xBF`) and `portamento` (`0xC7`).
const LAYER_COMMON: &[OpDef] = &[
    op("ldelay", 0xC0, &[Var]),
    op("shortvel", 0xC1, &[U8]),
    op("transpose", 0xC2, &[S8]),
    op("shortdelay", 0xC3, &[Var]),
    op("legato", 0xC4, &[]),
    op("nolegato", 0xC5, &[]),
    op("instr", 0xC6, &[U8]),
    op("noportamento", 0xC8, &[]),
    op("shortgate", 0xC9, &[U8]),
    op("notepan", 0xCA, &[U8]),
    op("env", 0xCB, &[Abs(Target::Envelope), U8]),
    op("nodrumpan", 0xCC, &[]),
    op("stereo", 0xCD, &[U8]),
    op("bendfine", 0xCE, &[S8]),
    op("releaserate", 0xCF, &[U8]),
    low("ldshortvel", 0xD0, 4, &[]),
    low("ldshortgate", 0xE0, 4, &[]),
];

const LAYER_MM: &[OpDef] = &[
    op("unk_F0", 0xF0, &[S16]),
    op("surroundeffect", 0xF1, &[U8]),
];

pub const PORTAMENTO: u8 = 0xC7;

fn tables(game: Game, level: Level) -> [&'static [OpDef]; 3] {
    match (level, game) {
        (Level::Seq, Game::Oot) => [SEQ_COMMON, &[], FLOW],
        (Level::Seq, Game::Mm) => [SEQ_COMMON, SEQ_MM, FLOW],
        (Level::Chan, Game::Oot) => [CHAN_COMMON, CHAN_OOT, FLOW],
        (Level::Chan, Game::Mm) => [CHAN_COMMON, CHAN_MM, FLOW],
        (Level::Layer, Game::Oot) => [LAYER_COMMON, &[], FLOW],
        (Level::Layer, Game::Mm) => [LAYER_COMMON, LAYER_MM, FLOW],
    }
}

/// Find the definition that decodes `byte` at `level`. Layer notes and
/// portamento return `None`; callers handle them first.
pub fn lookup(game: Game, level: Level, byte: u8) -> Option<&'static OpDef> {
    tables(game, level)
        .into_iter()
        .flatten()
        .find(|d| d.covers(byte))
}

/// Find a definition by mnemonic at `level`.
pub fn by_name(game: Game, level: Level, name: &str) -> Option<&'static OpDef> {
    tables(game, level)
        .into_iter()
        .flatten()
        .find(|d| d.name == name)
}

/// Layer note mnemonics, indexed by `[large][opcode >> 6]`.
pub const NOTE_NAMES: [[&str; 3]; 2] = [
    ["shortdvg", "shortdv", "shortvg"],
    ["notedvg", "notedv", "notevg"],
];
