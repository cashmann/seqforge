//! Control-flow-tracing disassembler for Zelda64 sequences.
//!
//! Decoding starts at offset 0 (sequence script) and follows every statically
//! known code reference: channel/layer starts, calls, jumps and branches.
//! Envelopes and fixed-size data blocks referenced by instructions are decoded
//! afterwards. Everything not reached stays as raw `.byte` data, so the output
//! always reassembles to the exact input bytes.
//!
//! Dyntables (jump tables used by `dyncall`, `dynjump` and `dynldlayer`) are
//! followed heuristically: entries are read while they point at unclaimed
//! bytes or at the start of code of the expected kind.
//!
//! Not followed (stays raw): computed pointers and self-modifying stores.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write;

use crate::Section;
use crate::abi::{self, Arg, Flow, Game, Level, NoteMode, OpDef, Target};

#[derive(Clone, Debug)]
pub(crate) enum Operand {
    Num(i64),
    Hex(i64),
    /// Variable-length value; `long` = encoded in 2 bytes.
    Var {
        value: u16,
        long: bool,
    },
    /// Address operand; `target` is the absolute offset it resolves to,
    /// `raw` the encoded value (displacement for relative operands).
    Addr {
        target: i64,
        raw: i64,
        rel: bool,
    },
}

#[derive(Clone, Debug)]
struct Unit {
    len: usize,
    section: Section,
    text: UnitText,
}

#[derive(Clone, Debug)]
enum UnitText {
    Insn {
        name: &'static str,
        ops: Vec<Operand>,
    },
    EnvPoint {
        delay: i16,
        arg: i16,
    },
    Entry {
        target: usize,
    },
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Work {
    off: usize,
    level: Level,
    /// Channel note mode (for channels) or the layer's decode mode (for layers).
    large: bool,
    /// The channel's current dyntable, if set by `dyntbl` on this path.
    dyntbl: Option<usize>,
}

/// Disassembly statistics, useful for corpus checks.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub total: usize,
    pub code_bytes: usize,
    pub data_bytes: usize,
    pub raw_bytes: usize,
    /// Code paths abandoned because they ran into already-claimed bytes,
    /// an unknown opcode, or the end of the file.
    pub conflicts: usize,
}

pub struct Disassembly {
    pub text: String,
    pub stats: Stats,
}

struct Decoder<'a> {
    game: Game,
    data: &'a [u8],
    units: BTreeMap<usize, Unit>,
    claimed: Vec<bool>,
    /// Requested label offsets and what kind of thing referenced them.
    wanted: BTreeMap<usize, Target>,
    data_work: Vec<(usize, Target)>,
    /// Pending dyntables: (table offset, what its entries point at).
    tables: Vec<(usize, Work)>,
    conflicts: usize,
}

pub fn disassemble(game: Game, data: &[u8]) -> Disassembly {
    let mut d = Decoder {
        game,
        data,
        units: BTreeMap::new(),
        claimed: vec![false; data.len()],
        wanted: BTreeMap::new(),
        data_work: Vec::new(),
        tables: Vec::new(),
        conflicts: 0,
    };
    d.trace_code();
    d.trace_data();
    d.render()
}

