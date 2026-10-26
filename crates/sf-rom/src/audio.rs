//! The four audio tables in `code` and the three audio files they index.
//!
//! In both OoT and MM the tables sit back to back in the `code` file's data:
//! the soundfont table, the sequence→font table, the sequence table and the
//! sample bank table. Each `AudioTable` is a 16-byte header (`s16` entry
//! count, then zeros in ROM) followed by 16-byte entries. Entry addresses are
//! offsets into the matching file (Audioseq, Audiobank, Audiotable); an entry
//! with size 0 is a pointer whose address field is the index of the real entry.

use crate::{GameId, Rom, RomError, be16, be32};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableEntry {
    /// Offset into the table's file (or, for a pointer entry, the real index).
    pub addr: u32,
    pub size: u32,
    pub medium: i8,
    pub cache_policy: i8,
    pub short1: i16,
    pub short2: i16,
    pub short3: i16,
}

impl TableEntry {
    fn read(d: &[u8], at: usize) -> TableEntry {
        TableEntry {
            addr: be32(d, at),
            size: be32(d, at + 4),
            medium: d[at + 8] as i8,
            cache_policy: d[at + 9] as i8,
            short1: be16(d, at + 10) as i16,
            short2: be16(d, at + 12) as i16,
            short3: be16(d, at + 14) as i16,
        }
    }

    pub fn is_pointer(&self) -> bool {
        self.size == 0
    }
}

/// Font table entries pack their counts into the short fields.
#[derive(Clone, Copy, Debug)]
pub struct FontInfo {
    pub sample_bank_1: u8,
    pub sample_bank_2: u8,
    pub num_instruments: u8,
    pub num_drums: u8,
    pub num_sfx: u16,
}

/// Everything the audio engine reads from ROM, copied out of it.
#[derive(Clone)]
pub struct AudioData {
    pub game: GameId,
    pub seq_table: Vec<TableEntry>,
    pub font_table: Vec<TableEntry>,
    pub sample_bank_table: Vec<TableEntry>,
    /// Raw `gSequenceFontTable`: a `u16` offset per sequence, then at each
    /// offset a count byte followed by that many font ids.
    pub seq_font_table: Vec<u8>,
    pub audioseq: Vec<u8>,
    pub audiobank: Vec<u8>,
    pub audiotable: Vec<u8>,
}

impl AudioData {
    pub fn num_sequences(&self) -> usize {
        self.seq_table.len()
    }

    /// Follow a pointer entry to the index of the real entry.
    pub fn real_index(table: &[TableEntry], id: usize) -> usize {
        match table.get(id) {
            Some(e) if e.is_pointer() => e.addr as usize,
            _ => id,
        }
    }

    pub fn sequence(&self, id: usize) -> Option<&[u8]> {
        let e = self.seq_table.get(Self::real_index(&self.seq_table, id))?;
        let start = e.addr as usize;
        self.audioseq.get(start..start + e.size as usize)
    }

    pub fn font(&self, id: usize) -> Option<&[u8]> {
        let e = self
            .font_table
            .get(Self::real_index(&self.font_table, id))?;
        let start = e.addr as usize;
        self.audiobank.get(start..start + e.size as usize)
    }

    pub fn font_info(&self, id: usize) -> Option<FontInfo> {
        let e = self.font_table.get(id)?;
        Some(FontInfo {
            sample_bank_1: (e.short1 as u16 >> 8) as u8,
            sample_bank_2: e.short1 as u8,
            num_instruments: (e.short2 as u16 >> 8) as u8,
            num_drums: e.short2 as u8,
            num_sfx: e.short3 as u16,
        })
    }

    /// Fonts a sequence loads, in table order. The engine's default font is
    /// the last one; `font n` in a channel picks `fonts[len - 1 - n]`.
    pub fn seq_fonts(&self, id: usize) -> Vec<u8> {
        let t = &self.seq_font_table;
        if t.len() < 2 * (id + 1) {
            return Vec::new();
        }
        let at = be16(t, 2 * id) as usize;
        let Some(&n) = t.get(at) else {
            return Vec::new();
        };
        t.get(at + 1..at + 1 + n as usize)
            .map(<[u8]>::to_vec)
            .unwrap_or_default()
    }

    /// Replace sequence `id` with `data` (appended to Audioseq) and, if given,
    /// set the fonts it loads. This is the in-memory version of ROM injection.
    pub fn replace_sequence(&mut self, id: usize, data: &[u8], fonts: Option<&[u8]>) {
        if id >= self.seq_table.len() {
            self.seq_table.resize(
                id + 1,
                TableEntry {
                    addr: 0,
                    size: 0,
                    medium: 2,
                    cache_policy: 2,
                    short1: 0,
                    short2: 0,
                    short3: 0,
                },
            );
        }
        while !self.audioseq.len().is_multiple_of(16) {
            self.audioseq.push(0);
        }
        let addr = self.audioseq.len() as u32;
        self.audioseq.extend_from_slice(data);
        let e = &mut self.seq_table[id];
        e.addr = addr;
        e.size = data.len() as u32;
        if let Some(fonts) = fonts {
            self.set_seq_fonts(id, fonts);
        }
    }

