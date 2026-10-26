//! A port of the OoT audio library (`src/audio/internal/` in zeldaret/oot):
//! sequence player (`seqplayer.rs`), note allocation and pools
//! (`playback.rs`), envelopes/vibrato/portamento (`effects.rs`), the RSP
//! command-list builder (`synthesis.rs`) and sequence/font loading
//! (`load.rs`). Structure and field names follow the decomp so the two can be
//! read side by side; pointers become RAM addresses or arena indices.

mod effects;
pub mod lists;
mod load;
mod playback;
mod seqplayer;
pub mod spec;
mod synthesis;

use sf_rom::{AudioData, TableEntry};

use crate::ram::Ram;
use crate::rsp::Rsp;
use crate::tables;
use lists::{ListValue, Lists, NodeId, PoolId};
use spec::{AudioSpec, REVERB_SETTINGS};

pub const SEQTICKS_PER_BEAT: u16 = 48;
pub const NUM_PLAYERS: usize = 4;
pub const NUM_LAYERS: usize = 64;
const SAMPLES_PER_FRAME: i32 = 16;
const SAMPLE_SIZE: i32 = 2;
const DMEM_1CH_SIZE: i32 = 13 * SAMPLES_PER_FRAME * SAMPLE_SIZE;
const DMEM_2CH_SIZE: i32 = 2 * DMEM_1CH_SIZE;
const WAVE_SAMPLE_COUNT: i32 = 64;
const AIBUF_LEN: usize = 88 * 16;

const MUTE_BEHAVIOR_3: u8 = 1 << 3;
const MUTE_BEHAVIOR_4: u8 = 1 << 4;
const MUTE_BEHAVIOR_SOFTEN: u8 = 1 << 5;
const MUTE_BEHAVIOR_STOP_NOTES: u8 = 1 << 6;
const MUTE_BEHAVIOR_STOP_SCRIPT: u8 = 1 << 7;

pub const ADSR_STATE_DISABLED: u8 = 0;
pub const ADSR_STATE_INITIAL: u8 = 1;
pub const ADSR_STATE_START_LOOP: u8 = 2;
pub const ADSR_STATE_LOOP: u8 = 3;
pub const ADSR_STATE_FADE: u8 = 4;
pub const ADSR_STATE_HANG: u8 = 5;
pub const ADSR_STATE_DECAY: u8 = 6;
pub const ADSR_STATE_RELEASE: u8 = 7;
pub const ADSR_STATE_SUSTAIN: u8 = 8;

pub const CODEC_ADPCM: u8 = 0;
pub const CODEC_S8: u8 = 1;
pub const CODEC_S16_INMEMORY: u8 = 2;
pub const CODEC_SMALL_ADPCM: u8 = 3;
pub const CODEC_REVERB: u8 = 4;
pub const CODEC_S16: u8 = 5;

pub const MEDIUM_RAM: u8 = 0;
pub const MEDIUM_UNK: u8 = 1;

pub const SOUND_OUTPUT_STEREO: u8 = 0;
pub const SOUND_OUTPUT_HEADSET: u8 = 1;
pub const SOUND_OUTPUT_MONO: u8 = 3;

/// N64 NTSC video clock, used by `osAiSetFrequency`.
const VI_CLOCK_NTSC: u32 = 48_681_812;

