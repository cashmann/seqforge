//! Two-pass assembler for the disassembler's text format.

use std::collections::HashMap;
use std::fmt;

use crate::Section;
use crate::abi::{self, Arg, Game, Level};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsmError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for AsmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for AsmError {}

#[derive(Clone, Debug)]
enum Op {
    Num(i64),
    Long(i64),
    Raw(i64),
    Label(String),
}

#[derive(Debug)]
enum Item {
    Label(String),
    Insn {
        section: Section,
        name: String,
        ops: Vec<Op>,
    },
    Bytes(Vec<u8>),
}

struct Line {
    no: usize,
    item: Item,
}

/// Assemble source text into sequence bytes. `game` overrides any `.game`
/// directive in the source; with neither, OoT is assumed.
pub fn assemble(src: &str, game: Option<Game>) -> Result<Vec<u8>, AsmError> {
    let (lines, src_game) = parse(src)?;
    let game = game.or(src_game).unwrap_or(Game::Oot);

    // Pass 1: lay out. Sizes never depend on label values.
    let mut labels = HashMap::new();
    let mut pc = 0usize;
    for line in &lines {
        match &line.item {
            Item::Label(name) => {
                if labels.insert(name.clone(), pc).is_some() {
                    return Err(err(line.no, format!("duplicate label `{name}`")));
                }
            }
            Item::Bytes(b) => pc += b.len(),
            Item::Insn { section, name, ops } => {
                pc += encode(game, *section, name, ops, pc, None)
                    .map_err(|m| err(line.no, m))?
                    .len();
            }
        }
    }

    // Pass 2: emit with labels resolved.
    let mut out = Vec::with_capacity(pc);
    for line in &lines {
        match &line.item {
            Item::Label(_) => {}
            Item::Bytes(b) => out.extend_from_slice(b),
            Item::Insn { section, name, ops } => {
                let bytes = encode(game, *section, name, ops, out.len(), Some(&labels))
                    .map_err(|m| err(line.no, m))?;
                out.extend(bytes);
            }
        }
    }
    Ok(out)
}

fn err(line: usize, message: impl Into<String>) -> AsmError {
    AsmError {
        line,
        message: message.into(),
    }
}

fn parse(src: &str) -> Result<(Vec<Line>, Option<Game>), AsmError> {
    let mut lines = Vec::new();
    let mut section: Option<Section> = None;
    let mut game = None;

    for (i, raw) in src.lines().enumerate() {
        let no = i + 1;
        let text = raw.split(';').next().unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        if let Some(name) = text.strip_suffix(':') {
            if !is_ident(name) {
                return Err(err(no, format!("invalid label `{name}`")));
            }
            lines.push(Line {
                no,
                item: Item::Label(name.to_string()),
            });
            continue;
        }

        let (head, rest) = match text.split_once(char::is_whitespace) {
            Some((h, r)) => (h, r.trim()),
            None => (text, ""),
        };
        match head {
            ".game" => {
                game = Some(
                    Game::from_name(rest)
                        .ok_or_else(|| err(no, format!("unknown game `{rest}`")))?,
                );
                continue;
            }
            ".sequence" => section = Some(Section::Sequence),
            ".channel" => section = Some(Section::Channel),
            ".layer" => {
                section = Some(match rest {
                    "large" => Section::Layer { large: true },
                    "short" => Section::Layer { large: false },
                    _ => return Err(err(no, "`.layer` needs `large` or `short`")),
                })
            }
            ".envelope" => section = Some(Section::Envelope),
            ".table" => section = Some(Section::Table),
            ".data" => section = Some(Section::Data),
            ".byte" => {
                let mut bytes = Vec::new();
                for op in split_ops(rest) {
                    let v = parse_int(op).ok_or_else(|| err(no, format!("bad byte `{op}`")))?;
                    if !(-128..=255).contains(&v) {
                        return Err(err(no, format!("byte out of range: {v}")));
                    }
                    bytes.push(v as u8);
                }
                lines.push(Line {
                    no,
                    item: Item::Bytes(bytes),
                });
            }
            _ if head.starts_with('.') => {
                return Err(err(no, format!("unknown directive `{head}`")));
            }
            _ => {
                let section =
                    section.ok_or_else(|| err(no, "instruction before any section directive"))?;
                let ops = split_ops(rest)
                    .map(|s| parse_op(s).ok_or_else(|| err(no, format!("bad operand `{s}`"))))
                    .collect::<Result<Vec<_>, _>>()?;
                lines.push(Line {
                    no,
                    item: Item::Insn {
                        section,
                        name: head.to_string(),
                        ops,
                    },
                });
            }
        }
    }
    Ok((lines, game))
}

fn split_ops(s: &str) -> impl Iterator<Item = &str> {
    s.split(',').map(str::trim).filter(|s| !s.is_empty())
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

fn parse_int(s: &str) -> Option<i64> {
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let v = match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        Some(hex) => i64::from_str_radix(hex, 16).ok()?,
        None => body.parse::<i64>().ok()?,
    };
    Some(if neg { -v } else { v })
}

