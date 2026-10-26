//! Read-only access to OoT / MM ROMs: byte-order normalization, the header,
//! the dmadata file table (with Yaz0-compressed files expanded), and the audio
//! tables the sound engine loads sequences, soundfonts and samples from.

pub mod audio;
mod names;
pub mod yaz0;

use std::fmt;

pub use audio::{AudioData, TableEntry};

#[derive(Debug)]
pub enum RomError {
    TooSmall,
    UnknownByteOrder,
    NoDmaData,
    BadFile(usize),
    NoAudioTables(&'static str),
}

impl fmt::Display for RomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RomError::TooSmall => write!(f, "file is too small to be an N64 ROM"),
            RomError::UnknownByteOrder => write!(f, "not an N64 ROM (unrecognized first word)"),
            RomError::NoDmaData => write!(f, "dmadata (file table) not found"),
            RomError::BadFile(i) => write!(
                f,
                "file {i} in dmadata is out of range or fails to decompress"
            ),
            RomError::NoAudioTables(what) => write!(f, "audio tables not found: {what}"),
        }
    }
}

impl std::error::Error for RomError {}

/// How the ROM image was stored on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteOrder {
    /// `.z64`: big-endian, the native order.
    Big,
    /// `.v64`: 16-bit byte-swapped.
    Swapped16,
    /// `.n64`: 32-bit little-endian.
    Little32,
}

/// The two games this crate understands, or anything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameId {
    Oot,
    Mm,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct Header {
    pub title: String,
    /// Four-character game code, e.g. `CZLE` (OoT, NTSC-U) or `NZSE` (MM, US).
    pub code: String,
    pub version: u8,
    pub crc1: u32,
    pub crc2: u32,
}

/// The vanilla name of sequence `id`, from the decomp's sequence table.
pub fn sequence_name(game: GameId, id: usize) -> Option<&'static str> {
    match game {
        GameId::Oot => names::OOT_SEQUENCE_NAMES.get(id).copied(),
        GameId::Mm => names::MM_SEQUENCE_NAMES.get(id).copied(),
        GameId::Unknown => None,
    }
}

impl Header {
    pub fn game(&self) -> GameId {
        match self.code.get(1..3) {
            Some("ZL") => GameId::Oot,
            Some("ZS") => GameId::Mm,
            _ => GameId::Unknown,
        }
    }
}

/// One dmadata entry. `rom_end == 0` means stored uncompressed.
#[derive(Clone, Copy, Debug)]
pub struct DmaEntry {
    pub vrom_start: u32,
    pub vrom_end: u32,
    pub rom_start: u32,
    pub rom_end: u32,
}

impl DmaEntry {
    pub fn vrom_size(&self) -> usize {
        self.vrom_end.saturating_sub(self.vrom_start) as usize
    }

    fn is_deleted(&self) -> bool {
        self.rom_start == 0xFFFF_FFFF && self.rom_end == 0xFFFF_FFFF
    }
}

/// A ROM with every file expanded at its virtual (VROM) address.
pub struct Rom {
    pub header: Header,
    pub byte_order: ByteOrder,
    pub files: Vec<DmaEntry>,
    /// The decompressed address space. For an already-decompressed ROM this is
    /// the ROM itself.
    pub vrom: Vec<u8>,
}

impl Rom {
    pub fn parse(raw: &[u8]) -> Result<Rom, RomError> {
        if raw.len() < 0x1000 {
            return Err(RomError::TooSmall);
        }
        let (data, byte_order) = normalize_byte_order(raw)?;
        let header = parse_header(&data);
        let files = find_dmadata(&data).ok_or(RomError::NoDmaData)?;

        let vrom_len = files.iter().map(|f| f.vrom_end as usize).max().unwrap_or(0);
        let mut vrom = vec![0u8; vrom_len];
        for (i, f) in files.iter().enumerate() {
            if f.is_deleted() || f.vrom_end <= f.vrom_start {
                continue;
            }
            let dst = f.vrom_start as usize..f.vrom_end as usize;
            if f.rom_end == 0 {
                let start = f.rom_start as usize;
                let src = data
                    .get(start..start + f.vrom_size())
                    .ok_or(RomError::BadFile(i))?;
                vrom[dst].copy_from_slice(src);
            } else {
                let src = data
                    .get(f.rom_start as usize..f.rom_end as usize)
                    .ok_or(RomError::BadFile(i))?;
                let out = yaz0::decompress(src).ok_or(RomError::BadFile(i))?;
                let n = out.len().min(f.vrom_size());
                vrom[f.vrom_start as usize..f.vrom_start as usize + n].copy_from_slice(&out[..n]);
            }
        }
        Ok(Rom {
            header,
            byte_order,
            files,
            vrom,
        })
    }

    /// The bytes of the file whose VROM range is exactly `start..end`.
    pub fn file(&self, entry: &DmaEntry) -> &[u8] {
        &self.vrom[entry.vrom_start as usize..entry.vrom_end as usize]
    }

    /// Locate and copy out the audio tables and files.
    pub fn audio(&self) -> Result<AudioData, RomError> {
        audio::locate(self)
    }
}

fn normalize_byte_order(raw: &[u8]) -> Result<(Vec<u8>, ByteOrder), RomError> {
    let mut data = raw.to_vec();
    let order = match &raw[..4] {
        [0x80, 0x37, 0x12, 0x40] => ByteOrder::Big,
        [0x37, 0x80, 0x40, 0x12] => {
            for pair in data.as_chunks_mut::<2>().0 {
                pair.swap(0, 1);
            }
            ByteOrder::Swapped16
        }
        [0x40, 0x12, 0x37, 0x80] => {
            for word in data.as_chunks_mut::<4>().0 {
                word.reverse();
            }
            ByteOrder::Little32
        }
        _ => return Err(RomError::UnknownByteOrder),
    };
    Ok((data, order))
}

fn parse_header(data: &[u8]) -> Header {
    let text = |r: std::ops::Range<usize>| {
        String::from_utf8_lossy(&data[r])
            .trim_end_matches(['\0', ' '])
            .to_string()
    };
    Header {
        title: text(0x20..0x34),
        code: text(0x3B..0x3F),
        version: data[0x3F],
        crc1: be32(data, 0x10),
        crc2: be32(data, 0x14),
    }
}

/// dmadata starts with the entry for the ROM header/IPL3 file ("makerom"),
/// `{0, 0x1060, 0, 0}`, followed by `boot` at 0x1060.
fn find_dmadata(data: &[u8]) -> Option<Vec<DmaEntry>> {
    const FIRST: [u8; 16] = [0, 0, 0, 0, 0, 0, 0x10, 0x60, 0, 0, 0, 0, 0, 0, 0, 0];
    let mut at = 0x1000;
    while at + 32 <= data.len() {
        if data[at..at + 16] == FIRST && be32(data, at + 16) == 0x1060 {
            let mut files = Vec::new();
            let mut p = at;
            while p + 16 <= data.len() {
                let e = DmaEntry {
                    vrom_start: be32(data, p),
                    vrom_end: be32(data, p + 4),
                    rom_start: be32(data, p + 8),
                    rom_end: be32(data, p + 12),
                };
                if e.vrom_start == 0 && e.vrom_end == 0 && !files.is_empty() {
                    break;
                }
                files.push(e);
                p += 16;
            }
            return Some(files);
        }
        at += 16;
    }
    None
}

pub(crate) fn be16(d: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([d[at], d[at + 1]])
}

pub(crate) fn be32(d: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}