#[derive(Clone, Copy, Default, Debug)]
pub struct SeqScriptState {
    pub pc: u32,
    pub stack: [u32; 4],
    pub rem_loop_iters: [u8; 4],
    pub depth: u8,
    pub value: i8,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct AdsrSettings {
    pub decay_index: u8,
    pub sustain: u8,
    pub envelope: u32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Portamento {
    pub mode: u8,
    pub cur: u16,
    pub speed: u16,
    pub extent: f32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct AdsrState {
    pub hang: bool,
    pub decay: bool,
    pub release: bool,
    pub state: u8,
    pub env_index: u8,
    pub delay: i16,
    pub sustain: f32,
    pub velocity: f32,
    pub fade_out_vel: f32,
    pub current: f32,
    pub target: f32,
    pub envelope: u32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct VibratoState {
    pub channel: Option<usize>,
    pub time: u32,
    pub curve: u32,
    pub depth: f32,
    pub rate: f32,
    pub active: bool,
    pub rate_change_timer: u16,
    pub depth_change_timer: u16,
    pub delay: u16,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct NoteAttributes {
    pub reverb: u8,
    pub gain: u8,
    pub pan: u8,
    pub stereo: u8,
    pub comb_filter_size: u8,
    pub comb_filter_gain: u16,
    pub freq_scale: f32,
    pub velocity: f32,
    pub filter: u32,
    /// RAM address of this note's 8-coefficient copy of a filter.
    pub filter_buf: u32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct NotePlaybackState {
    pub priority: u8,
    pub wave_id: u8,
    pub harmonic_index: u8,
    pub font_id: u8,
    pub unk_04: u8,
    pub stereo_headset_effects: bool,
    pub portamento_freq_scale: f32,
    pub vibrato_freq_scale: f32,
    pub prev_parent_layer: Option<usize>,
    pub parent_layer: Option<usize>,
    pub wanted_parent_layer: Option<usize>,
    pub attributes: NoteAttributes,
    pub adsr: AdsrState,
    pub portamento: Portamento,
    pub vibrato_state: VibratoState,
}

/// `NoteSampleState` ("commonch"): what the synthesis pass reads.
#[derive(Clone, Copy, Default, Debug)]
pub struct NoteSampleState {
    pub enabled: bool,
    pub needs_init: bool,
    pub finished: bool,
    pub stereo_strong_right: bool,
    pub stereo_strong_left: bool,
    pub stereo_headset_effects: bool,
    pub uses_headset_pan_effects: bool,
    pub reverb_index: u8,
    pub book_offset: u8,
    pub is_synthetic_wave: bool,
    pub has_two_parts: bool,
    pub use_haas_effect: bool,
    pub gain: u8,
    pub haas_effect_left_delay_size: u8,
    pub haas_effect_right_delay_size: u8,
    pub reverb_vol: u8,
    pub harmonic_index_cur_and_prev: u8,
    pub comb_filter_size: u8,
    pub target_vol_left: u16,
    pub target_vol_right: u16,
    pub resampling_rate_fixed_point: u16,
    pub comb_filter_gain: u16,
    /// `TunedSample*`, or the synthetic wave's sample address.
    pub tuned_sample: u32,
    pub filter: u32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct NoteSynthesisState {
    pub restart: bool,
    pub prev_haas_effect_left_delay_size: u8,
    pub prev_haas_effect_right_delay_size: u8,
    pub reverb_vol: u8,
    pub num_parts: u8,
    pub sample_pos_frac: u16,
    pub sample_pos_int: i32,
    pub synthesis_buffers: u32,
    pub cur_vol_left: u16,
    pub cur_vol_right: u16,
    pub comb_filter_needs_init: bool,
}

// Offsets into a note's `NoteSynthesisBuffers`.
const SYNTH_ADPCMDEC_STATE: u32 = 0x00;
const SYNTH_FINAL_RESAMPLE_STATE: u32 = 0x20;
const SYNTH_MIX_ENVELOPE_STATE: u32 = 0x40;
const SYNTH_HAAS_DELAY_STATE: u32 = 0xA0;
const SYNTH_COMB_FILTER_STATE: u32 = 0xE0;
const SYNTH_BUFFERS_SIZE: usize = 0x1E0;

#[derive(Clone, Copy, Debug)]
pub struct Note {
    pub list_item: NodeId,
    pub synthesis_state: NoteSynthesisState,
    pub playback_state: NotePlaybackState,
    pub start_sample_pos: u32,
    pub sample_state: NoteSampleState,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SequenceLayer {
    pub enabled: bool,
    pub finished: bool,
    pub muted: bool,
    pub continuous_notes: bool,
    pub bit3: bool,
    pub ignore_drum_pan: bool,
    pub bit1: bool,
    pub note_properties_need_init: bool,
    pub stereo: u8,
    pub inst_or_wave: u8,
    pub gate_time: u8,
    pub semitone: u8,
    pub portamento_target_note: u8,
    pub pan: u8,
    pub note_pan: u8,
    pub delay: i16,
    pub gate_delay: i16,
    pub delay2: i16,
    pub portamento_time: u16,
    pub transposition: i16,
    pub short_note_default_delay: i16,
    pub last_delay: i16,
    pub adsr: AdsrSettings,
    pub portamento: Portamento,
    pub note: Option<usize>,
    pub freq_scale: f32,
    pub bend: f32,
    pub velocity_square2: f32,
    pub velocity_square: f32,
    pub note_velocity: f32,
    pub note_freq_scale: f32,
    pub instrument: u32,
    pub tuned_sample: u32,
    pub channel: Option<usize>,
    pub script_state: SeqScriptState,
    pub list_item: NodeId,
}

#[derive(Clone, Copy, Debug)]
pub struct SequenceChannel {
    pub enabled: bool,
    pub finished: bool,
    pub stop_script: bool,
    pub muted: bool,
    pub has_instrument: bool,
    pub stereo_headset_effects: bool,
    pub large_notes: bool,
    pub change_freq_scale: bool,
    pub change_volume: bool,
    pub change_pan: bool,
    pub note_alloc_policy: u8,
    pub mute_behavior: u8,
    pub target_reverb_vol: u8,
    pub note_priority: u8,
    pub some_other_priority: u8,
    pub font_id: u8,
    pub reverb_index: u8,
    pub book_offset: u8,
    pub new_pan: u8,
    pub pan_channel_weight: u8,
    pub gain: u8,
    pub velocity_random_variance: u8,
    pub gate_time_random_variance: u8,
    pub comb_filter_size: u8,
    pub vibrato_rate_start: u16,
    pub vibrato_depth_start: u16,
    pub vibrato_rate_target: u16,
    pub vibrato_depth_target: u16,
    pub vibrato_rate_change_delay: u16,
    pub vibrato_depth_change_delay: u16,
    pub vibrato_delay: u16,
    pub delay: u16,
    pub comb_filter_gain: u16,
    pub unk_22: u16,
    pub inst_or_wave: i16,
    pub transposition: i16,
    pub volume_scale: f32,
    pub volume: f32,
    pub pan: i32,
    pub applied_volume: f32,
    pub freq_scale: f32,
    pub dyn_table: u32,
    /// `Instrument*`; 1 and 2 are the drum/SFX sentinels, 0 is NULL.
    pub instrument: u32,
    pub seq_player: usize,
    pub layers: [Option<usize>; 4],
    pub script_state: SeqScriptState,
    pub adsr: AdsrSettings,
    pub note_pool: PoolId,
    pub seq_script_io: [i8; 8],
    pub filter: u32,
    pub stereo: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct SequencePlayer {
    pub enabled: bool,
    pub finished: bool,
    pub muted: bool,
    pub recalculate_volume: bool,
    pub stop_script: bool,
    pub apply_bend: bool,
    pub state: u8,
    pub note_alloc_policy: u8,
    pub mute_behavior: u8,
    pub seq_id: u8,
    pub default_font: u8,
    pub player_idx: i8,
    pub tempo: u16,
    pub tempo_acc: u16,
    pub tempo_change: u16,
    pub transposition: i16,
    pub delay: u16,
    pub fade_timer: u16,
    pub fade_timer_unk_eu: u16,
    pub seq_data: u32,
    pub fade_volume: f32,
    pub fade_velocity: f32,
    pub volume: f32,
    pub mute_volume_scale: f32,
    pub fade_volume_scale: f32,
    pub applied_fade_volume: f32,
    pub bend: f32,
    pub channels: [usize; 16],
    pub script_state: SeqScriptState,
    pub short_note_velocity_table: u32,
    pub short_note_gate_time_table: u32,
    pub note_pool: PoolId,
    pub skip_ticks: i32,
    pub script_counter: u32,
    pub seq_script_io: [i8; 8],
    /// Not in the game: sequence ticks executed since the player started.
    pub ticks_elapsed: u32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct RingBufferItem {
    pub to_downsample_left: u32,
    pub to_downsample_right: u32,
    pub start_pos: i32,
    pub length_a: i16,
    pub length_b: i16,
    pub unk_14: u16,
    pub unk_16: u16,
    pub unk_18: u16,
}

#[derive(Clone, Debug, Default)]
pub struct SynthesisReverb {
    pub resample_flags: u8,
    pub use_reverb: u8,
    pub frames_to_ignore: u8,
    pub cur_frame: u8,
    pub downsample_rate: u8,
    pub unk_05: i8,
    pub window_size: u16,
    pub unk_08: i16,
    pub volume: i16,
    pub decay_ratio: u16,
    pub unk_0e: u16,
    pub leak_rtl: i16,
    pub leak_ltr: i16,
    pub unk_14: u16,
    pub unk_16: i16,
    pub unk_18: u8,
    pub next_ring_buf_pos: i32,
    pub buf_size_per_chan: i32,
    pub left_ring_buf: u32,
    pub right_ring_buf: u32,
    pub resample_state: [u32; 4],
    pub items: [[RingBufferItem; 5]; 2],
    pub items2: [[RingBufferItem; 5]; 2],
    pub filter_left: u32,
    pub filter_right: u32,
    pub filter_left_state: u32,
    pub filter_right_state: u32,
    /// RAM address of this reverb's `TunedSample` (instOrWave 0xC0+).
    pub tuned_sample: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BufferParams {
    pub sampling_frequency: u16,
    pub ai_sampling_frequency: u16,
    pub samples_per_frame_target: i16,
    pub max_ai_buffer_length: i16,
    pub min_ai_buffer_length: i16,
    pub ticks_per_update: i16,
    pub samples_per_tick: i16,
    pub samples_per_tick_max: i16,
    pub samples_per_tick_min: i16,
    pub num_sequence_players: i16,
    pub resample_rate: f32,
    pub ticks_per_update_inv: f32,
    pub ticks_per_update_inv_scaled: f32,
    pub ticks_per_update_scaled: f32,
    pub spec_unk4: i16,
}

/// A loaded, relocated soundfont (`SoundFont`).
#[derive(Clone, Copy, Debug, Default)]
pub struct SoundFont {
    pub num_instruments: u8,
    pub num_drums: u8,
    pub sample_bank_id1: u8,
    pub sample_bank_id2: u8,
    pub num_sfx: u16,
    pub instruments: u32,
    pub drums: u32,
    pub sound_effects: u32,
    pub loaded: bool,
}

/// A note starting, reported for visualization (not part of the game).
#[derive(Clone, Copy, Debug)]
pub struct NoteEvent {
    pub player: usize,
    pub channel: usize,
    pub layer: usize,
    /// Player tick at which the note starts.
    pub tick: u32,
    pub semitone: u8,
    pub velocity: f32,
    /// Length in sequence ticks (the layer's delay).
    pub duration: u16,
}

pub struct AudioContext {
    pub ram: Ram,
    pub rsp: Rsp,
    pub lists: Lists,
    pub data: AudioData,
    pub spec: AudioSpec,
    pub bp: BufferParams,
    pub num_notes: usize,
    pub max_tempo: i16,
    pub unk_2870: f32,
    pub refresh_rate: i32,
    pub adsr_decay_table: [f32; 256],
    pub notes: Vec<Note>,
    pub sample_states: Vec<NoteSampleState>,
    pub sample_state_offset: usize,
    pub layers: Vec<SequenceLayer>,
    pub layer_free_list: NodeId,
    pub channels: Vec<SequenceChannel>,
    pub players: Vec<SequencePlayer>,
    pub note_free_lists: PoolId,
    pub synthesis_reverbs: Vec<SynthesisReverb>,
    pub num_synthesis_reverbs: usize,
    pub sound_output_mode: u8,
    pub audio_random: u32,
    pub total_task_count: u32,
    pub sound_fonts: Vec<SoundFont>,
    pub sample_bank_table: Vec<TableEntry>,
    pub cur_loaded_book: u32,
    pub(crate) wave_samples: [u32; 9],
    noise_base: u32,
    default_envelope: u32,
    default_short_velocity: u32,
    default_short_gate: u32,
    book_offset_1: u32,
    seq_buffers: [u32; NUM_PLAYERS],
    ai_buf: u32,
    /// Fractional AI samples still queued at the start of the next frame.
    ai_queue: f64,
    pub vi_rate: f64,
    os_count: u32,
    next_random_state: u32,
    pub record_events: bool,
    pub events: Vec<NoteEvent>,
}

/// `osAiSetFrequency`: the rate the AI actually runs at.
fn ai_frequency(freq: u32) -> u16 {
    let dac_rate = (VI_CLOCK_NTSC as f32 / freq as f32 + 0.5) as u32;
    (VI_CLOCK_NTSC / dac_rate) as u16
}

impl AudioContext {
    pub fn new(data: AudioData, spec: AudioSpec) -> AudioContext {
        let mut ram = Ram::new();
        let mut lists = Lists::default();

        // Fixed data the engine points at.
        let wave_tables: [&[i16; 256]; 8] = [
            &tables::SAWTOOTH_WAVE,
            &tables::TRIANGLE_WAVE,
            &tables::SINE_WAVE,
            &tables::SQUARE_WAVE,
            &tables::WHITE_NOISE_WAVE,
            &tables::UNK_WAVE_5,
            &tables::EIGHTH_PULSE_WAVE,
            &tables::QUARTER_PULSE_WAVE,
        ];
        let mut wave_samples = [0u32; 9];
        for (i, w) in wave_tables.iter().enumerate() {
            wave_samples[i] = alloc_i16s(&mut ram, &w[..]);
        }
        wave_samples[8] = wave_samples[7];
        // gWaveSamples[8] reads the game's own machine code as noise. Stand in
        // with fixed pseudo-random bytes.
        let mut noise = vec![0u8; 0x11000];
        let mut x: u32 = 0x2545_F491;
        for b in noise.iter_mut() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
        let noise_base = ram.alloc_bytes(&noise, 16);
        let default_envelope = alloc_i16s(&mut ram, &[1, 32000, 1000, 32000, -1, 0, 0, 0]);
        let default_short_velocity = ram.alloc_bytes(&tables::DEFAULT_SHORT_VELOCITY, 16);
        let default_short_gate = ram.alloc_bytes(&tables::DEFAULT_SHORT_GATE, 16);
        let book_offset_1 = alloc_i16s(&mut ram, &tables::BOOK_OFFSET_1_TABLE) + 2;

        // Audiotable is streamed from cartridge by the game; map it into RAM
        // so sample addresses can be read directly.
        let audiotable_base = ram.alloc_bytes(&data.audiotable, 16);
        let mut sample_bank_table = data.sample_bank_table.clone();
        for e in sample_bank_table.iter_mut() {
            if e.size != 0 && e.medium == 2 {
                e.addr += audiotable_base;
            }
        }

        let mut seq_buffers = [0u32; NUM_PLAYERS];
        for b in seq_buffers.iter_mut() {
            *b = ram.alloc(0x10000, 16);
        }
        let ai_buf = ram.alloc(AIBUF_LEN * 4, 16);

        let num_fonts = data.font_table.len();
        let sound_fonts = (0..num_fonts)
            .map(|i| {
                let info = data.font_info(i).unwrap();
                SoundFont {
                    num_instruments: info.num_instruments,
                    num_drums: info.num_drums,
                    sample_bank_id1: info.sample_bank_1,
                    sample_bank_id2: info.sample_bank_2,
                    num_sfx: info.num_sfx,
                    ..SoundFont::default()
                }
            })
            .collect();

        let note_free_lists = lists.new_pool();
        let layer_free_list = lists.new_head(None);

        let mut ctx = AudioContext {
            ram,
            rsp: Rsp::new(),
            lists,
            data,
            spec,
            bp: BufferParams::default(),
            num_notes: 0,
            max_tempo: 0,
            unk_2870: 0.0,
            refresh_rate: 60,
            adsr_decay_table: [0.0; 256],
            notes: Vec::new(),
            sample_states: Vec::new(),
            sample_state_offset: 0,
            layers: Vec::new(),
            layer_free_list,
            channels: Vec::new(),
            players: Vec::new(),
            note_free_lists,
            synthesis_reverbs: Vec::new(),
            num_synthesis_reverbs: 0,
            sound_output_mode: SOUND_OUTPUT_STEREO,
            audio_random: 0,
            total_task_count: 0,
            sound_fonts,
            sample_bank_table,
            cur_loaded_book: 0,
            wave_samples,
            noise_base,
            default_envelope,
            default_short_velocity,
            default_short_gate,
            book_offset_1,
            seq_buffers,
            ai_buf,
            ai_queue: 0.0,
            vi_rate: 60.0,
            os_count: 0x1234_5678,
            next_random_state: 0x1234_5678,
            record_events: false,
            events: Vec::new(),
        };
        ctx.heap_init();
        ctx
    }

    /// `AudioHeap_Init`, minus the memory pools.
    fn heap_init(&mut self) {
        let spec = self.spec;
        let refresh_rate = self.refresh_rate;
        // NTSC: 1000 * REFRESH_RATE_DEVIATION_NTSC / REFRESH_RATE_NTSC
        let max_tempo_tv_type_factors: f32 = 1000.0f32 * 1.00278f32 / 60.0f32;

        let bp = &mut self.bp;
        bp.sampling_frequency = spec.sampling_frequency as u16;
        bp.ai_sampling_frequency = ai_frequency(spec.sampling_frequency);
        bp.samples_per_frame_target =
            ((spec.sampling_frequency as i32 / refresh_rate + 15) & !15) as i16;
        bp.min_ai_buffer_length = bp.samples_per_frame_target - 0x10;
        bp.max_ai_buffer_length = bp.samples_per_frame_target + 0x10;
        bp.ticks_per_update = ((bp.samples_per_frame_target + 0x10) / 0xD0) + 1;
        bp.samples_per_tick = (bp.samples_per_frame_target / bp.ticks_per_update) & !7;
        bp.samples_per_tick_max = bp.samples_per_tick + 8;
        bp.samples_per_tick_min = bp.samples_per_tick - 8;
        bp.resample_rate = 32000.0f32 / spec.sampling_frequency as i32 as f32;
        bp.ticks_per_update_inv_scaled = (1.0f32 / 256.0f32) / bp.ticks_per_update as f32;
        bp.ticks_per_update_scaled = bp.ticks_per_update as f32 / 4.0f32;
        bp.ticks_per_update_inv = 1.0f32 / bp.ticks_per_update as f32;

        self.num_notes = spec.num_notes as usize;
        bp.num_sequence_players = (spec.num_sequence_players as i16).min(4);

        self.max_tempo = (bp.ticks_per_update as f32
            * (60 * 1000 * SEQTICKS_PER_BEAT as i32) as f32
            / SEQTICKS_PER_BEAT as f32
            / max_tempo_tv_type_factors) as u32 as i16;

        let mut unk_2870 = refresh_rate as f32;
        unk_2870 *= bp.ticks_per_update as f32;
        unk_2870 /= bp.ai_sampling_frequency as f32;
        unk_2870 /= self.max_tempo as f32;
        self.unk_2870 = unk_2870;

        bp.spec_unk4 = spec.unk_04 as i16;
        bp.samples_per_frame_target *= bp.spec_unk4;
        bp.max_ai_buffer_length *= bp.spec_unk4;
        bp.min_ai_buffer_length *= bp.spec_unk4;
        bp.ticks_per_update *= bp.spec_unk4;
        if bp.spec_unk4 >= 2 {
            bp.max_ai_buffer_length -= 0x10;
        }

        // Notes.
        self.notes.clear();
        for i in 0..self.num_notes {
            let list_item = self.lists.new_item(ListValue::Note(i));
            let synthesis_buffers = self.ram.alloc(SYNTH_BUFFERS_SIZE, 16);
            let filter_buf = self.ram.alloc(16, 16);
            let mut note = Note {
                list_item,
                synthesis_state: NoteSynthesisState {
                    synthesis_buffers,
                    ..Default::default()
                },
                playback_state: NotePlaybackState::default(),
                start_sample_pos: 0,
                sample_state: NoteSampleState::default(),
            };
            note.playback_state.attributes.filter_buf = filter_buf;
            self.notes.push(note);
        }
        self.note_init_all();
        self.init_note_free_list();
        let ticks = self.bp.ticks_per_update as usize;
        self.sample_states = vec![NoteSampleState::default(); ticks * self.num_notes];

        self.init_adsr_decay_table();

        // Reverbs.
        self.num_synthesis_reverbs = spec.num_reverbs as usize;
        self.synthesis_reverbs = vec![SynthesisReverb::default(); 4];
        let settings = REVERB_SETTINGS[spec.reverb_settings];
        for i in 0..self.num_synthesis_reverbs {
            let s = settings[i];
            let mut r = SynthesisReverb {
                downsample_rate: s.downsample_rate,
                ..Default::default()
            };
            r.window_size = s.window_size.wrapping_mul(64);
            r.window_size /= r.downsample_rate as u16;
            r.decay_ratio = s.decay_ratio;
            r.volume = s.volume as i16;
            r.unk_14 = s.unk_6.wrapping_mul(64);
            r.unk_16 = s.unk_8 as i16;
            r.unk_18 = 0;
            r.leak_rtl = s.leak_rtl as i16;
            r.leak_ltr = s.leak_ltr as i16;
            r.unk_05 = s.unk_10;
            r.unk_08 = s.unk_12 as i16;
            r.use_reverb = 8;
            r.left_ring_buf = self.ram.alloc(r.window_size as usize * 2, 16);
            r.right_ring_buf = self.ram.alloc(r.window_size as usize * 2, 16);
            r.next_ring_buf_pos = 0;
            r.cur_frame = 0;
            r.buf_size_per_chan = r.window_size as i32;
            r.frames_to_ignore = 2;
            r.resample_flags = 1;

            // TunedSample / Sample / AdpcmLoop for playing the reverb buffer
            // as an instrument (instOrWave 0xC0+).
            let loop_addr = self.ram.alloc(0x30, 16);
            self.ram.set_u32(loop_addr, 0);
            self.ram.set_u32(loop_addr + 4, r.window_size as u32);
            self.ram.set_u32(loop_addr + 8, 1);
            let sample_addr = self.ram.alloc(0x10, 16);
            let size = r.window_size as u32 * 2;
            self.ram.set_u32(
                sample_addr,
                (CODEC_REVERB as u32) << 28 | (MEDIUM_RAM as u32) << 26 | (size & 0xFF_FFFF),
            );
            self.ram.set_u32(sample_addr + 4, r.left_ring_buf);
            self.ram.set_u32(sample_addr + 8, loop_addr);
            let ts = self.ram.alloc(8, 8);
            self.ram.set_u32(ts, sample_addr);
            self.ram.set_u32(ts + 4, 1.0f32.to_bits());
            r.tuned_sample = ts;

            if r.downsample_rate != 1 {
                r.unk_0e = (0x8000 / r.downsample_rate as u32) as u16;
                for st in r.resample_state.iter_mut() {
                    *st = self.ram.alloc(32, 16);
                }
                for j in 0..self.bp.ticks_per_update as usize {
                    for frame in 0..2 {
                        let buf = self.ram.alloc(DMEM_2CH_SIZE as usize, 16);
                        r.items[frame][j].to_downsample_left = buf;
                        r.items[frame][j].to_downsample_right = buf + DMEM_1CH_SIZE as u32;
                    }
                }
            }
            if s.low_pass_cutoff_left != 0 {
                r.filter_left_state = self.ram.alloc(2 * (16 + 16), 16);
                r.filter_left = self.ram.alloc(16, 16);
                self.load_low_pass_filter(r.filter_left, s.low_pass_cutoff_left as usize);
            }
            if s.low_pass_cutoff_right != 0 {
                r.filter_right_state = self.ram.alloc(2 * (16 + 16), 16);
                r.filter_right = self.ram.alloc(16, 16);
                self.load_low_pass_filter(r.filter_right, s.low_pass_cutoff_right as usize);
            }
            self.synthesis_reverbs[i] = r;
        }

        // Sequence players, channels and layers.
        self.init_sequence_players();
        self.ai_queue = 0.0;
    }

    fn init_adsr_decay_table(&mut self) {
        let inv = self.bp.ticks_per_update_inv_scaled;
        let calc = |scale_inv: f32| (256.0f32 * inv) / scale_inv;
        let t = &mut self.adsr_decay_table;
        t[255] = calc(0.25);
        t[254] = calc(0.33);
        t[253] = calc(0.5);
        t[252] = calc(0.66);
        t[251] = calc(0.75);
        for i in 128..251 {
            t[i] = calc((251 - i) as f32);
        }
        for i in 16..128 {
            t[i] = calc((4 * (143 - i)) as f32);
        }
        for i in 1..16 {
            t[i] = calc((60 * (23 - i)) as f32);
        }
        t[0] = 0.0;
    }

    fn load_low_pass_filter(&mut self, filter: u32, cutoff: usize) {
        for i in 0..8 {
            self.ram.set_u16(
                filter + 2 * i as u32,
                tables::LOW_PASS_FILTER[8 * cutoff + i] as u16,
            );
        }
    }

    fn load_high_pass_filter(&mut self, filter: u32, cutoff: usize) {
        for i in 0..8 {
            self.ram.set_u16(
                filter + 2 * i as u32,
                tables::HIGH_PASS_FILTER[8 * (cutoff - 1) + i] as u16,
            );
        }
    }

    /// `AudioHeap_LoadFilter`
    fn load_filter(&mut self, filter: u32, low_pass_cutoff: usize, high_pass_cutoff: usize) {
        if low_pass_cutoff == 0 && high_pass_cutoff == 0 {
            self.load_low_pass_filter(filter, 0);
        } else if high_pass_cutoff == 0 {
            self.load_low_pass_filter(filter, low_pass_cutoff);
        } else if low_pass_cutoff == 0 {
            self.load_high_pass_filter(filter, high_pass_cutoff);
        } else {
            for i in 0..8 {
                let a = tables::LOW_PASS_FILTER[8 * low_pass_cutoff + i] as i32;
                let b = tables::HIGH_PASS_FILTER[8 * (high_pass_cutoff - 1) + i] as i32;
                self.ram
                    .set_u16(filter + 2 * i as u32, ((a + b) / 2) as u16);
            }
        }
    }

    /// One audio frame (`AudioThread_Update` + the RSP task): run the
    /// sequence ticks, build and execute the command list, and append the
    /// produced stereo samples to `out`.
    pub fn update(&mut self, out: &mut Vec<[i16; 2]>) {
        self.total_task_count = self.total_task_count.wrapping_add(1);
        let len = self.next_ai_buffer_length();
        let mut cmds = Vec::with_capacity(512);
        self.synth_update(&mut cmds, self.ai_buf, len as i32);
        self.rsp.run(&mut self.ram, &cmds);
        Rsp::read_output(&self.ram, self.ai_buf, len, out);

        // AudioThread_Update's random update. osGetCount() is the CPU cycle
        // counter, which an offline render can't reproduce; use a fixed
        // sequence so renders are deterministic.
        self.os_count = self
            .os_count
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        self.audio_random = self
            .audio_random
            .wrapping_add(self.total_task_count)
            .wrapping_mul(self.os_count);
        let idx = (self.total_task_count & 0xFF) as usize % len.max(1);
        self.audio_random = self
            .audio_random
            .wrapping_add(self.ram.s16(self.ai_buf + 2 * idx as u32) as i32 as u32);
        self.wave_samples[8] = self.noise_base + (self.audio_random & 0xFFF0);
    }

    /// `aiBufLengths[index]` from AudioThread_Update, with the audio
    /// interface's playback simulated: it drains `aiFreq / viRate` samples per
    /// video frame.
    fn next_ai_buffer_length(&mut self) -> usize {
        let bp = self.bp;
        let remaining = self.ai_queue.max(0.0) as i32;
        let mut len = (((bp.samples_per_frame_target as i32 - remaining) + 0x80) & !0xF) + 0x10;
        len = len.clamp(
            bp.min_ai_buffer_length as i32,
            bp.max_ai_buffer_length as i32,
        );
        // The real AI holds two buffers; model its queue as the backlog beyond
        // one frame's worth.
        let per_frame = bp.ai_sampling_frequency as f64 / self.vi_rate;
        self.ai_queue = (self.ai_queue + len as f64 - per_frame).max(0.0);
        if self.ai_queue > per_frame * 2.0 {
            self.ai_queue = per_frame * 2.0;
        }
        len as usize
    }

    /// The AI's output sample rate.
    pub fn output_rate(&self) -> u32 {
        self.bp.ai_sampling_frequency as u32
    }

    /// `AudioThread_NextRandom`
    pub fn next_random(&mut self) -> u32 {
        self.os_count = self
            .os_count
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        self.next_random_state = (self.os_count.wrapping_add(0x1234567))
            .wrapping_mul(self.next_random_state.wrapping_add(self.total_task_count));
        self.next_random_state = self.next_random_state.wrapping_add(self.audio_random);
        self.next_random_state
    }
}

fn alloc_i16s(ram: &mut Ram, data: &[i16]) -> u32 {
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_be_bytes()).collect();
    ram.alloc_bytes(&bytes, 16)
}
