//! Builds the RSP command list for one audio frame (`synthesis.c`, original
//! name `driver.c`): per-note sample decode, resampling, filters, envelope
//! mixing, Haas effect, and the reverb ring buffers.

use super::{
    AudioContext, CODEC_ADPCM, CODEC_REVERB, CODEC_S8, CODEC_S16, CODEC_S16_INMEMORY,
    CODEC_SMALL_ADPCM, DMEM_1CH_SIZE, DMEM_2CH_SIZE, MEDIUM_RAM, MEDIUM_UNK, SAMPLE_SIZE,
    SAMPLES_PER_FRAME, SOUND_OUTPUT_MONO, SYNTH_ADPCMDEC_STATE, SYNTH_COMB_FILTER_STATE,
    SYNTH_FINAL_RESAMPLE_STATE, SYNTH_HAAS_DELAY_STATE, SYNTH_MIX_ENVELOPE_STATE,
    WAVE_SAMPLE_COUNT,
};
use crate::rsp::{self, A_CONTINUE, A_INIT, A_LOOP, Acmd};

const DMEM_TEMP: i32 = 0x3C0;
const DMEM_UNCOMPRESSED_NOTE: i32 = 0x580;
const DMEM_HAAS_TEMP: i32 = 0x5C0;
const DMEM_COMB_TEMP: i32 = 0x760;
const DMEM_COMPRESSED_ADPCM_DATA: i32 = 0x940;
const DMEM_LEFT_CH: i32 = 0x940;
const DMEM_RIGHT_CH: i32 = 0xAE0;
const DMEM_WET_TEMP: i32 = 0x3E0;
const DMEM_WET_SCRATCH: i32 = 0x720;
const DMEM_WET_LEFT_CH: i32 = 0xC80;
const DMEM_WET_RIGHT_CH: i32 = 0xE20;

const HAAS_EFFECT_DELAY_NONE: i32 = 0;
const HAAS_EFFECT_DELAY_LEFT: i32 = 1;
const HAAS_EFFECT_DELAY_RIGHT: i32 = 2;

const fn mk_cmd(b0: i32, b1: i32, b2: i32, b3: i32) -> u32 {
    ((b0 as u32 & 0xFF) << 24)
        | ((b1 as u32 & 0xFF) << 16)
        | ((b2 as u32 & 0xFF) << 8)
        | (b3 as u32 & 0xFF)
}

const ENV_MIXER_LEFT_HAAS_DESTS: u32 = mk_cmd(
    DMEM_HAAS_TEMP >> 4,
    DMEM_RIGHT_CH >> 4,
    DMEM_WET_LEFT_CH >> 4,
    DMEM_WET_RIGHT_CH >> 4,
);
const ENV_MIXER_RIGHT_HAAS_DESTS: u32 = mk_cmd(
    DMEM_LEFT_CH >> 4,
    DMEM_HAAS_TEMP >> 4,
    DMEM_WET_LEFT_CH >> 4,
    DMEM_WET_RIGHT_CH >> 4,
);
const ENV_MIXER_DEFAULT_DESTS: u32 = mk_cmd(
    DMEM_LEFT_CH >> 4,
    DMEM_RIGHT_CH >> 4,
    DMEM_WET_LEFT_CH >> 4,
    DMEM_WET_RIGHT_CH >> 4,
);

const NUM_SAMPLES_PER_WAVE_PERIOD: [i32; 4] = [
    WAVE_SAMPLE_COUNT,
    WAVE_SAMPLE_COUNT / 2,
    WAVE_SAMPLE_COUNT / 4,
    WAVE_SAMPLE_COUNT / 8,
];

fn align16(x: i32) -> i32 {
    (x + 0xF) & !0xF
}
fn align8(x: i32) -> i32 {
    (x + 7) & !7
}
fn align64(x: i32) -> i32 {
    (x + 0x3F) & !0x3F
}

fn u(x: i32) -> u32 {
    x as u32
}

impl AudioContext {
    /// `AudioSynth_Update`: run every tick of this frame's sequences, then
    /// emit the command list producing `ai_buf_len` stereo samples at `ai_buf`.
    pub(crate) fn synth_update(&mut self, cmd: &mut Vec<Acmd>, ai_buf: u32, ai_buf_len: i32) {
        let ticks = self.bp.ticks_per_update as i32;
        for i in (1..=ticks).rev() {
            self.process_sequences(i - 1);
            self.push_drv_reg(ticks - i);
        }

        let mut ai_buf_p = ai_buf;
        let mut ai_buf_len = ai_buf_len;
        self.cur_loaded_book = 0;

        for i in (1..=ticks).rev() {
            let bp = self.bp;
            let chunk_len = if i == 1 {
                ai_buf_len
            } else if ai_buf_len / i >= bp.samples_per_tick_max as i32 {
                bp.samples_per_tick_max as i32
            } else if bp.samples_per_tick_min as i32 >= ai_buf_len / i {
                bp.samples_per_tick_min as i32
            } else {
                bp.samples_per_tick as i32
            };
            for j in 0..self.num_synthesis_reverbs {
                if self.synthesis_reverbs[j].use_reverb != 0 {
                    self.init_next_ring_buf(chunk_len, ticks - i, j);
                }
            }
            self.do_one_audio_update(cmd, ai_buf_p, chunk_len, ticks - i);
            ai_buf_len -= chunk_len;
            ai_buf_p = ai_buf_p.wrapping_add(u(4 * chunk_len));
        }

        for j in 0..self.num_synthesis_reverbs {
            let r = &mut self.synthesis_reverbs[j];
            if r.frames_to_ignore != 0 {
                r.frames_to_ignore -= 1;
            }
            r.cur_frame ^= 1;
        }
    }

    /// `func_800DB03C` (`__Nas_PushDrvReg`)
    fn push_drv_reg(&mut self, update_index: i32) {
        let base = self.num_notes * update_index as usize;
        for i in 0..self.num_notes {
            if self.notes[i].sample_state.enabled {
                self.notes[i].sample_state.needs_init = false;
            } else {
                self.sample_states[base + i].enabled = false;
            }
            self.notes[i].sample_state.harmonic_index_cur_and_prev = 0;
        }
    }

