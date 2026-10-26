//! Sequence and soundfont loading (`load.c`): starting a sequence player,
//! relocating soundfont data in RAM, and instrument/drum/SFX lookup
//! (`playback.c`'s `Audio_Get*`).

use sf_rom::AudioData;

use super::{AudioContext, MEDIUM_RAM};
use crate::ram::KSEG0;

/// `AUDIO_RELOCATED_ADDRESS_START`
const RELOCATED: u32 = KSEG0;

struct SampleBankReloc {
    base1: u32,
    medium1: u8,
    base2: u32,
    medium2: u8,
}

impl AudioContext {
    pub(crate) fn is_font_loaded(&self, font_id: u8) -> bool {
        if font_id == 0xFF {
            return true;
        }
        let real = AudioData::real_index(&self.data.font_table, font_id as usize);
        self.sound_fonts
            .get(font_id as usize)
            .is_some_and(|f| f.loaded)
            || self.sound_fonts.get(real).is_some_and(|f| f.loaded)
    }

    /// `AudioLoad_TrySyncLoadSampleBank`
    fn sample_bank(&self, id: u8) -> (u32, u8) {
        let id = id as usize;
        let real = AudioData::real_index(&self.sample_bank_table, id);
        let (Some(e), Some(r)) = (
            self.sample_bank_table.get(id),
            self.sample_bank_table.get(real),
        ) else {
            return (0, MEDIUM_RAM);
        };
        if e.cache_policy == 4 {
            (r.addr, e.medium as u8)
        } else {
            // The game would load the whole bank into RAM; it is already there.
            (r.addr, MEDIUM_RAM)
        }
    }

    /// `AudioLoad_SyncLoadFont`: copy a soundfont into RAM and relocate it.
    pub(crate) fn load_font(&mut self, font_id: u8) {
        let real = AudioData::real_index(&self.data.font_table, font_id as usize);
        if real >= self.sound_fonts.len() || self.sound_fonts[real].loaded {
            return;
        }
        let font = self.sound_fonts[real];
        let reloc = SampleBankReloc {
            base1: 0,
            medium1: MEDIUM_RAM,
            base2: 0,
            medium2: MEDIUM_RAM,
        };
        let reloc = SampleBankReloc {
            base1: if font.sample_bank_id1 != 0xFF {
                self.sample_bank(font.sample_bank_id1).0
            } else {
                reloc.base1
            },
            medium1: if font.sample_bank_id1 != 0xFF {
                self.sample_bank(font.sample_bank_id1).1
            } else {
                reloc.medium1
            },
            base2: if font.sample_bank_id2 != 0xFF {
                self.sample_bank(font.sample_bank_id2).0
            } else {
                reloc.base2
            },
            medium2: if font.sample_bank_id2 != 0xFF {
                self.sample_bank(font.sample_bank_id2).1
            } else {
                reloc.medium2
            },
        };
        let Some(bytes) = self.data.font(real).map(<[u8]>::to_vec) else {
            return;
        };
        let base = self.ram.alloc_bytes(&bytes, 16);
        self.relocate_font(real, base, &reloc);
        self.sound_fonts[real].loaded = true;
    }