fn parse_op(s: &str) -> Option<Op> {
    if let Some(inner) = s.strip_prefix("long(").and_then(|r| r.strip_suffix(')')) {
        return parse_int(inner.trim()).map(Op::Long);
    }
    if let Some(raw) = s.strip_prefix('$') {
        return parse_int(raw).map(Op::Raw);
    }
    if let Some(v) = parse_int(s) {
        return Some(Op::Num(v));
    }
    is_ident(s).then(|| Op::Label(s.to_string()))
}

type Labels = HashMap<String, usize>;

/// Encode one instruction at `pc`. With `labels == None` (layout pass),
/// address operands encode as zero; lengths are still exact.
fn encode(
    game: Game,
    section: Section,
    name: &str,
    ops: &[Op],
    pc: usize,
    labels: Option<&Labels>,
) -> Result<Vec<u8>, String> {
    match section {
        Section::Envelope => encode_env(name, ops),
        Section::Table => encode_entry(name, ops, labels),
        Section::Data => Err(format!("instruction `{name}` in .data (use .byte)")),
        Section::Layer { large } => {
            if let Some(kind) = abi::NOTE_NAMES[large as usize]
                .iter()
                .position(|n| *n == name)
            {
                return encode_note(large, kind, ops);
            }
            if abi::NOTE_NAMES[!large as usize].contains(&name) {
                let mode = if large { "large" } else { "short" };
                return Err(format!(
                    "`{name}` is not valid in a `.layer {mode}` section"
                ));
            }
            if name == "portamento" {
                return encode_portamento(ops);
            }
            encode_table(game, Level::Layer, name, ops, pc, labels)
        }
        Section::Sequence | Section::Channel => {
            encode_table(game, section.level().unwrap(), name, ops, pc, labels)
        }
    }
}

fn want_ops(name: &str, ops: &[Op], n: usize) -> Result<(), String> {
    if ops.len() != n {
        return Err(format!("`{name}` takes {n} operand(s), got {}", ops.len()));
    }
    Ok(())
}

fn num(op: &Op, lo: i64, hi: i64) -> Result<i64, String> {
    match op {
        Op::Num(v) | Op::Raw(v) if (lo..=hi).contains(v) => Ok(*v),
        Op::Num(v) | Op::Raw(v) => Err(format!("value {v} out of range {lo}..={hi}")),
        Op::Long(_) => Err("`long(...)` is only valid for variable-length operands".into()),
        Op::Label(l) => Err(format!("expected a number, got label `{l}`")),
    }
}

fn push_var(out: &mut Vec<u8>, op: &Op) -> Result<(), String> {
    let (v, long) = match op {
        Op::Long(v) => (*v, true),
        other => (num(other, 0, 0x7FFF)?, false),
    };
    if !(0..=0x7FFF).contains(&v) {
        return Err(format!("variable-length value {v} out of range 0..=32767"));
    }
    if long || v >= 0x80 {
        out.push(0x80 | (v >> 8) as u8);
        out.push(v as u8);
    } else {
        out.push(v as u8);
    }
    Ok(())
}

fn encode_note(large: bool, kind: usize, ops: &[Op]) -> Result<Vec<u8>, String> {
    let name = abi::NOTE_NAMES[large as usize][kind];
    let arity = match (large, kind) {
        (true, 0) => 4,
        (true, _) => 3,
        (false, 0) => 2,
        (false, _) => 1,
    };
    want_ops(name, ops, arity)?;
    let pitch = num(&ops[0], 0, 63)?;
    let mut out = vec![((kind as u8) << 6) | pitch as u8];
    match (large, kind) {
        (true, 0) => {
            push_var(&mut out, &ops[1])?;
            out.push(num(&ops[2], 0, 255)? as u8);
            out.push(num(&ops[3], 0, 255)? as u8);
        }
        (true, 1) => {
            push_var(&mut out, &ops[1])?;
            out.push(num(&ops[2], 0, 255)? as u8);
        }
        (true, _) => {
            out.push(num(&ops[1], 0, 255)? as u8);
            out.push(num(&ops[2], 0, 255)? as u8);
        }
        (false, 0) => push_var(&mut out, &ops[1])?,
        (false, _) => {}
    }
    Ok(out)
}

fn encode_portamento(ops: &[Op]) -> Result<Vec<u8>, String> {
    want_ops("portamento", ops, 3)?;
    let mode = num(&ops[0], 0, 255)? as u8;
    let mut out = vec![abi::PORTAMENTO, mode, num(&ops[1], 0, 255)? as u8];
    if mode & 0x80 != 0 {
        out.push(num(&ops[2], 0, 255)? as u8);
    } else {
        push_var(&mut out, &ops[2])?;
    }
    Ok(out)
}