    /// `AudioSynth_InitNextRingBuf` (`Nas_CpuFX`)
    fn init_next_ring_buf(&mut self, chunk_len: i32, update_index: i32, reverb_index: usize) {
        let ui = update_index as usize;
        let r = self.synthesis_reverbs[reverb_index].clone();
        if r.downsample_rate >= 2 && r.frames_to_ignore == 0 {
            let item = r.items[r.cur_frame as usize][ui];
            let rate = r.downsample_rate as u32;
            let mut j = 0u32;
            for i in 0..(item.length_a as i32 / SAMPLE_SIZE) as u32 {
                let l = self.ram.u16(item.to_downsample_left + 2 * j);
                let rr = self.ram.u16(item.to_downsample_right + 2 * j);
                let pos = (item.start_pos as u32).wrapping_add(i);
                self.ram.set_u16(r.left_ring_buf + 2 * pos, l);
                self.ram.set_u16(r.right_ring_buf + 2 * pos, rr);
                j += rate;
            }
            for i in 0..(item.length_b as i32 / SAMPLE_SIZE) as u32 {
                let l = self.ram.u16(item.to_downsample_left + 2 * j);
                let rr = self.ram.u16(item.to_downsample_right + 2 * j);
                self.ram.set_u16(r.left_ring_buf + 2 * i, l);
                self.ram.set_u16(r.right_ring_buf + 2 * i, rr);
                j += rate;
            }
        }

        let r = &mut self.synthesis_reverbs[reverb_index];
        let cur = r.cur_frame as usize;
        let num_samples = chunk_len / r.downsample_rate as i32;
        let extra = (num_samples + r.next_ring_buf_pos) - r.buf_size_per_chan;
        let start = r.next_ring_buf_pos;
        {
            let item = &mut r.items[cur][ui];
            if extra < 0 {
                item.length_a = (num_samples * SAMPLE_SIZE) as i16;
                item.length_b = 0;
                item.start_pos = r.next_ring_buf_pos;
                r.next_ring_buf_pos += num_samples;
            } else {
                item.length_a = ((num_samples - extra) * SAMPLE_SIZE) as i16;
                item.length_b = (extra * SAMPLE_SIZE) as i16;
                item.start_pos = r.next_ring_buf_pos;
                r.next_ring_buf_pos = extra;
            }
        }

        if r.unk_14 != 0 {
            let mut pos = r.unk_14 as i32 + start;
            if pos >= r.buf_size_per_chan {
                pos -= r.buf_size_per_chan;
            }
            let extra = (pos + num_samples) - r.buf_size_per_chan;
            let item = &mut r.items2[cur][ui];
            if extra < 0 {
                item.length_a = (num_samples * SAMPLE_SIZE) as i16;
                item.length_b = 0;
            } else {
                item.length_a = ((num_samples - extra) * SAMPLE_SIZE) as i16;
                item.length_b = (extra * SAMPLE_SIZE) as i16;
            }
            item.start_pos = pos;
        }
    }

    fn load_ring_buffer_part(
        &self,
        cmd: &mut Vec<Acmd>,
        dmem: i32,
        start_pos: i32,
        size: i32,
        reverb: usize,
    ) {
        let r = &self.synthesis_reverbs[reverb];
        let off = u(start_pos as u16 as i32 * 2);
        cmd.push(rsp::load_buffer(
            r.left_ring_buf.wrapping_add(off),
            u(dmem),
            u(size),
        ));
        cmd.push(rsp::load_buffer(
            r.right_ring_buf.wrapping_add(off),
            u(dmem + DMEM_1CH_SIZE),
            u(size),
        ));
    }

    fn save_ring_buffer_part(
        &self,
        cmd: &mut Vec<Acmd>,
        dmem: i32,
        start_pos: i32,
        size: i32,
        reverb: usize,
    ) {
        let r = &self.synthesis_reverbs[reverb];
        let off = u(start_pos as u16 as i32 * 2);
        cmd.push(rsp::save_buffer(
            u(dmem),
            r.left_ring_buf.wrapping_add(off),
            u(size),
        ));
        cmd.push(rsp::save_buffer(
            u(dmem + DMEM_1CH_SIZE),
            r.right_ring_buf.wrapping_add(off),
            u(size),
        ));
    }

    /// `AudioSynth_LoadRingBuffer1AtTemp`
    fn load_ring_buffer1_at_temp(&self, cmd: &mut Vec<Acmd>, reverb: usize, ui: usize) {
        let r = &self.synthesis_reverbs[reverb];
        let item = r.items[r.cur_frame as usize][ui];
        self.load_ring_buffer_part(
            cmd,
            DMEM_WET_TEMP,
            item.start_pos,
            item.length_a as i32,
            reverb,
        );
        if item.length_b != 0 {
            self.load_ring_buffer_part(
                cmd,
                DMEM_WET_TEMP + item.length_a as i32,
                0,
                item.length_b as i32,
                reverb,
            );
        }
    }

    /// `AudioSynth_SaveRingBuffer1AtTemp`
    fn save_ring_buffer1_at_temp(&self, cmd: &mut Vec<Acmd>, reverb: usize, ui: usize) {
        let r = &self.synthesis_reverbs[reverb];
        let item = r.items[r.cur_frame as usize][ui];
        self.save_ring_buffer_part(
            cmd,
            DMEM_WET_TEMP,
            item.start_pos,
            item.length_a as i32,
            reverb,
        );
        if item.length_b != 0 {
            self.save_ring_buffer_part(
                cmd,
                DMEM_WET_TEMP + item.length_a as i32,
                0,
                item.length_b as i32,
                reverb,
            );
        }
    }

    /// `AudioSynth_LeakReverb`
    fn leak_reverb(&self, cmd: &mut Vec<Acmd>, reverb: usize) {
        let r = &self.synthesis_reverbs[reverb];
        cmd.push(rsp::dmem_move(
            u(DMEM_WET_LEFT_CH),
            u(DMEM_WET_SCRATCH),
            u(DMEM_1CH_SIZE),
        ));
        cmd.push(rsp::mix(
            u(DMEM_1CH_SIZE >> 4),
            r.leak_rtl as u16 as u32,
            u(DMEM_WET_RIGHT_CH),
            u(DMEM_WET_LEFT_CH),
        ));
        cmd.push(rsp::mix(
            u(DMEM_1CH_SIZE >> 4),
            r.leak_ltr as u16 as u32,
            u(DMEM_WET_SCRATCH),
            u(DMEM_WET_RIGHT_CH),
        ));
    }