    /// `AudioLoad_RelocateFont`
    fn relocate_font(&mut self, font_id: usize, base: u32, reloc: &SampleBankReloc) {
        let font = self.sound_fonts[font_id];
        let ram_reloc = |off: u32| off.wrapping_add(base);

        // Drums: fontData[0] is the offset of a list of drum offsets.
        let list = self.ram.u32(base);
        if list != 0 && font.num_drums != 0 {
            let list = ram_reloc(list);
            self.ram.set_u32(base, list);
            for i in 0..font.num_drums as u32 {
                let off = self.ram.u32(list + 4 * i);
                if off == 0 {
                    continue;
                }
                let drum = ram_reloc(off);
                self.ram.set_u32(list + 4 * i, drum);
                if self.ram.u8(drum + 2) != 0 {
                    continue;
                }
                self.relocate_sample(drum + 4, base, reloc);
                let env = self.ram.u32(drum + 0xC);
                self.ram.set_u32(drum + 0xC, ram_reloc(env));
                self.ram.set_u8(drum + 2, 1);
            }
        }

        // Sound effects: fontData[1] is the offset of the first entry.
        let list = self.ram.u32(base + 4);
        if list != 0 && font.num_sfx != 0 {
            let list = ram_reloc(list);
            self.ram.set_u32(base + 4, list);
            for i in 0..font.num_sfx as u32 {
                let se = list + 8 * i;
                if self.ram.u32(se) == 0 {
                    continue;
                }
                self.relocate_sample(se, base, reloc);
            }
        }

        // Instruments: fontData[2..] are instrument offsets.
        let n = font.num_instruments.min(126) as u32;
        for i in 0..n {
            let slot = base + 8 + 4 * i;
            let off = self.ram.u32(slot);
            if off == 0 {
                continue;
            }
            let inst = ram_reloc(off);
            self.ram.set_u32(slot, inst);
            if self.ram.u8(inst) != 0 {
                continue;
            }
            if self.ram.u8(inst + 1) != 0 {
                self.relocate_sample(inst + 8, base, reloc);
            }
            self.relocate_sample(inst + 0x10, base, reloc);
            if self.ram.u8(inst + 2) != 0x7F {
                self.relocate_sample(inst + 0x18, base, reloc);
            }
            let env = self.ram.u32(inst + 4);
            self.ram.set_u32(inst + 4, ram_reloc(env));
            self.ram.set_u8(inst, 1);
        }

        let f = &mut self.sound_fonts[font_id];
        f.drums = self.ram.u32(base);
        f.sound_effects = self.ram.u32(base + 4);
        f.instruments = base + 8;
    }

    /// `AudioLoad_RelocateSample`
    fn relocate_sample(&mut self, tuned_sample: u32, font_base: u32, reloc: &SampleBankReloc) {
        let sample = self.ram.u32(tuned_sample);
        if sample > RELOCATED {
            return;
        }
        let sample = sample.wrapping_add(font_base);
        self.ram.set_u32(tuned_sample, sample);
        let word = self.ram.u32(sample);
        let size = word & 0xFF_FFFF;
        let is_relocated = word >> 24 & 1 != 0;
        if size == 0 || is_relocated {
            return;
        }
        let loop_ = self.ram.u32(sample + 8);
        self.ram.set_u32(sample + 8, loop_.wrapping_add(font_base));
        let book = self.ram.u32(sample + 0xC);
        self.ram.set_u32(sample + 0xC, book.wrapping_add(font_base));
        let mut medium = (word >> 26) & 3;
        let addr = self.ram.u32(sample + 4);
        match medium {
            0 => {
                self.ram.set_u32(sample + 4, addr.wrapping_add(reloc.base1));
                medium = reloc.medium1 as u32;
            }
            1 => {
                self.ram.set_u32(sample + 4, addr.wrapping_add(reloc.base2));
                medium = reloc.medium2 as u32;
            }
            _ => {}
        }
        let word = (word & !(3 << 26)) | (medium & 3) << 26 | 1 << 24;
        self.ram.set_u32(sample, word);
    }