fn encode_env(name: &str, ops: &[Op]) -> Result<Vec<u8>, String> {
    let (delay, arg) = match name {
        "point" => {
            want_ops(name, ops, 2)?;
            (num(&ops[0], -32768, 65535)?, num(&ops[1], -32768, 65535)?)
        }
        "disable" | "hang" | "restart" => {
            want_ops(name, ops, 0)?;
            (
                match name {
                    "disable" => 0,
                    "hang" => -1,
                    _ => -3,
                },
                0,
            )
        }
        "goto" => {
            want_ops(name, ops, 1)?;
            (-2, num(&ops[0], -32768, 65535)?)
        }
        _ => return Err(format!("unknown envelope command `{name}`")),
    };
    let mut out = (delay as u16).to_be_bytes().to_vec();
    out.extend((arg as u16).to_be_bytes());
    Ok(out)
}

fn encode_entry(name: &str, ops: &[Op], labels: Option<&Labels>) -> Result<Vec<u8>, String> {
    if name != "entry" {
        return Err(format!("unknown table command `{name}` (expected `entry`)"));
    }
    want_ops(name, ops, 1)?;
    let v = match resolve(&ops[0], labels)? {
        Some(v) => v,
        None => num(&ops[0], -32768, 65535)?,
    };
    Ok((v as u16).to_be_bytes().to_vec())
}

fn arg_size(arg: &Arg, op: &Op) -> usize {
    match arg {
        Arg::U8 | Arg::S8 | Arg::Rel8(_) => 1,
        Arg::U16 | Arg::S16 | Arg::Abs(_) | Arg::Rel16(_) => 2,
        Arg::Var => match op {
            Op::Long(_) => 2,
            Op::Num(v) | Op::Raw(v) if *v >= 0x80 => 2,
            _ => 1,
        },
    }
}

fn resolve(op: &Op, labels: Option<&Labels>) -> Result<Option<i64>, String> {
    match (op, labels) {
        (Op::Label(_), None) => Ok(Some(0)),
        (Op::Label(l), Some(map)) => map
            .get(l)
            .map(|&v| Some(v as i64))
            .ok_or_else(|| format!("undefined label `{l}`")),
        (Op::Num(v), _) => Ok(Some(*v)),
        // Raw: encoded verbatim, no displacement math.
        (Op::Raw(_), _) => Ok(None),
        (Op::Long(_), _) => Err("`long(...)` is only valid for variable-length operands".into()),
    }
}

fn encode_table(
    game: Game,
    level: Level,
    name: &str,
    ops: &[Op],
    pc: usize,
    labels: Option<&Labels>,
) -> Result<Vec<u8>, String> {
    let def = abi::by_name(game, level, name)
        .ok_or_else(|| format!("unknown {level:?} instruction `{name}` for {game}"))?;
    let has_low = def.low_bits > 0;
    want_ops(name, ops, def.args.len() + has_low as usize)?;

    let mut opcode = def.op;
    let args = if has_low {
        let low = num(&ops[0], 0, def.op_count() as i64 - 1)?;
        opcode += low as u8;
        &ops[1..]
    } else {
        ops
    };

    let end = pc
        + 1
        + def
            .args
            .iter()
            .zip(args)
            .map(|(a, o)| arg_size(a, o))
            .sum::<usize>();
    let mut out = vec![opcode];
    for (arg, op) in def.args.iter().zip(args) {
        match arg {
            Arg::U8 => out.push(num(op, -128, 255)? as u8),
            Arg::S8 => out.push(num(op, -128, 255)? as u8),
            Arg::U16 | Arg::S16 => out.extend((num(op, -32768, 65535)? as u16).to_be_bytes()),
            Arg::Var => push_var(&mut out, op)?,
            Arg::Abs(_) => {
                let v = match resolve(op, labels)? {
                    Some(v) => v,
                    None => num(op, -32768, 65535)?,
                };
                if !(0..=0xFFFF).contains(&v) && !matches!(op, Op::Raw(_)) {
                    return Err(format!("address {v} does not fit in 16 bits"));
                }
                out.extend((v as u16).to_be_bytes());
            }
            Arg::Rel8(_) => {
                let disp = match resolve(op, labels)? {
                    Some(target) if labels.is_some() || !matches!(op, Op::Label(_)) => {
                        target - end as i64
                    }
                    Some(_) => 0,
                    None => num(op, -128, 127)?,
                };
                if labels.is_some() && !(-128..=127).contains(&disp) {
                    return Err(format!(
                        "relative branch out of range ({disp} bytes, max ±127); use `jump`"
                    ));
                }
                out.push(disp as i8 as u8);
            }
            Arg::Rel16(_) => {
                let disp = match resolve(op, labels)? {
                    Some(target) if labels.is_some() || !matches!(op, Op::Label(_)) => {
                        target - end as i64
                    }
                    Some(_) => 0,
                    None => num(op, -32768, 32767)?,
                };
                out.extend((disp as i16 as u16).to_be_bytes());
            }
        }
    }
    Ok(out)
}
