//! The OoT audio engine, ported from the zeldaret decomp so that SeqForge
//! plays sequences the way the game does: the same sequence interpreter, note
//! pool, envelopes and command lists, executed by a high-level model of the RSP
//! audio microcode.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let rom = sf_rom::Rom::parse(&std::fs::read("oot.z64")?)?;
//! let mut synth = sf_synth::Synth::new(rom.audio()?, 0);
//! synth.play(0, 0x28); // BGM player, Kokiri Forest
//! let mut out = Vec::new();
//! for _ in 0..60 * 30 {
//!     synth.render_frame(&mut out);
//! }
//! # Ok(()) }
//! ```

// The engine mirrors the decomp's C, index loops included.
#![allow(clippy::needless_range_loop)]

pub mod engine;
mod ram;
mod rsp;
mod tables;
#[cfg(test)]
mod tests;

pub use engine::NoteEvent;
pub use engine::spec::{AudioSpec, OOT_SPECS};

use engine::{AudioContext, NUM_PLAYERS};
use sf_rom::AudioData;

/// Sequence player slots, as the game assigns them.
pub mod player {
    pub const BGM_MAIN: usize = 0;
    pub const FANFARE: usize = 1;
    pub const SFX: usize = 2;
    pub const BGM_SUB: usize = 3;
}

/// What a channel is doing right now, for meters and editors.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChannelView {
    pub enabled: bool,
    pub muted: bool,
    /// Instrument id as the sequence set it (`instr`), or `None` if unset.
    pub instrument: Option<u8>,
    pub volume: f32,
    pub pan: u8,
    pub reverb: u8,
    /// Semitones currently sounding, one per active layer.
    pub notes: [Option<u8>; 4],
}

#[derive(Clone, Debug, Default)]
pub struct PlayerView {
    pub enabled: bool,
    pub seq_id: u8,
    /// Tempo in BPM.
    pub bpm: f32,
    pub ticks: u32,
    pub channels: [ChannelView; 16],
}

pub struct Synth {
    ctx: AudioContext,
}

impl Synth {
    /// A synth using audio spec `spec` (0 is the game's default).
    pub fn new(data: AudioData, spec: usize) -> Synth {
        let spec = OOT_SPECS.get(spec).copied().unwrap_or(OOT_SPECS[0]);
        Synth {
            ctx: AudioContext::new(data, spec),
        }
    }

    /// Start sequence `seq_id` on a player, loading its soundfonts.
    pub fn play(&mut self, player: usize, seq_id: usize) {
        self.ctx.init_seq_player(player, seq_id);
    }

    /// Replace a sequence slot with custom bytes (and optionally fonts), then
    /// play it. Changes last until the next `replace_sequence` of that slot.
    pub fn play_custom(&mut self, player: usize, slot: usize, seq: &[u8], fonts: Option<&[u8]>) {
        self.ctx.data.replace_sequence(slot, seq, fonts);
        self.ctx.init_seq_player(player, slot);
    }

    pub fn stop(&mut self, player: usize) {
        if player < NUM_PLAYERS {
            self.ctx.sequence_player_disable(player);
        }
    }

    pub fn is_playing(&self, player: usize) -> bool {
        self.ctx.players.get(player).is_some_and(|p| p.enabled)
    }

    /// Render one video frame of audio (about 1/60 s), appending stereo
    /// samples at [`Synth::sample_rate`].
    pub fn render_frame(&mut self, out: &mut Vec<[i16; 2]>) {
        self.ctx.update(out);
    }

    /// The audio interface rate (32006 Hz for the default spec on NTSC).
    pub fn sample_rate(&self) -> u32 {
        self.ctx.output_rate()
    }

    pub fn set_channel_muted(&mut self, player: usize, channel: usize, muted: bool) {
        if let Some(p) = self.ctx.players.get(player) {
            let ch = p.channels[channel & 15];
            self.ctx.channels[ch].muted = muted;
        }
    }

    /// Per-player volume, as the game's `SEQCMD_SET_PLAYER_VOLUME` sets it.
    pub fn set_player_volume(&mut self, player: usize, volume: f32) {
        if let Some(p) = self.ctx.players.get_mut(player) {
            p.fade_volume_scale = volume;
            p.recalculate_volume = true;
        }
    }

    /// Write a sequence IO port (how the game talks to scripts, e.g. for
    /// day/night or area variations).
    pub fn set_io(&mut self, player: usize, port: usize, value: i8) {
        if let Some(p) = self.ctx.players.get_mut(player) {
            p.seq_script_io[port & 7] = value;
        }
    }

    /// Record a [`NoteEvent`] for each note started.
    pub fn record_events(&mut self, on: bool) {
        self.ctx.record_events = on;
    }

    pub fn take_events(&mut self) -> Vec<NoteEvent> {
        std::mem::take(&mut self.ctx.events)
    }

    /// Voices currently allocated to something (playing or releasing).
    pub fn active_voices(&self) -> usize {
        self.ctx
            .notes
            .iter()
            .filter(|n| n.playback_state.priority != 0)
            .count()
    }

    pub fn max_voices(&self) -> usize {
        self.ctx.num_notes
    }

    pub fn player_view(&self, player: usize) -> PlayerView {
        let Some(p) = self.ctx.players.get(player) else {
            return PlayerView::default();
        };
        let mut view = PlayerView {
            enabled: p.enabled,
            seq_id: p.seq_id,
            bpm: p.tempo as f32 / engine::SEQTICKS_PER_BEAT as f32,
            ticks: p.ticks_elapsed,
            channels: Default::default(),
        };
        for (i, &ch) in p.channels.iter().enumerate() {
            let c = &self.ctx.channels[ch];
            let mut notes = [None; 4];
            for (k, l) in c.layers.iter().enumerate() {
                if let Some(l) = l {
                    let layer = &self.ctx.layers[*l];
                    let sounding = layer.enabled
                        && !layer.muted
                        && layer.note.is_some_and(|n| {
                            self.ctx.notes[n].playback_state.parent_layer == Some(*l)
                        });
                    if sounding {
                        notes[k] = Some(layer.semitone);
                    }
                }
            }
            view.channels[i] = ChannelView {
                enabled: c.enabled,
                muted: c.muted,
                instrument: c.has_instrument.then(|| {
                    if c.inst_or_wave >= 2 {
                        (c.inst_or_wave - 2) as u8
                    } else {
                        0x7F - c.inst_or_wave as u8
                    }
                }),
                volume: c.volume,
                pan: c.new_pan,
                reverb: c.target_reverb_vol,
                notes,
            };
        }
        view
    }

    /// The underlying engine, for tools that need more than this API exposes.
    pub fn engine(&self) -> &AudioContext {
        &self.ctx
    }
}