    /// `func_800DB4E4` (`Nas_LoadAuxBufferC`): load a downsampled reverb and
    /// upsample it back.
    fn load_aux_buffer_c(&self, cmd: &mut Vec<Acmd>, ai_buf_len: i32, reverb: usize, ui: usize) {
        let r = &self.synthesis_reverbs[reverb];
        let item = r.items[r.cur_frame as usize][ui];
        let offset_a = (item.start_pos & 7) * SAMPLE_SIZE;
        let offset_b = align16(offset_a + item.length_a as i32);
        self.load_ring_buffer_part(
            cmd,
            DMEM_WET_TEMP,
            item.start_pos - offset_a / SAMPLE_SIZE,
            DMEM_1CH_SIZE,
            reverb,
        );
        if item.length_b != 0 {
            self.load_ring_buffer_part(
                cmd,
                DMEM_WET_TEMP + offset_b,
                0,
                DMEM_1CH_SIZE - offset_b,
                reverb,
            );
        }
        cmd.push(rsp::set_buffer(
            0,
            u(DMEM_WET_TEMP + offset_a),
            u(DMEM_WET_LEFT_CH),
            u(ai_buf_len * SAMPLE_SIZE),
        ));
        cmd.push(rsp::resample(
            r.resample_flags as u32,
            r.unk_0e as u32,
            r.resample_state[0],
        ));
        cmd.push(rsp::set_buffer(
            0,
            u(DMEM_WET_TEMP + DMEM_1CH_SIZE + offset_a),
            u(DMEM_WET_RIGHT_CH),
            u(ai_buf_len * SAMPLE_SIZE),
        ));
        cmd.push(rsp::resample(
            r.resample_flags as u32,
            r.unk_0e as u32,
            r.resample_state[1],
        ));
    }

    /// `AudioSynth_FilterReverb`
    fn filter_reverb(&self, cmd: &mut Vec<Acmd>, size: i32, reverb: usize) {
        let r = &self.synthesis_reverbs[reverb];
        if r.filter_left != 0 {
            cmd.push(rsp::filter(2, u(size), r.filter_left));
            cmd.push(rsp::filter(
                r.resample_flags as u32,
                u(DMEM_WET_LEFT_CH),
                r.filter_left_state,
            ));
        }
        if r.filter_right != 0 {
            cmd.push(rsp::filter(2, u(size), r.filter_right));
            cmd.push(rsp::filter(
                r.resample_flags as u32,
                u(DMEM_WET_RIGHT_CH),
                r.filter_right_state,
            ));
        }
    }

    /// `AudioSynth_MaybeMixRingBuffer1`
    fn maybe_mix_ring_buffer1(&self, cmd: &mut Vec<Acmd>, reverb: usize, ui: usize) {
        let r = &self.synthesis_reverbs[reverb];
        let other = r.unk_05 as u8 as usize & 3;
        if self.synthesis_reverbs[other].downsample_rate == 1 {
            self.load_ring_buffer1_at_temp(cmd, other, ui);
            cmd.push(rsp::mix(
                u(DMEM_2CH_SIZE >> 4),
                r.unk_08 as u16 as u32,
                u(DMEM_WET_LEFT_CH),
                u(DMEM_WET_TEMP),
            ));
            self.save_ring_buffer1_at_temp(cmd, other, ui);
        }
    }

    /// `AudioSynth_LoadRingBuffer1` / `AudioSynth_LoadRingBuffer2`
    fn load_ring_buffer_items(&self, cmd: &mut Vec<Acmd>, reverb: usize, ui: usize, second: bool) {
        let r = &self.synthesis_reverbs[reverb];
        let item = if second { r.items2 } else { r.items }[r.cur_frame as usize][ui];
        self.load_ring_buffer_part(
            cmd,
            DMEM_WET_LEFT_CH,
            item.start_pos,
            item.length_a as i32,
            reverb,
        );
        if item.length_b != 0 {
            self.load_ring_buffer_part(
                cmd,
                DMEM_WET_LEFT_CH + item.length_a as i32,
                0,
                item.length_b as i32,
                reverb,
            );
        }
    }

    /// `AudioSynth_SaveRingBuffer2` and the downsample-rate-1 half of
    /// `AudioSynth_SaveReverbSamples`.
    fn save_ring_buffer_items(&self, cmd: &mut Vec<Acmd>, reverb: usize, ui: usize, second: bool) {
        let r = &self.synthesis_reverbs[reverb];
        let item = if second { r.items2 } else { r.items }[r.cur_frame as usize][ui];
        self.save_ring_buffer_part(
            cmd,
            DMEM_WET_LEFT_CH,
            item.start_pos,
            item.length_a as i32,
            reverb,
        );
        if item.length_b != 0 {
            self.save_ring_buffer_part(
                cmd,
                DMEM_WET_LEFT_CH + item.length_a as i32,
                0,
                item.length_b as i32,
                reverb,
            );
        }
    }

    /// `AudioSynth_LoadReverbSamples`
    fn load_reverb_samples(&self, cmd: &mut Vec<Acmd>, ai_buf_len: i32, reverb: usize, ui: usize) {
        if self.synthesis_reverbs[reverb].downsample_rate == 1 {
            // unk_18 is always 0, so func_800DB828 is never used.
            self.load_ring_buffer_items(cmd, reverb, ui, false);
        } else {
            self.load_aux_buffer_c(cmd, ai_buf_len, reverb, ui);
        }
    }

    /// `AudioSynth_SaveReverbSamples`
    fn save_reverb_samples(&mut self, cmd: &mut Vec<Acmd>, reverb: usize, ui: usize) {
        let r = &self.synthesis_reverbs[reverb];
        if r.downsample_rate == 1 {
            self.save_ring_buffer_items(cmd, reverb, ui, false);
        } else {
            let dst = r.items[r.cur_frame as usize][ui].to_downsample_left;
            cmd.push(rsp::save_buffer(u(DMEM_WET_LEFT_CH), dst, u(DMEM_2CH_SIZE)));
        }
        self.synthesis_reverbs[reverb].resample_flags = 0;
    }