    /// Replace soundfont `id` with `data` (appended to Audiobank). `meta` is
    /// the font table entry's last 8 bytes (medium, cache policy, sample banks,
    /// instrument/drum/SFX counts), the same layout as MMR's `.bankmeta`.
    pub fn replace_font(&mut self, id: usize, data: &[u8], meta: [u8; 8]) {
        let blank = TableEntry {
            addr: 0,
            size: 0,
            medium: 2,
            cache_policy: 2,
            short1: 0,
            short2: 0,
            short3: 0,
        };
        if id >= self.font_table.len() {
            self.font_table.resize(id + 1, blank);
        }
        while !self.audiobank.len().is_multiple_of(16) {
            self.audiobank.push(0);
        }
        let addr = self.audiobank.len() as u32;
        self.audiobank.extend_from_slice(data);
        let mut entry = [0u8; 16];
        entry[8..].copy_from_slice(&meta);
        let mut e = TableEntry::read(&entry, 0);
        e.addr = addr;
        e.size = data.len() as u32;
        self.font_table[id] = e;
    }

    fn set_seq_fonts(&mut self, id: usize, fonts: &[u8]) {
        // Rebuild the table so it stays a valid u16-offset table even if the
        // sequence count grew.
        let n = self.seq_table.len();
        let lists: Vec<Vec<u8>> = (0..n)
            .map(|i| {
                if i == id {
                    fonts.to_vec()
                } else {
                    self.seq_fonts(i)
                }
            })
            .collect();
        let mut t = vec![0u8; 2 * n];
        for (i, list) in lists.iter().enumerate() {
            let at = t.len() as u16;
            t[2 * i..2 * i + 2].copy_from_slice(&at.to_be_bytes());
            t.push(list.len() as u8);
            t.extend_from_slice(list);
        }
        self.seq_font_table = t;
    }
}

struct Located {
    font_table: usize,
    seq_font_table: usize,
    seq_table: usize,
    sample_bank_table: usize,
}

fn read_table(d: &[u8], at: usize) -> Vec<TableEntry> {
    let n = be16(d, at) as usize;
    (0..n)
        .map(|i| TableEntry::read(d, at + 16 + 16 * i))
        .collect()
}

fn header_ok(d: &[u8], at: usize, max: usize) -> Option<usize> {
    if at + 16 > d.len() {
        return None;
    }
    let n = be16(d, at) as usize;
    if n == 0 || n > max || d[at + 4..at + 16].iter().any(|&b| b != 0) {
        return None;
    }
    if at + 16 + 16 * n > d.len() {
        return None;
    }
    Some(n)
}

fn entries_ok(d: &[u8], at: usize, n: usize) -> bool {
    (0..n).all(|i| {
        let e = TableEntry::read(d, at + 16 + 16 * i);
        (0..=3).contains(&e.medium) && (0..=4).contains(&e.cache_policy)
    })
}

fn find_tables(d: &[u8]) -> Option<Located> {
    let mut s = 0;
    while s + 32 <= d.len() {
        if let Some(n) = header_ok(d, s, 0x400) {
            let e0 = TableEntry::read(d, s + 16);
            // Entry 0 is always the SFX sequence, at offset 0 in Audioseq.
            if e0.addr == 0
                && (0x400..0x20000).contains(&e0.size)
                && entries_ok(d, s, n)
                && let Some(loc) = around_seq_table(d, s, n)
            {
                return Some(loc);
            }
        }
        s += 16;
    }
    None
}

fn around_seq_table(d: &[u8], s: usize, nseq: usize) -> Option<Located> {
    // The sequence→font table ends at the sequence table; the font table ends
    // where it begins.
    let lo = s.saturating_sub(0x8000);
    let mut h = s.checked_sub(16)?;
    while h >= lo {
        if let Some(nf) = header_ok(d, h, 0xFF) {
            let f = h + 16 + 16 * nf;
            if f < s && be16(d, f) as usize == 2 * nseq && entries_ok(d, h, nf) {
                let b = s + 16 + 16 * nseq;
                header_ok(d, b, 0x40)?;
                return Some(Located {
                    font_table: h,
                    seq_font_table: f,
                    seq_table: s,
                    sample_bank_table: b,
                });
            }
        }
        h = h.checked_sub(16)?;
    }
    None
}

/// The file a table's offsets point into: the dmadata file whose size matches
/// the furthest extent of the table's entries.
fn file_for(rom: &Rom, table: &[TableEntry], what: &'static str) -> Result<Vec<u8>, RomError> {
    let extent = table
        .iter()
        .filter(|e| !e.is_pointer() && e.medium == 2)
        .map(|e| e.addr as usize + e.size as usize)
        .max()
        .ok_or(RomError::NoAudioTables(what))?;
    let file = rom
        .files
        .iter()
        .filter(|f| f.vrom_size() >= extent && f.vrom_size() - extent < 0x40)
        .min_by_key(|f| f.vrom_size() - extent)
        .ok_or(RomError::NoAudioTables(what))?;
    Ok(rom.file(file).to_vec())
}

pub(crate) fn locate(rom: &Rom) -> Result<AudioData, RomError> {
    let d = &rom.vrom;
    let loc = find_tables(d).ok_or(RomError::NoAudioTables("sequence table"))?;
    let seq_table = read_table(d, loc.seq_table);
    let font_table = read_table(d, loc.font_table);
    let sample_bank_table = read_table(d, loc.sample_bank_table);
    let seq_font_table = d[loc.seq_font_table..loc.seq_table].to_vec();
    Ok(AudioData {
        game: rom.header.game(),
        audioseq: file_for(rom, &seq_table, "Audioseq")?,
        audiobank: file_for(rom, &font_table, "Audiobank")?,
        audiotable: file_for(rom, &sample_bank_table, "Audiotable")?,
        seq_table,
        font_table,
        sample_bank_table,
        seq_font_table,
    })
}
