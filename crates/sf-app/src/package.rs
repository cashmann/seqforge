//! Loading sequences from files: raw `.zseq`/`.seq`/`.aseq`, OoT Randomizer
//! `.ootrs` and MMR `.mmrs` packages (optionally with a custom soundfont).

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sf_rom::{AudioData, GameId};

pub struct CustomBank {
    pub id: u8,
    pub data: Vec<u8>,
    pub meta: [u8; 8],
}

pub struct Loaded {
    pub title: String,
    pub seq: Vec<u8>,
    /// Fonts to load (last = default), if the file says.
    pub fonts: Option<Vec<u8>>,
    pub bank: Option<CustomBank>,
    /// Which game the package was made for, if known.
    pub game: Option<GameId>,
}

impl Loaded {
    /// Install into a copy of the ROM's audio data at a fresh sequence slot.
    /// Returns the slot.
    pub fn install(&self, audio: &mut AudioData) -> usize {
        if let Some(bank) = &self.bank {
            audio.replace_font(bank.id as usize, &bank.data, bank.meta);
        }
        let slot = audio.num_sequences();
        audio.replace_sequence(slot, &self.seq, Some(self.fonts.as_deref().unwrap_or(&[])));
        slot
    }
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("sequence")
        .to_string()
}

fn parse_hex(s: &str) -> Option<u8> {
    let s = s.trim();
    u8::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()
}

pub fn load(path: &Path) -> Result<Loaded> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "ootrs" | "mmrs" => load_package(path, &ext),
        _ => Ok(Loaded {
            title: stem(path),
            seq: std::fs::read(path).with_context(|| format!("reading {}", path.display()))?,
            fonts: None,
            bank: None,
            game: None,
        }),
    }
}

fn load_package(path: &Path, ext: &str) -> Result<Loaded> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("not a valid zip package")?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        if !e.is_file() {
            continue;
        }
        let name = e.name().to_string();
        let mut data = Vec::new();
        e.read_to_end(&mut data)?;
        entries.push((name, data));
    }
    let find = |suffix: &str| {
        entries
            .iter()
            .find(|(n, _)| n.to_ascii_lowercase().ends_with(suffix))
    };
    let name_stem = |n: &str| {
        Path::new(n)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    };

    let bank = |id: u8| -> Option<CustomBank> {
        let (_, data) = find(".zbank")?;
        let (_, meta) = find(".bankmeta")?;
        let meta: [u8; 8] = meta.get(..8)?.try_into().ok()?;
        Some(CustomBank {
            id,
            data: data.clone(),
            meta,
        })
    };

    if ext == "mmrs" {
        // `<font hex>.zseq`, optionally `<font hex>.zbank` + `.bankmeta`.
        let Some((name, seq)) = find(".zseq") else {
            bail!("no .zseq in package");
        };
        let font = parse_hex(&name_stem(name));
        Ok(Loaded {
            title: stem(path),
            seq: seq.clone(),
            fonts: font.map(|f| vec![f]),
            bank: font.and_then(bank),
            game: Some(GameId::Mm),
        })
    } else {
        // `<name>.seq` + `<name>.meta`: title, font (hex), type, groups.
        let Some((_, seq)) = find(".seq").or_else(|| find(".zseq")) else {
            bail!("no .seq in package");
        };
        let meta = find(".meta")
            .map(|(_, m)| String::from_utf8_lossy(m).to_string())
            .unwrap_or_default();
        let mut lines = meta.lines();
        let title = lines
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let font = lines.next().and_then(parse_hex);
        Ok(Loaded {
            title: title.unwrap_or_else(|| stem(path)),
            seq: seq.clone(),
            fonts: font.map(|f| vec![f]),
            bank: font.and_then(bank),
            game: Some(GameId::Oot),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SEQFORGE_TEST_ROM=mm.z64 SEQFORGE_TEST_PACKAGE=song.mmrs cargo test -p sf-app -- --ignored`
    #[test]
    #[ignore = "needs a ROM and a package (SEQFORGE_TEST_ROM, SEQFORGE_TEST_PACKAGE)"]
    fn package_plays() {
        let rom = std::env::var_os("SEQFORGE_TEST_ROM").expect("set SEQFORGE_TEST_ROM");
        let pkg = std::env::var_os("SEQFORGE_TEST_PACKAGE").expect("set SEQFORGE_TEST_PACKAGE");
        let rom = sf_rom::Rom::parse(&std::fs::read(rom).unwrap()).unwrap();
        let mut audio = rom.audio().unwrap();
        let loaded = load(Path::new(&pkg)).unwrap();
        let slot = loaded.install(&mut audio);
        let mut synth = sf_synth::Synth::new(audio, 0);
        synth.play(0, slot);
        let mut out = Vec::new();
        for _ in 0..60 * 10 {
            synth.render_frame(&mut out);
        }
        let peak = out
            .iter()
            .flat_map(|s| s.iter())
            .map(|&v| (v as i32).abs())
            .max()
            .unwrap();
        eprintln!(
            "{}: custom bank {}, peak {peak}",
            loaded.title,
            loaded.bank.is_some()
        );
        assert!(peak > 500, "package is silent");
    }
}