    /// `AudioSynth_DoOneAudioUpdate` (`Nas_DriveRsp`)
    fn do_one_audio_update(
        &mut self,
        cmd: &mut Vec<Acmd>,
        ai_buf: u32,
        ai_buf_len: i32,
        update_index: i32,
    ) {
        let ui = update_index as usize;
        let t = self.num_notes * ui;
        let mut note_indices: Vec<usize> = Vec::with_capacity(self.num_notes);
        if self.num_synthesis_reverbs == 0 {
            for i in 0..self.num_notes {
                if self.sample_states[t + i].enabled {
                    note_indices.push(i);
                }
            }
        } else {
            for ri in 0..self.num_synthesis_reverbs {
                for i in 0..self.num_notes {
                    let s = &self.sample_states[t + i];
                    if s.enabled && s.reverb_index as usize == ri {
                        note_indices.push(i);
                    }
                }
            }
            for i in 0..self.num_notes {
                let s = &self.sample_states[t + i];
                if s.enabled && s.reverb_index as usize >= self.num_synthesis_reverbs {
                    note_indices.push(i);
                }
            }
        }

        cmd.push(rsp::clear_buffer(u(DMEM_LEFT_CH), u(DMEM_2CH_SIZE)));

        let mut i = 0;
        for ri in 0..self.num_synthesis_reverbs {
            let use_reverb = self.synthesis_reverbs[ri].use_reverb != 0;
            let unk14 = self.synthesis_reverbs[ri].unk_14 != 0;
            if use_reverb {
                self.load_reverb_samples(cmd, ai_buf_len, ri, ui);
                let r = &self.synthesis_reverbs[ri];
                cmd.push(rsp::mix(
                    u(DMEM_2CH_SIZE >> 4),
                    r.volume as u16 as u32,
                    u(DMEM_WET_LEFT_CH),
                    u(DMEM_LEFT_CH),
                ));
                if unk14 {
                    cmd.push(rsp::dmem_move(
                        u(DMEM_WET_LEFT_CH),
                        u(DMEM_WET_TEMP),
                        u(DMEM_2CH_SIZE),
                    ));
                }
                // Decay the reverb: (+ 0x8000) is -100%.
                cmd.push(rsp::mix(
                    u(DMEM_2CH_SIZE >> 4),
                    (r.decay_ratio as u32 + 0x8000) & 0xFFFF,
                    u(DMEM_WET_LEFT_CH),
                    u(DMEM_WET_LEFT_CH),
                ));
                if (r.leak_rtl != 0 || r.leak_ltr != 0)
                    && self.sound_output_mode != SOUND_OUTPUT_MONO
                {
                    self.leak_reverb(cmd, ri);
                }
                if unk14 {
                    self.save_reverb_samples(cmd, ri, ui);
                    if self.synthesis_reverbs[ri].unk_05 != -1 {
                        self.maybe_mix_ring_buffer1(cmd, ri, ui);
                    }
                    if self.synthesis_reverbs[ri].downsample_rate == 1 {
                        self.load_ring_buffer_items(cmd, ri, ui, true);
                    }
                    let r = &self.synthesis_reverbs[ri];
                    cmd.push(rsp::mix(
                        u(DMEM_2CH_SIZE >> 4),
                        r.unk_16 as u16 as u32,
                        u(DMEM_WET_TEMP),
                        u(DMEM_WET_LEFT_CH),
                    ));
                }
            }

            while i < note_indices.len() {
                let n = note_indices[i];
                if self.sample_states[n + t].reverb_index as usize == ri {
                    self.process_note(cmd, n, n + t, ai_buf_len, update_index);
                } else {
                    break;
                }
                i += 1;
            }

            if use_reverb {
                let r = &self.synthesis_reverbs[ri];
                if r.filter_left != 0 || r.filter_right != 0 {
                    self.filter_reverb(cmd, ai_buf_len * SAMPLE_SIZE, ri);
                }
                if unk14 {
                    self.save_ring_buffer_items(cmd, ri, ui, true);
                } else {
                    self.save_reverb_samples(cmd, ri, ui);
                    if self.synthesis_reverbs[ri].unk_05 != -1 {
                        self.maybe_mix_ring_buffer1(cmd, ri, ui);
                    }
                }
            }
        }

        while i < note_indices.len() {
            let n = note_indices[i];
            self.process_note(cmd, n, n + t, ai_buf_len, update_index);
            i += 1;
        }

        cmd.push(rsp::interleave(
            u(DMEM_TEMP),
            u(DMEM_LEFT_CH),
            u(DMEM_RIGHT_CH),
            u(2 * ai_buf_len),
        ));
        cmd.push(rsp::save_buffer(
            u(DMEM_TEMP),
            ai_buf,
            u(2 * ai_buf_len * SAMPLE_SIZE),
        ));
    }

    /// `func_800DB2C0`: a finished note stops for the rest of the frame.
    fn wave_terminate_process(&mut self, update_index: i32, note: usize) {
        for i in (update_index + 1)..self.bp.ticks_per_update as i32 {
            let s = &mut self.sample_states[self.num_notes * i as usize + note];
            if !s.needs_init {
                s.enabled = false;
            } else {
                break;
            }
        }
    }