impl Decoder<'_> {
    fn trace_code(&mut self) {
        let mut queue = VecDeque::new();
        if !self.data.is_empty() {
            queue.push_back(Work {
                off: 0,
                level: Level::Seq,
                large: false,
                dyntbl: None,
            });
            self.wanted.insert(0, Target::Same);
        }
        // Statically reachable code first, then dyntable entries (which may
        // reveal more code), so heuristics never pre-empt certain decodes.
        loop {
            while let Some(w) = queue.pop_front() {
                self.trace_path(w, &mut queue);
            }
            if self.tables.is_empty() {
                break;
            }
            for (off, entry) in std::mem::take(&mut self.tables) {
                self.trace_table(off, entry, &mut queue);
            }
        }
    }

    fn trace_table(&mut self, start: usize, entry: Work, queue: &mut VecDeque<Work>) {
        if self.units.contains_key(&start) {
            return;
        }
        let section = Self::section_for(entry.level, entry.large);
        let mut off = start;
        while off + 2 <= self.data.len() && off - start < 2 * 64 {
            if self.claimed[off]
                || self.claimed[off + 1]
                || (off != start && self.wanted.contains_key(&off))
            {
                break;
            }
            let target = u16::from_be_bytes([self.data[off], self.data[off + 1]]) as usize;
            let plausible = target < self.data.len()
                && (target != 0 || entry.level == Level::Seq)
                && !(start..off + 2).contains(&target)
                && match self.units.get(&target) {
                    Some(u) => u.section == section,
                    None => !self.claimed[target],
                };
            if !plausible {
                break;
            }
            self.claim(off, 2, Section::Table, UnitText::Entry { target });
            self.wanted.entry(target).or_insert(Target::Same);
            queue.push_back(Work {
                off: target,
                dyntbl: None,
                ..entry
            });
            off += 2;
        }
    }

    fn section_for(level: Level, large: bool) -> Section {
        match level {
            Level::Seq => Section::Sequence,
            Level::Chan => Section::Channel,
            Level::Layer => Section::Layer { large },
        }
    }

    fn trace_path(&mut self, mut w: Work, queue: &mut VecDeque<Work>) {
        loop {
            let section = Self::section_for(w.level, w.large);
            if let Some(existing) = self.units.get(&w.off) {
                if existing.section != section {
                    self.conflicts += 1;
                }
                return;
            }
            let Some((len, name, ops, def)) = self.decode_insn(w) else {
                self.conflicts += 1;
                return;
            };
            if self.claimed[w.off..w.off + len].iter().any(|&c| c) {
                self.conflicts += 1;
                return;
            }
            for c in &mut self.claimed[w.off..w.off + len] {
                *c = true;
            }

            let mut flow = Flow::Next;
            if let Some(def) = def {
                flow = def.flow;
                if w.level == Level::Chan {
                    match def.mode {
                        NoteMode::Short => w.large = false,
                        NoteMode::Large => w.large = true,
                        NoteMode::Keep => {}
                    }
                }
                let addr_args = def
                    .args
                    .iter()
                    .filter(|a| matches!(a, Arg::Abs(_) | Arg::Rel8(_) | Arg::Rel16(_)));
                let addr_ops = ops.iter().filter(|o| matches!(o, Operand::Addr { .. }));
                for (arg, operand) in addr_args.zip(addr_ops) {
                    let Operand::Addr { target, .. } = *operand else {
                        continue;
                    };
                    let kind = match arg {
                        Arg::Abs(t) | Arg::Rel8(t) | Arg::Rel16(t) => *t,
                        _ => continue,
                    };
                    if kind == Target::DynTable && (0..self.data.len() as i64).contains(&target) {
                        match w.level {
                            Level::Seq => self.tables.push((target as usize, w)),
                            _ => w.dyntbl = Some(target as usize),
                        }
                    }
                    self.reference(target, kind, w, queue);
                }
                if let (Some(entry_kind), Some(tbl)) = (def.dyn_use, w.dyntbl) {
                    let entry = match entry_kind {
                        Target::Layer => Work {
                            level: Level::Layer,
                            ..w
                        },
                        _ => w,
                    };
                    self.tables.push((tbl, entry));
                }
            }

            self.units.insert(
                w.off,
                Unit {
                    len,
                    section,
                    text: UnitText::Insn { name, ops },
                },
            );
            if flow == Flow::Stop {
                return;
            }
            w.off += len;
            if w.off >= self.data.len() {
                self.conflicts += 1;
                return;
            }
        }
    }

    fn reference(&mut self, target: i64, kind: Target, from: Work, queue: &mut VecDeque<Work>) {
        if target < 0 || target as usize >= self.data.len() {
            return;
        }
        let off = target as usize;
        self.wanted.entry(off).or_insert(kind);
        let work = match kind {
            Target::Same => Some(Work { off, ..from }),
            Target::Chan => Some(Work {
                off,
                level: Level::Chan,
                large: false,
                dyntbl: None,
            }),
            // A layer decodes in the note mode its channel was in when it started it.
            Target::Layer => Some(Work {
                off,
                level: Level::Layer,
                large: from.large,
                dyntbl: None,
            }),
            Target::Envelope | Target::Block(_) => {
                self.data_work.push((off, kind));
                None
            }
            Target::Ref | Target::DynTable => None,
        };
        if let Some(work) = work {
            queue.push_back(work);
        }
    }

    fn trace_data(&mut self) {
        let work = std::mem::take(&mut self.data_work);
        for (off, kind) in work {
            if self.units.contains_key(&off) {
                continue;
            }
            match kind {
                Target::Envelope => self.trace_envelope(off),
                Target::Block(n) => {
                    let end = off + n as usize;
                    if end <= self.data.len() && !self.claimed[off..end].iter().any(|&c| c) {
                        self.claim(off, n as usize, Section::Data, UnitText::Block);
                    }
                }
                _ => {}
            }
        }
    }

    fn trace_envelope(&mut self, start: usize) {
        let mut off = start;
        // Envelopes are short; cap the walk so garbage can't claim the file.
        for _ in 0..64 {
            if off + 4 > self.data.len() || self.claimed[off..off + 4].iter().any(|&c| c) {
                return;
            }
            let delay = i16::from_be_bytes([self.data[off], self.data[off + 1]]);
            let arg = i16::from_be_bytes([self.data[off + 2], self.data[off + 3]]);
            self.claim(off, 4, Section::Envelope, UnitText::EnvPoint { delay, arg });
            off += 4;
            // 0 = disable, -1 = hang, -2 = goto, -3 = restart
            if (-3..=0).contains(&delay) {
                return;
            }
        }
    }

    fn claim(&mut self, off: usize, len: usize, section: Section, text: UnitText) {
        for c in &mut self.claimed[off..off + len] {
            *c = true;
        }
        self.units.insert(off, Unit { len, section, text });
    }

    /// Decode one instruction. Returns (length, mnemonic, operands, table def).
    fn decode_insn(
        &self,
        w: Work,
    ) -> Option<(usize, &'static str, Vec<Operand>, Option<&'static OpDef>)> {
        let mut r = Reader {
            data: self.data,
            pos: w.off,
        };
        let opcode = r.u8()?;

        if w.level == Level::Layer && opcode < 0xC0 {
            return self
                .decode_note(&mut r, opcode, w.large)
                .map(|(n, o)| (r.pos - w.off, n, o, None));
        }
        if w.level == Level::Layer && opcode == abi::PORTAMENTO {
            let mode = r.u8()?;
            let target = r.u8()?;
            let time = if mode & 0x80 != 0 {
                Operand::Num(r.u8()? as i64)
            } else {
                r.var()?
            };
            let ops = vec![Operand::Hex(mode as i64), Operand::Num(target as i64), time];
            return Some((r.pos - w.off, "portamento", ops, None));
        }

        let def = abi::lookup(self.game, w.level, opcode)?;
        let mut ops = Vec::new();
        if def.low_bits > 0 {
            ops.push(Operand::Num((opcode - def.op) as i64));
        }
        for arg in def.args {
            let operand = match arg {
                Arg::U8 => Operand::Num(r.u8()? as i64),
                Arg::S8 => Operand::Num(r.u8()? as i8 as i64),
                Arg::U16 => Operand::Hex(r.u16()? as i64),
                Arg::S16 => Operand::Num(r.u16()? as i16 as i64),
                Arg::Var => r.var()?,
                Arg::Abs(_) => {
                    let v = r.u16()? as i64;
                    Operand::Addr {
                        target: v,
                        raw: v,
                        rel: false,
                    }
                }
                Arg::Rel8(_) => {
                    let disp = r.u8()? as i8 as i64;
                    Operand::Addr {
                        target: r.pos as i64 + disp,
                        raw: disp,
                        rel: true,
                    }
                }
                Arg::Rel16(_) => {
                    let disp = r.u16()? as i16 as i64;
                    Operand::Addr {
                        target: r.pos as i64 + disp,
                        raw: disp,
                        rel: true,
                    }
                }
            };
            ops.push(operand);
        }
        Some((r.pos - w.off, def.name, ops, Some(def)))
    }

    fn decode_note(
        &self,
        r: &mut Reader,
        opcode: u8,
        large: bool,
    ) -> Option<(&'static str, Vec<Operand>)> {
        let kind = (opcode >> 6) as usize;
        let name = abi::NOTE_NAMES[large as usize][kind];
        let mut ops = vec![Operand::Num((opcode & 0x3F) as i64)];
        match (large, kind) {
            (true, 0) => {
                ops.push(r.var()?);
                ops.push(Operand::Num(r.u8()? as i64));
                ops.push(Operand::Num(r.u8()? as i64));
            }
            (true, 1) => {
                ops.push(r.var()?);
                ops.push(Operand::Num(r.u8()? as i64));
            }
            (true, _) => {
                ops.push(Operand::Num(r.u8()? as i64));
                ops.push(Operand::Num(r.u8()? as i64));
            }
            (false, 0) => ops.push(r.var()?),
            (false, _) => {}
        }
        Some((name, ops))
    }

    fn label_name(&self, off: usize) -> Option<String> {
        let prefix = match self.units.get(&off) {
            Some(u) => match u.section {
                Section::Sequence => "seq",
                Section::Channel => "chan",
                Section::Layer { .. } => "layer",
                Section::Envelope => "env",
                Section::Table => "tbl",
                Section::Data => "blk",
            },
            None if !self.claimed[off] => "data",
            // Points into the middle of a decoded instruction.
            None => return None,
        };
        Some(format!("{prefix}_{off:04X}"))
    }

    fn render(&self) -> Disassembly {
        let labels: BTreeMap<usize, String> = self
            .wanted
            .keys()
            .filter_map(|&off| self.label_name(off).map(|n| (off, n)))
            .collect();

        let mut out = String::new();
        let _ = writeln!(out, "; SeqForge disassembly ({} bytes)", self.data.len());
        let _ = writeln!(out, ".game {}", self.game);

        let mut stats = Stats {
            total: self.data.len(),
            conflicts: self.conflicts,
            ..Default::default()
        };
        let mut current: Option<Section> = None;
        let mut off = 0;
        while off < self.data.len() {
            if let Some(unit) = self.units.get(&off) {
                if current != Some(unit.section) {
                    let _ = writeln!(out, "\n{}", unit.section.directive());
                    current = Some(unit.section);
                }
                if let Some(l) = labels.get(&off) {
                    let _ = writeln!(out, "{l}:");
                }
                match &unit.text {
                    UnitText::Insn { name, ops } => {
                        stats.code_bytes += unit.len;
                        let _ = writeln!(out, "    {}", format_insn(name, ops, &labels));
                    }
                    UnitText::EnvPoint { delay, arg } => {
                        stats.data_bytes += unit.len;
                        let _ = writeln!(out, "    {}", format_env(*delay, *arg));
                    }
                    UnitText::Entry { target } => {
                        stats.data_bytes += unit.len;
                        let op = Operand::Addr {
                            target: *target as i64,
                            raw: *target as i64,
                            rel: false,
                        };
                        let _ = writeln!(out, "    {}", format_insn("entry", &[op], &labels));
                    }
                    UnitText::Block => {
                        stats.data_bytes += unit.len;
                        write_bytes(&mut out, &self.data[off..off + unit.len]);
                    }
                }
                off += unit.len;
                continue;
            }

            // Raw run: up to the next decoded unit, split at labels.
            let mut end = off + 1;
            while end < self.data.len()
                && !self.units.contains_key(&end)
                && !labels.contains_key(&end)
            {
                end += 1;
            }
            if current != Some(Section::Data) {
                let _ = writeln!(out, "\n{}", Section::Data.directive());
                current = Some(Section::Data);
            }
            if let Some(l) = labels.get(&off) {
                let _ = writeln!(out, "{l}:");
            }
            stats.raw_bytes += end - off;
            write_bytes(&mut out, &self.data[off..end]);
            off = end;
        }
        Disassembly { text: out, stats }
    }
}