    /// `AudioLoad_SyncInitSeqPlayerInternal`, with the sequence's bytes given
    /// directly (the engine reads them from the sequence table normally).
    pub(crate) fn init_seq_player(&mut self, player: usize, seq_id: usize) {
        if seq_id >= self.data.num_sequences() || player >= self.players.len() {
            return;
        }
        self.sequence_player_disable(player);

        let mut font_id = 0xFFu8;
        for f in self.data.seq_fonts(seq_id) {
            font_id = f;
            self.load_font(f);
        }

        let Some(seq) = self.data.sequence(seq_id) else {
            return;
        };
        let seq = seq[..seq.len().min(0x10000)].to_vec();
        let seq_data = self.seq_buffers[player];
        self.ram.write(seq_data, &vec![0; 0x10000]);
        self.ram.write(seq_data, &seq);

        self.reset_sequence_player(player);
        let default_font = if font_id == 0xFF {
            0xFF
        } else {
            AudioData::real_index(&self.data.font_table, font_id as usize) as u8
        };
        let p = &mut self.players[player];
        p.seq_id = seq_id as u8;
        p.default_font = default_font;
        p.seq_data = seq_data;
        p.enabled = true;
        p.script_state.pc = seq_data;
        p.script_state.depth = 0;
        p.delay = 0;
        p.finished = false;
        p.player_idx = player as i8;
        p.ticks_elapsed = 0;
        self.skip_forward_sequence(player);
    }

    /// `Audio_GetInstrumentInner`: an `Instrument*`, or 0.
    pub(crate) fn get_instrument_inner(&self, font_id: u8, inst_id: u8) -> u32 {
        if font_id == 0xFF || !self.is_font_loaded(font_id) {
            return 0;
        }
        let f = &self.sound_fonts[font_id as usize];
        if inst_id >= f.num_instruments {
            return 0;
        }
        self.ram.u32(f.instruments + 4 * inst_id as u32)
    }

    /// `Audio_GetDrum`: a `Drum*`, or 0.
    pub(crate) fn get_drum(&self, font_id: u8, drum_id: u8) -> u32 {
        if font_id == 0xFF || !self.is_font_loaded(font_id) {
            return 0;
        }
        let f = &self.sound_fonts[font_id as usize];
        if drum_id >= f.num_drums || f.drums < RELOCATED {
            return 0;
        }
        self.ram.u32(f.drums + 4 * drum_id as u32)
    }

    /// `Audio_GetSoundEffect`: a `SoundEffect*`, or 0.
    pub(crate) fn get_sound_effect(&self, font_id: u8, sfx_id: u16) -> u32 {
        if font_id == 0xFF || !self.is_font_loaded(font_id) {
            return 0;
        }
        let f = &self.sound_fonts[font_id as usize];
        if sfx_id >= f.num_sfx || f.sound_effects < RELOCATED {
            return 0;
        }
        let se = f.sound_effects + 8 * sfx_id as u32;
        if self.ram.u32(se) == 0 {
            return 0;
        }
        se
    }

    /// `Audio_GetInstrumentTunedSample`
    pub(crate) fn instrument_tuned_sample(&self, instrument: u32, semitone: i32) -> u32 {
        let lo = self.ram.u8(instrument + 1) as i32;
        let hi = self.ram.u8(instrument + 2) as i32;
        if semitone < lo {
            instrument + 8
        } else if semitone <= hi {
            instrument + 0x10
        } else {
            instrument + 0x18
        }
    }

    // Field accessors for structures living in RAM.

    pub(crate) fn tuned_sample_sample(&self, ts: u32) -> u32 {
        self.ram.u32(ts)
    }

    pub(crate) fn tuned_sample_tuning(&self, ts: u32) -> f32 {
        self.ram.f32(ts + 4)
    }

    pub(crate) fn sample_codec(&self, sample: u32) -> u8 {
        (self.ram.u32(sample) >> 28) as u8
    }

    pub(crate) fn sample_medium(&self, sample: u32) -> u8 {
        (self.ram.u32(sample) >> 26 & 3) as u8
    }

    pub(crate) fn sample_loop(&self, sample: u32) -> u32 {
        self.ram.u32(sample + 8)
    }

    pub(crate) fn sample_book(&self, sample: u32) -> u32 {
        self.ram.u32(sample + 0xC)
    }

    pub(crate) fn sample_addr(&self, sample: u32) -> u32 {
        self.ram.u32(sample + 4)
    }
}