    /// `AudioSynth_ProcessNote` (`Nas_SynthMain`)
    fn process_note(
        &mut self,
        cmd: &mut Vec<Acmd>,
        note_index: usize,
        ss: usize,
        ai_buf_len: i32,
        update_index: i32,
    ) {
        let sample_state = self.sample_states[ss];
        let book_offset = sample_state.book_offset as i32;
        let mut finished = sample_state.finished;
        let mut flags = A_CONTINUE;
        let buffers = self.notes[note_index].synthesis_state.synthesis_buffers;

        if sample_state.needs_init {
            flags = A_INIT;
            let start = self.notes[note_index].start_sample_pos as i32;
            let syn = &mut self.notes[note_index].synthesis_state;
            syn.restart = false;
            syn.sample_pos_int = start;
            syn.sample_pos_frac = 0;
            syn.cur_vol_left = 0;
            syn.cur_vol_right = 0;
            syn.prev_haas_effect_left_delay_size = 0;
            syn.prev_haas_effect_right_delay_size = 0;
            syn.reverb_vol = sample_state.reverb_vol;
            syn.num_parts = 0;
            syn.comb_filter_needs_init = true;
            self.notes[note_index].sample_state.finished = false;
            finished = false;
        }

        let resampling_rate = sample_state.resampling_rate_fixed_point;
        let n_parts = sample_state.has_two_parts as i32 + 1;
        let samples_len_fixed_point = (resampling_rate as u32)
            .wrapping_mul(u(ai_buf_len))
            .wrapping_mul(2)
            .wrapping_add(self.notes[note_index].synthesis_state.sample_pos_frac as u32);
        let num_samples_to_load = (samples_len_fixed_point >> 16) as i32;
        let mut skip_bytes: i32 = 0;
        self.notes[note_index].synthesis_state.sample_pos_frac = samples_len_fixed_point as u16;
        self.notes[note_index].synthesis_state.num_parts = n_parts as u8;

        let mut sample_dmem_before_resampling: i32 = DMEM_UNCOMPRESSED_NOTE;

        if sample_state.is_synthetic_wave {
            self.load_wave_samples(cmd, ss, note_index, num_samples_to_load);
            let syn = &mut self.notes[note_index].synthesis_state;
            sample_dmem_before_resampling =
                DMEM_UNCOMPRESSED_NOTE + syn.sample_pos_int * SAMPLE_SIZE;
            syn.sample_pos_int += num_samples_to_load;
        } else {
            let sample = self.tuned_sample_sample(sample_state.tuned_sample);
            let codec = self.sample_codec(sample);
            let medium = self.sample_medium(sample);
            let loop_info = self.sample_loop(sample);
            let loop_start = self.ram.u32(loop_info) as i32;
            let loop_end_pos = self.ram.u32(loop_info + 4) as i32;
            let loop_count = self.ram.u32(loop_info + 8);
            let sample_addr = self.sample_addr(sample);
            let book = self.sample_book(sample);
            let mut resampled_temp_len = 0;

            for cur_part in 0..n_parts {
                let mut n_samples_processed = 0;
                let mut s5: i32 = 0;
                let samples_len_adjusted = if n_parts == 1 {
                    num_samples_to_load
                } else if num_samples_to_load & 1 != 0 {
                    (num_samples_to_load & !1) + cur_part * 2
                } else {
                    num_samples_to_load
                };

                if (codec == CODEC_ADPCM || codec == CODEC_SMALL_ADPCM)
                    && self.cur_loaded_book != book + 8
                {
                    self.cur_loaded_book = match book_offset {
                        1 => self.book_offset_1,
                        _ => book + 8,
                    };
                    let order = self.ram.u32(book) as i32;
                    let npred = self.ram.u32(book + 4) as i32;
                    let n_entries = SAMPLES_PER_FRAME * order * npred;
                    cmd.push(rsp::load_adpcm(u(n_entries), self.cur_loaded_book));
                }

                while n_samples_processed != samples_len_adjusted {
                    let mut note_finished = false;
                    let mut restart = false;
                    let mut phi_s4 = 0;
                    let syn = self.notes[note_index].synthesis_state;
                    let mut n_first_frame_samples_to_ignore = syn.sample_pos_int & 0xF;
                    let n_samples_until_loop_end = loop_end_pos - syn.sample_pos_int;
                    let n_samples_to_process = samples_len_adjusted - n_samples_processed;
                    if n_first_frame_samples_to_ignore == 0 && !syn.restart {
                        n_first_frame_samples_to_ignore = SAMPLES_PER_FRAME;
                    }
                    let mut n_samples_in_first_frame =
                        SAMPLES_PER_FRAME - n_first_frame_samples_to_ignore;
                    let mut n_samples_to_decode;
                    let n_trailing_samples_to_ignore;
                    let n_frames_to_decode;

                    if n_samples_to_process < n_samples_until_loop_end {
                        n_frames_to_decode = (n_samples_to_process - n_samples_in_first_frame
                            + SAMPLES_PER_FRAME
                            - 1)
                            / SAMPLES_PER_FRAME;
                        n_samples_to_decode = n_frames_to_decode * SAMPLES_PER_FRAME;
                        n_trailing_samples_to_ignore =
                            n_samples_in_first_frame + n_samples_to_decode - n_samples_to_process;
                    } else {
                        n_samples_to_decode = n_samples_until_loop_end - n_samples_in_first_frame;
                        n_trailing_samples_to_ignore = 0;
                        if n_samples_to_decode <= 0 {
                            n_samples_to_decode = 0;
                            n_samples_in_first_frame = n_samples_until_loop_end;
                        }
                        n_frames_to_decode =
                            (n_samples_to_decode + SAMPLES_PER_FRAME - 1) / SAMPLES_PER_FRAME;
                        if loop_count != 0 {
                            restart = true;
                        } else {
                            note_finished = true;
                        }
                    }

                    let (frame_size, skip_initial_samples, sample_data_start) = match codec {
                        CODEC_ADPCM => (9, SAMPLES_PER_FRAME, 0),
                        CODEC_SMALL_ADPCM => (5, SAMPLES_PER_FRAME, 0),
                        CODEC_S8 => (16, SAMPLES_PER_FRAME, 0),
                        _ => (0, 0, 0),
                    };

                    let mut skipped = false;
                    if codec == CODEC_S16_INMEMORY || codec == CODEC_S16 {
                        cmd.push(rsp::clear_buffer(
                            u(DMEM_UNCOMPRESSED_NOTE),
                            u((samples_len_adjusted + SAMPLES_PER_FRAME) * SAMPLE_SIZE),
                        ));
                        flags = A_CONTINUE;
                        skip_bytes = 0;
                        n_samples_processed = samples_len_adjusted;
                        s5 = samples_len_adjusted;
                        skipped = true;
                    } else if codec == CODEC_REVERB || codec > CODEC_S16 {
                        // The game reads an uninitialized frame size here;
                        // treat these as silence.
                        cmd.push(rsp::clear_buffer(
                            u(DMEM_UNCOMPRESSED_NOTE),
                            u((samples_len_adjusted + SAMPLES_PER_FRAME) * SAMPLE_SIZE),
                        ));
                        n_samples_processed = samples_len_adjusted;
                        s5 = samples_len_adjusted;
                        skipped = true;
                    }

                    if !skipped {
                        let sample_data_start_pad;
                        let mut n_samples_to_decode = n_samples_to_decode;
                        if n_frames_to_decode != 0 {
                            let frame_index = (syn.sample_pos_int + skip_initial_samples
                                - n_first_frame_samples_to_ignore)
                                / SAMPLES_PER_FRAME;
                            let sample_data_offset = frame_index * frame_size;
                            if medium == MEDIUM_UNK {
                                return;
                            }
                            // MEDIUM_RAM or a DMA from cartridge: either way
                            // the bytes are at this address in our RAM.
                            let _ = MEDIUM_RAM;
                            let sample_data =
                                sample_addr.wrapping_add(u(sample_data_start + sample_data_offset));
                            sample_data_start_pad = (sample_data & 0xF) as i32;
                            let aligned =
                                align16(n_frames_to_decode * frame_size + SAMPLES_PER_FRAME);
                            let addr = DMEM_COMPRESSED_ADPCM_DATA - aligned;
                            cmd.push(rsp::load_buffer(
                                sample_data.wrapping_sub(u(sample_data_start_pad)),
                                u(addr) & 0xFFFF,
                                u(aligned),
                            ));
                        } else {
                            n_samples_to_decode = 0;
                            sample_data_start_pad = 0;
                        }

                        if syn.restart {
                            cmd.push(rsp::set_loop(loop_info + 0x10));
                            flags = A_LOOP;
                            self.notes[note_index].synthesis_state.restart = false;
                        }

                        let n_samples_in_this_iteration = n_samples_to_decode
                            + n_samples_in_first_frame
                            - n_trailing_samples_to_ignore;
                        if n_samples_processed == 0 {
                            skip_bytes = n_first_frame_samples_to_ignore * SAMPLE_SIZE;
                        } else {
                            phi_s4 = align16(s5 + 8 * SAMPLE_SIZE);
                        }

                        let aligned = align16(n_frames_to_decode * frame_size + SAMPLES_PER_FRAME);
                        let addr = DMEM_COMPRESSED_ADPCM_DATA - aligned;
                        match codec {
                            CODEC_ADPCM | CODEC_SMALL_ADPCM => {
                                cmd.push(rsp::set_buffer(
                                    0,
                                    u(addr + sample_data_start_pad) & 0xFFFF,
                                    u(DMEM_UNCOMPRESSED_NOTE + phi_s4),
                                    u(n_samples_to_decode * SAMPLE_SIZE),
                                ));
                                let f = if codec == CODEC_SMALL_ADPCM {
                                    flags | 4
                                } else {
                                    flags
                                };
                                cmd.push(rsp::adpcm_dec(f, buffers + SYNTH_ADPCMDEC_STATE));
                            }
                            CODEC_S8 => {
                                cmd.push(rsp::set_buffer(
                                    0,
                                    u(addr + sample_data_start_pad) & 0xFFFF,
                                    u(DMEM_UNCOMPRESSED_NOTE + phi_s4),
                                    u(n_samples_to_decode * SAMPLE_SIZE),
                                ));
                                cmd.push(rsp::s8_dec(flags, buffers + SYNTH_ADPCMDEC_STATE));
                            }
                            _ => {}
                        }

                        if n_samples_processed != 0 {
                            cmd.push(rsp::dmem_move(
                                u(DMEM_UNCOMPRESSED_NOTE
                                    + phi_s4
                                    + n_first_frame_samples_to_ignore * SAMPLE_SIZE),
                                u(DMEM_UNCOMPRESSED_NOTE + s5),
                                u(n_samples_in_this_iteration * SAMPLE_SIZE),
                            ));
                        }

                        n_samples_processed += n_samples_in_this_iteration;

                        match flags {
                            A_INIT => {
                                skip_bytes = SAMPLES_PER_FRAME * SAMPLE_SIZE;
                                s5 = (n_samples_to_decode + SAMPLES_PER_FRAME) * SAMPLE_SIZE;
                            }
                            A_LOOP => s5 += n_samples_in_this_iteration * SAMPLE_SIZE,
                            _ => {
                                if s5 != 0 {
                                    s5 += n_samples_in_this_iteration * SAMPLE_SIZE;
                                } else {
                                    s5 = (n_first_frame_samples_to_ignore
                                        + n_samples_in_this_iteration)
                                        * SAMPLE_SIZE;
                                }
                            }
                        }
                        flags = A_CONTINUE;
                    }

                    // skip:
                    if note_finished {
                        cmd.push(rsp::clear_buffer(
                            u(DMEM_UNCOMPRESSED_NOTE + s5),
                            u((samples_len_adjusted - n_samples_processed) * SAMPLE_SIZE),
                        ));
                        finished = true;
                        self.notes[note_index].sample_state.finished = true;
                        self.wave_terminate_process(update_index, note_index);
                        break;
                    } else if restart {
                        let syn = &mut self.notes[note_index].synthesis_state;
                        syn.restart = true;
                        syn.sample_pos_int = loop_start;
                    } else {
                        self.notes[note_index].synthesis_state.sample_pos_int +=
                            n_samples_to_process;
                    }
                }

                match n_parts {
                    1 => sample_dmem_before_resampling = DMEM_UNCOMPRESSED_NOTE + skip_bytes,
                    _ => {
                        if cur_part == 0 {
                            cmd.push(rsp::interl(
                                u(DMEM_UNCOMPRESSED_NOTE + skip_bytes),
                                u(DMEM_TEMP + SAMPLES_PER_FRAME * SAMPLE_SIZE),
                                u(align8(samples_len_adjusted / 2)),
                            ));
                            resampled_temp_len = samples_len_adjusted;
                            sample_dmem_before_resampling =
                                DMEM_TEMP + SAMPLES_PER_FRAME * SAMPLE_SIZE;
                            if finished {
                                cmd.push(rsp::clear_buffer(
                                    u(sample_dmem_before_resampling + resampled_temp_len),
                                    u(samples_len_adjusted + SAMPLES_PER_FRAME),
                                ));
                            }
                        } else {
                            cmd.push(rsp::interl(
                                u(DMEM_UNCOMPRESSED_NOTE + skip_bytes),
                                u(
                                    DMEM_TEMP
                                        + SAMPLES_PER_FRAME * SAMPLE_SIZE
                                        + resampled_temp_len,
                                ),
                                u(align8(samples_len_adjusted / 2)),
                            ));
                        }
                    }
                }
                if finished {
                    break;
                }
            }
        }

        flags = A_CONTINUE;
        if self.sample_states[ss].needs_init {
            self.sample_states[ss].needs_init = false;
            flags = A_INIT;
        }

        // AudioSynth_FinalResample
        let size = ai_buf_len * SAMPLE_SIZE;
        if resampling_rate == 0 {
            cmd.push(rsp::clear_buffer(u(DMEM_TEMP), u(size)));
        } else {
            cmd.push(rsp::set_buffer(
                0,
                u(sample_dmem_before_resampling) & 0xFFFF,
                u(DMEM_TEMP),
                u(size),
            ));
            cmd.push(rsp::resample(
                flags,
                resampling_rate as u32,
                buffers + SYNTH_FINAL_RESAMPLE_STATE,
            ));
        }

        if book_offset == 3 {
            cmd.push(rsp::unk_cmd19(
                u(DMEM_TEMP),
                u(DMEM_TEMP),
                u(ai_buf_len * SAMPLE_SIZE),
                0,
            ));
        }
        if book_offset == 2 {
            cmd.push(rsp::unk_cmd3(
                u(DMEM_TEMP),
                u(DMEM_TEMP),
                u(ai_buf_len * SAMPLE_SIZE),
            ));
        }

        let mut gain = sample_state.gain as i32;
        if gain != 0 {
            // UQ4.4: 0x10 is unity.
            if gain < 0x10 {
                gain = 0x10;
            }
            cmd.push(rsp::hilo_gain(
                u(gain),
                u(DMEM_TEMP),
                0,
                u((ai_buf_len + SAMPLES_PER_FRAME) * SAMPLE_SIZE),
            ));
        }

        if sample_state.filter != 0 {
            cmd.push(rsp::filter(
                2,
                u(ai_buf_len * SAMPLE_SIZE),
                sample_state.filter,
            ));
            cmd.push(rsp::filter(
                flags,
                u(DMEM_TEMP),
                buffers + SYNTH_MIX_ENVELOPE_STATE,
            ));
        }

        // Comb filter: add a slightly delayed copy of the signal.
        let comb_size = sample_state.comb_filter_size as i32;
        let comb_gain = sample_state.comb_filter_gain;
        let comb_state = buffers + SYNTH_COMB_FILTER_STATE;
        if comb_size != 0 && comb_gain != 0 {
            cmd.push(rsp::dmem_move(
                u(DMEM_TEMP),
                u(DMEM_COMB_TEMP),
                u(ai_buf_len * SAMPLE_SIZE),
            ));
            let comb_dmem = DMEM_COMB_TEMP - comb_size;
            if self.notes[note_index]
                .synthesis_state
                .comb_filter_needs_init
            {
                cmd.push(rsp::clear_buffer(u(comb_dmem), u(comb_size)));
                self.notes[note_index]
                    .synthesis_state
                    .comb_filter_needs_init = false;
            } else {
                cmd.push(rsp::load_buffer(comb_state, u(comb_dmem), u(comb_size)));
            }
            cmd.push(rsp::save_buffer(
                u(DMEM_TEMP + ai_buf_len * SAMPLE_SIZE - comb_size),
                comb_state,
                u(comb_size),
            ));
            cmd.push(rsp::mix(
                u((ai_buf_len * SAMPLE_SIZE) >> 4),
                comb_gain as u32,
                u(DMEM_COMB_TEMP),
                u(comb_dmem),
            ));
            cmd.push(rsp::dmem_move(
                u(comb_dmem),
                u(DMEM_TEMP),
                u(ai_buf_len * SAMPLE_SIZE),
            ));
        } else {
            self.notes[note_index]
                .synthesis_state
                .comb_filter_needs_init = true;
        }

        let syn = self.notes[note_index].synthesis_state;
        let haas_side = if sample_state.haas_effect_left_delay_size != 0
            || syn.prev_haas_effect_left_delay_size != 0
        {
            HAAS_EFFECT_DELAY_LEFT
        } else if sample_state.haas_effect_right_delay_size != 0
            || syn.prev_haas_effect_right_delay_size != 0
        {
            HAAS_EFFECT_DELAY_RIGHT
        } else {
            HAAS_EFFECT_DELAY_NONE
        };

        self.process_envelope(cmd, ss, note_index, ai_buf_len, DMEM_TEMP, haas_side);

        if sample_state.use_haas_effect {
            if flags & A_INIT == 0 {
                flags = A_CONTINUE;
            }
            self.apply_haas_effect(
                cmd,
                ss,
                note_index,
                ai_buf_len * SAMPLE_SIZE,
                flags,
                haas_side,
            );
        }
    }