fn format_insn(name: &str, ops: &[Operand], labels: &BTreeMap<usize, String>) -> String {
    if ops.is_empty() {
        return name.to_string();
    }
    let parts: Vec<String> = ops
        .iter()
        .map(|o| match *o {
            Operand::Num(v) => v.to_string(),
            Operand::Hex(v) => format!("0x{v:X}"),
            Operand::Var { value, long } if long && value < 0x80 => format!("long({value})"),
            Operand::Var { value, .. } => value.to_string(),
            Operand::Addr { target, raw, rel } => {
                let label = (target >= 0)
                    .then(|| labels.get(&(target as usize)))
                    .flatten();
                match (label, rel) {
                    (Some(l), _) => l.clone(),
                    (None, true) => format!("${raw}"),
                    (None, false) => format!("$0x{raw:04X}"),
                }
            }
        })
        .collect();
    format!("{name} {}", parts.join(", "))
}

fn format_env(delay: i16, arg: i16) -> String {
    match (delay, arg) {
        (0, 0) => "disable".into(),
        (-1, 0) => "hang".into(),
        (-2, i) => format!("goto {i}"),
        (-3, 0) => "restart".into(),
        (d, a) => format!("point {d}, {a}"),
    }
}

fn write_bytes(out: &mut String, bytes: &[u8]) {
    for chunk in bytes.chunks(16) {
        let hex: Vec<String> = chunk.iter().map(|b| format!("0x{b:02X}")).collect();
        let _ = writeln!(out, "    .byte {}", hex.join(", "));
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn u8(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }

    fn var(&mut self) -> Option<Operand> {
        let first = self.u8()?;
        if first & 0x80 == 0 {
            return Some(Operand::Var {
                value: first as u16,
                long: false,
            });
        }
        let value = (((first & 0x7F) as u16) << 8) | self.u8()? as u16;
        Some(Operand::Var { value, long: true })
    }
}