    /// `AudioSynth_ProcessEnvelope`
    fn process_envelope(
        &mut self,
        cmd: &mut Vec<Acmd>,
        ss: usize,
        note: usize,
        ai_buf_len: i32,
        dmem_src: i32,
        haas_side: i32,
    ) {
        let s = self.sample_states[ss];
        let syn = &mut self.notes[note].synthesis_state;
        let cur_vol_left = syn.cur_vol_left;
        let target_vol_left = s.target_vol_left.wrapping_shl(4);
        let reverb_vol = s.reverb_vol as i16;
        let cur_vol_right = syn.cur_vol_right;
        let target_vol_right = s.target_vol_right.wrapping_shl(4);
        let steps = ai_buf_len >> 3;

        let ramp_left = if target_vol_left != cur_vol_left {
            ((target_vol_left as i32 - cur_vol_left as i32) / steps) as i16
        } else {
            0
        };
        let ramp_right = if target_vol_right != cur_vol_right {
            ((target_vol_right as i32 - cur_vol_right as i32) / steps) as i16
        } else {
            0
        };
        let source_reverb_vol = syn.reverb_vol as i16;
        let phi_t1 = (source_reverb_vol & 0x7F) as i32;
        let ramp_reverb = if source_reverb_vol != reverb_vol {
            syn.reverb_vol = reverb_vol as u8;
            ((((reverb_vol & 0x7F) as i32 - phi_t1) << 9) / steps) as i16
        } else {
            0
        };
        syn.cur_vol_left = cur_vol_left.wrapping_add((ramp_left as i32 * steps) as u16);
        syn.cur_vol_right = cur_vol_right.wrapping_add((ramp_right as i32 * steps) as u16);

        let dests = if s.use_haas_effect {
            cmd.push(rsp::clear_buffer(u(DMEM_HAAS_TEMP), u(DMEM_1CH_SIZE)));
            match haas_side {
                HAAS_EFFECT_DELAY_LEFT => ENV_MIXER_LEFT_HAAS_DESTS,
                HAAS_EFFECT_DELAY_RIGHT => ENV_MIXER_RIGHT_HAAS_DESTS,
                _ => ENV_MIXER_DEFAULT_DESTS,
            }
        } else {
            ENV_MIXER_DEFAULT_DESTS
        };
        cmd.push(rsp::env_setup1(
            u(phi_t1 * 2),
            ramp_reverb as u16 as u32,
            ramp_left as u16 as u32,
            ramp_right as u16 as u32,
        ));
        cmd.push(rsp::env_setup2(cur_vol_left as u32, cur_vol_right as u32));
        cmd.push(rsp::env_mixer(
            u(dmem_src),
            u(ai_buf_len),
            ((source_reverb_vol & 0x80) >> 7) as u32,
            [
                s.stereo_headset_effects as u32,
                s.uses_headset_pan_effects as u32,
                s.stereo_strong_right as u32,
                s.stereo_strong_left as u32,
            ],
            dests,
        ));
    }

    /// `AudioSynth_LoadWaveSamples`
    fn load_wave_samples(
        &mut self,
        cmd: &mut Vec<Acmd>,
        ss: usize,
        note: usize,
        num_samples_to_load: i32,
    ) {
        let s = self.sample_states[ss];
        let harmonic = s.harmonic_index_cur_and_prev as i32;
        let mut pos = self.notes[note].synthesis_state.sample_pos_int;
        if s.book_offset != 0 {
            // Noise: read "samples" from elsewhere in memory.
            cmd.push(rsp::load_buffer(
                self.wave_samples[8],
                u(DMEM_UNCOMPRESSED_NOTE),
                u(align16(num_samples_to_load * SAMPLE_SIZE)),
            ));
            self.wave_samples[8] =
                self.wave_samples[8].wrapping_add(u(num_samples_to_load * SAMPLE_SIZE));
            return;
        }
        cmd.push(rsp::load_buffer(
            s.tuned_sample,
            u(DMEM_UNCOMPRESSED_NOTE),
            u(WAVE_SAMPLE_COUNT * SAMPLE_SIZE),
        ));
        if harmonic != 0 {
            pos = pos * NUM_SAMPLES_PER_WAVE_PERIOD[(harmonic >> 2) as usize & 3]
                / NUM_SAMPLES_PER_WAVE_PERIOD[(harmonic & 3) as usize];
        }
        pos = (pos as u32 % WAVE_SAMPLE_COUNT as u32) as i32;
        let avail = WAVE_SAMPLE_COUNT - pos;
        if num_samples_to_load > avail {
            let dups = (num_samples_to_load - avail + WAVE_SAMPLE_COUNT - 1) / WAVE_SAMPLE_COUNT;
            if dups != 0 {
                cmd.push(rsp::duplicate(
                    u(dups),
                    u(DMEM_UNCOMPRESSED_NOTE),
                    u(DMEM_UNCOMPRESSED_NOTE + WAVE_SAMPLE_COUNT * SAMPLE_SIZE),
                ));
            }
        }
        self.notes[note].synthesis_state.sample_pos_int = pos;
    }

    /// `AudioSynth_ApplyHaasEffect`
    fn apply_haas_effect(
        &mut self,
        cmd: &mut Vec<Acmd>,
        ss: usize,
        note: usize,
        size: i32,
        flags: u32,
        side: i32,
    ) {
        let s = self.sample_states[ss];
        let buffers = self.notes[note].synthesis_state.synthesis_buffers;
        let syn = &mut self.notes[note].synthesis_state;
        let (dmem_dest, delay, prev) = match side {
            HAAS_EFFECT_DELAY_LEFT => {
                let d = s.haas_effect_left_delay_size as i32;
                let p = syn.prev_haas_effect_left_delay_size as i32;
                syn.prev_haas_effect_right_delay_size = 0;
                syn.prev_haas_effect_left_delay_size = d as u8;
                (DMEM_LEFT_CH, d, p)
            }
            HAAS_EFFECT_DELAY_RIGHT => {
                let d = s.haas_effect_right_delay_size as i32;
                let p = syn.prev_haas_effect_right_delay_size as i32;
                syn.prev_haas_effect_right_delay_size = d as u8;
                syn.prev_haas_effect_left_delay_size = 0;
                (DMEM_RIGHT_CH, d, p)
            }
            _ => return,
        };
        let state = buffers + SYNTH_HAAS_DELAY_STATE;
        if flags != A_INIT {
            if delay != prev {
                let pitch = (((size << 15) / 2) - 1) / ((size + delay - prev - 2) / 2);
                cmd.push(rsp::set_buffer(
                    0,
                    u(DMEM_HAAS_TEMP),
                    u(DMEM_TEMP),
                    u(size + delay - prev),
                ));
                cmd.push(rsp::resample_zoh(pitch as u16 as u32, 0));
            } else {
                cmd.push(rsp::dmem_move(u(DMEM_HAAS_TEMP), u(DMEM_TEMP), u(size)));
            }
            if prev != 0 {
                cmd.push(rsp::load_buffer(state, u(DMEM_HAAS_TEMP), u(align16(prev))));
                cmd.push(rsp::dmem_move(
                    u(DMEM_TEMP),
                    u(DMEM_HAAS_TEMP + prev),
                    u(size + delay - prev),
                ));
            } else {
                cmd.push(rsp::dmem_move(
                    u(DMEM_TEMP),
                    u(DMEM_HAAS_TEMP),
                    u(size + delay),
                ));
            }
        } else {
            cmd.push(rsp::dmem_move(u(DMEM_HAAS_TEMP), u(DMEM_TEMP), u(size)));
            cmd.push(rsp::clear_buffer(u(DMEM_HAAS_TEMP), u(delay)));
            cmd.push(rsp::dmem_move(
                u(DMEM_TEMP),
                u(DMEM_HAAS_TEMP + delay),
                u(size),
            ));
        }
        if delay != 0 {
            cmd.push(rsp::save_buffer(
                u(DMEM_HAAS_TEMP + size),
                state,
                u(align16(delay)),
            ));
        }
        cmd.push(rsp::add_mixer(
            u(align64(size)),
            u(DMEM_HAAS_TEMP),
            u(dmem_dest),
            0x7FFF,
        ));
    }
}
