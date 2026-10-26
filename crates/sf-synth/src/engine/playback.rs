//! Notes (voices): allocation and stealing, release/decay hand-off, and the
//! per-tick update that feeds the synthesis pass (`playback.c`, original name
//! `channel.c`).

use super::lists::{ListValue, PoolId};
use super::{
    ADSR_STATE_DECAY, ADSR_STATE_DISABLED, ADSR_STATE_INITIAL, ADSR_STATE_RELEASE, AudioContext,
    MUTE_BEHAVIOR_3, MUTE_BEHAVIOR_STOP_NOTES, NoteSampleState, SOUND_OUTPUT_HEADSET,
    SOUND_OUTPUT_MONO, SOUND_OUTPUT_STEREO, WAVE_SAMPLE_COUNT,
};
use crate::tables;

/// `NoteSampleStateAttributes`
#[derive(Clone, Copy, Default)]
struct SampleAttrs {
    frequency: f32,
    velocity: f32,
    pan: u8,
    reverb_vol: u8,
    stereo: u8,
    gain: u8,
    filter: u32,
    comb_filter_size: u8,
    comb_filter_gain: u16,
}

// `Stereo` union bits (MSB first): unused:2, bit2:2, strongRight, strongLeft,
// stereoHeadsetEffects, usesHeadsetPanEffects.
fn stereo_bit2(s: u8) -> u8 {
    (s >> 4) & 3
}
fn stereo_strong_right(s: u8) -> bool {
    s & 0x08 != 0
}
fn stereo_strong_left(s: u8) -> bool {
    s & 0x04 != 0
}
fn stereo_headset_effects(s: u8) -> bool {
    s & 0x02 != 0
}
fn stereo_uses_headset_pan(s: u8) -> bool {
    s & 0x01 != 0
}

impl AudioContext {
    /// `Audio_InitSampleState`: fill `sampleStates[dest]` from a note.
    fn init_sample_state(&mut self, note: usize, dest: usize, attrs: &SampleAttrs) {
        let n = &self.notes[note];
        let headset_effects = n.playback_state.stereo_headset_effects;
        let src = n.sample_state;
        let mut vel = attrs.velocity;
        let mut pan = attrs.pan;
        let stereo = attrs.stereo;

        let d = &mut self.sample_states[dest];
        // Copy bitField0/bitField1, the sample pointer and harmonic indices.
        d.enabled = src.enabled;
        d.needs_init = src.needs_init;
        d.finished = src.finished;
        d.stereo_strong_right = src.stereo_strong_right;
        d.stereo_strong_left = src.stereo_strong_left;
        d.stereo_headset_effects = src.stereo_headset_effects;
        d.uses_headset_pan_effects = src.uses_headset_pan_effects;
        d.reverb_index = src.reverb_index;
        d.book_offset = src.book_offset;
        d.is_synthetic_wave = src.is_synthetic_wave;
        d.has_two_parts = src.has_two_parts;
        d.use_haas_effect = src.use_haas_effect;
        d.tuned_sample = src.tuned_sample;
        d.harmonic_index_cur_and_prev = src.harmonic_index_cur_and_prev;

        set_resampling_rate(d, attrs.frequency);

        pan &= 0x7F;
        d.stereo_strong_right = false;
        d.stereo_strong_left = false;
        d.stereo_headset_effects = stereo_headset_effects(stereo);
        d.uses_headset_pan_effects = stereo_uses_headset_pan(stereo);

        let (vol_left, vol_right);
        if headset_effects && self.sound_output_mode == SOUND_OUTPUT_HEADSET {
            let half = ((pan >> 1) as usize).min(0x3F);
            d.haas_effect_right_delay_size = tables::HAAS_DELAY_SIZES[half] as u8;
            d.haas_effect_left_delay_size = tables::HAAS_DELAY_SIZES[0x3F - half] as u8;
            d.use_haas_effect = true;
            vol_left = tables::HEADSET_PAN_VOLUME[pan as usize];
            vol_right = tables::HEADSET_PAN_VOLUME[0x7F - pan as usize];
        } else if headset_effects && self.sound_output_mode == SOUND_OUTPUT_STEREO {
            d.haas_effect_left_delay_size = 0;
            d.haas_effect_right_delay_size = 0;
            d.use_haas_effect = false;
            vol_left = tables::STEREO_PAN_VOLUME[pan as usize];
            vol_right = tables::STEREO_PAN_VOLUME[0x7F - pan as usize];
            let strong_left = pan < 0x20;
            let strong_right = pan > 0x60;
            d.stereo_strong_right = strong_right;
            d.stereo_strong_left = strong_left;
            match stereo_bit2(stereo) {
                1 => {
                    d.stereo_strong_right = stereo_strong_right(stereo);
                    d.stereo_strong_left = stereo_strong_left(stereo);
                }
                2 => {
                    d.stereo_strong_right = stereo_strong_right(stereo) | strong_right;
                    d.stereo_strong_left = stereo_strong_left(stereo) | strong_left;
                }
                3 => {
                    d.stereo_strong_right = stereo_strong_right(stereo) ^ strong_right;
                    d.stereo_strong_left = stereo_strong_left(stereo) ^ strong_left;
                }
                _ => {}
            }
        } else if self.sound_output_mode == SOUND_OUTPUT_MONO {
            d.stereo_headset_effects = false;
            d.uses_headset_pan_effects = false;
            vol_left = 0.707;
            vol_right = 0.707;
        } else {
            d.stereo_strong_right = stereo_strong_right(stereo);
            d.stereo_strong_left = stereo_strong_left(stereo);
            vol_left = tables::DEFAULT_PAN_VOLUME[pan as usize];
            vol_right = tables::DEFAULT_PAN_VOLUME[0x7F - pan as usize];
        }

        vel = vel.clamp(0.0, 1.0);
        d.target_vol_left = ((vel * vol_left) * (4096.0f32 - 0.001f32)) as i32 as u16;
        d.target_vol_right = ((vel * vol_right) * (4096.0f32 - 0.001f32)) as i32 as u16;
        d.gain = attrs.gain;
        d.filter = attrs.filter;
        d.comb_filter_size = attrs.comb_filter_size;
        d.comb_filter_gain = attrs.comb_filter_gain;
        d.reverb_vol = attrs.reverb_vol;
    }

    /// `Audio_NoteInit`
    fn note_init(&mut self, note: usize) {
        let parent = self.notes[note].playback_state.parent_layer.unwrap();
        let l = self.layers[parent];
        let env = if l.adsr.decay_index == 0 {
            self.channels[l.channel.unwrap()].adsr.envelope
        } else {
            l.adsr.envelope
        };
        let n = &mut self.notes[note];
        adsr_init(&mut n.playback_state.adsr, env);
        n.playback_state.unk_04 = 0;
        n.playback_state.adsr.state = ADSR_STATE_INITIAL;
        n.sample_state = NoteSampleState {
            enabled: true,
            needs_init: true,
            ..Default::default()
        };
    }

    /// `Audio_NoteDisable`
    fn note_disable(&mut self, note: usize) {
        let n = &mut self.notes[note];
        n.sample_state.needs_init = false;
        n.playback_state.priority = 0;
        n.sample_state.enabled = false;
        n.playback_state.unk_04 = 0;
        n.sample_state.finished = false;
        n.playback_state.parent_layer = None;
        n.playback_state.prev_parent_layer = None;
        n.playback_state.adsr.state = ADSR_STATE_DISABLED;
        n.playback_state.adsr.current = 0.0;
    }

    fn pool_of_note(&self, note: usize) -> PoolId {
        self.lists
            .pool_of(self.notes[note].list_item)
            .unwrap_or(self.note_free_lists)
    }

    /// `Audio_ProcessNotes`
    pub(crate) fn process_notes(&mut self) {
        for i in 0..self.num_notes {
            let dest = self.sample_state_offset + i;
            let ps = self.notes[i].playback_state;

            let mut go_out = false;
            if let Some(parent) = ps.parent_layer {
                let layer = self.layers[parent];
                let release;
                if layer.note != Some(i) && ps.unk_04 == 0 {
                    let a = &mut self.notes[i].playback_state;
                    a.adsr.release = true;
                    a.adsr.fade_out_vel = self.bp.ticks_per_update_inv;
                    a.priority = 1;
                    a.unk_04 = 2;
                    go_out = true;
                    release = false;
                } else if (!layer.enabled && ps.unk_04 == 0 && ps.priority >= 1)
                    || layer.channel.is_some_and(|c| {
                        let ch = &self.channels[c];
                        self.players[ch.seq_player].muted
                            && (ch.mute_behavior & MUTE_BEHAVIOR_STOP_NOTES) != 0
                    })
                {
                    release = true;
                } else {
                    go_out = true;
                    release = false;
                }
                if release {
                    self.seq_layer_note_release(Some(parent));
                    let item = self.notes[i].list_item;
                    self.lists.remove(item);
                    let pool = self.pool_of_note(i);
                    self.lists.push_front(self.lists.pools[pool].decaying, item);
                    let a = &mut self.notes[i].playback_state;
                    a.priority = 1;
                    a.unk_04 = 2;
                    go_out = true;
                }
            } else if ps.unk_04 == 0 && ps.priority >= 1 {
                continue;
            } else {
                go_out = true;
            }
            debug_assert!(go_out);

            if self.notes[i].playback_state.priority == 0 {
                continue;
            }
            let ps = self.notes[i].playback_state;
            let finished = self.notes[i].sample_state.finished;
            if ps.unk_04 >= 1 || finished {
                if ps.adsr.state == ADSR_STATE_DISABLED || finished {
                    if let Some(wanted) = ps.wanted_parent_layer {
                        self.note_disable(i);
                        if self.layers[wanted].channel.is_some() {
                            self.note_init_for_layer(i, wanted);
                            self.note_vibrato_init(i);
                            self.note_portamento_init(i);
                            let item = self.notes[i].list_item;
                            self.lists.remove(item);
                            let pool = self.pool_of_note(i);
                            self.lists.push_back(self.lists.pools[pool].active, item);
                            self.notes[i].playback_state.wanted_parent_layer = None;
                        } else {
                            self.note_disable(i);
                            let item = self.notes[i].list_item;
                            self.lists.remove(item);
                            let pool = self.pool_of_note(i);
                            self.lists.push_back(self.lists.pools[pool].disabled, item);
                            self.notes[i].playback_state.wanted_parent_layer = None;
                            continue;
                        }
                    } else {
                        if let Some(p) = ps.parent_layer {
                            self.layers[p].bit1 = true;
                        }
                        self.note_disable(i);
                        let item = self.notes[i].list_item;
                        self.lists.remove(item);
                        let pool = self.pool_of_note(i);
                        self.lists.push_back(self.lists.pools[pool].disabled, item);
                        continue;
                    }
                }
            } else if ps.adsr.state == ADSR_STATE_DISABLED {
                if let Some(p) = ps.parent_layer {
                    self.layers[p].bit1 = true;
                }
                self.note_disable(i);
                let item = self.notes[i].list_item;
                self.lists.remove(item);
                let pool = self.pool_of_note(i);
                self.lists.push_back(self.lists.pools[pool].disabled, item);
                continue;
            }

            let scale = self.adsr_update(i);
            self.note_vibrato_update(i);
            let ps = self.notes[i].playback_state;
            let mut sa = SampleAttrs::default();
            let book_offset;
            if ps.unk_04 == 1 || ps.unk_04 == 2 {
                let a = ps.attributes;
                sa.frequency = a.freq_scale;
                sa.velocity = a.velocity;
                sa.pan = a.pan;
                sa.reverb_vol = a.reverb;
                sa.stereo = a.stereo;
                sa.gain = a.gain;
                sa.filter = a.filter;
                sa.comb_filter_size = a.comb_filter_size;
                sa.comb_filter_gain = a.comb_filter_gain;
                book_offset = self.notes[i].sample_state.book_offset;
            } else {
                let Some(parent) = ps.parent_layer else {
                    continue;
                };
                let layer = self.layers[parent];
                let ch = self.channels[layer.channel.unwrap_or(0)];
                sa.frequency = layer.note_freq_scale;
                sa.velocity = layer.note_velocity;
                sa.pan = layer.note_pan;
                sa.stereo = if layer.stereo == 0 {
                    ch.stereo
                } else {
                    layer.stereo
                };
                sa.reverb_vol = ch.target_reverb_vol;
                sa.gain = ch.gain;
                sa.filter = ch.filter;
                sa.comb_filter_size = ch.comb_filter_size;
                sa.comb_filter_gain = ch.comb_filter_gain;
                book_offset = ch.book_offset & 7;
                if self.players[ch.seq_player].muted && (ch.mute_behavior & MUTE_BEHAVIOR_3) != 0 {
                    sa.frequency = 0.0;
                    sa.velocity = 0.0;
                }
            }
            sa.frequency *= ps.vibrato_freq_scale * ps.portamento_freq_scale;
            sa.frequency *= self.bp.resample_rate;
            sa.velocity *= scale;
            self.init_sample_state(i, dest, &sa);
            self.notes[i].sample_state.book_offset = book_offset & 3;
        }
    }

    /// `Audio_SeqLayerDecayRelease`
    fn seq_layer_decay_release(&mut self, layer: Option<usize>, target: u8) {
        let Some(layer) = layer else {
            return;
        };
        self.layers[layer].bit3 = false;
        let Some(note) = self.layers[layer].note else {
            return;
        };
        let ticks_inv = self.bp.ticks_per_update_inv;
        {
            let ps = &mut self.notes[note].playback_state;
            if ps.wanted_parent_layer == Some(layer) {
                ps.wanted_parent_layer = None;
            }
            if ps.parent_layer != Some(layer) {
                if ps.parent_layer.is_none()
                    && ps.wanted_parent_layer.is_none()
                    && ps.prev_parent_layer == Some(layer)
                    && target != ADSR_STATE_DECAY
                {
                    ps.adsr.fade_out_vel = ticks_inv;
                    ps.adsr.release = true;
                }
                return;
            }
        }

        if self.notes[note].playback_state.adsr.state != ADSR_STATE_DECAY {
            let l = self.layers[layer];
            {
                let a = &mut self.notes[note].playback_state.attributes;
                a.freq_scale = l.note_freq_scale;
                a.velocity = l.note_velocity;
                a.pan = l.note_pan;
            }
            if let Some(chi) = l.channel {
                let ch = self.channels[chi];
                let filter_buf = self.notes[note].playback_state.attributes.filter_buf;
                {
                    let a = &mut self.notes[note].playback_state.attributes;
                    a.reverb = ch.target_reverb_vol;
                    a.gain = ch.gain;
                    a.filter = ch.filter;
                }
                if ch.filter != 0 {
                    for k in 0..8 {
                        let v = self.ram.u16(ch.filter + 2 * k);
                        self.ram.set_u16(filter_buf + 2 * k, v);
                    }
                    self.notes[note].playback_state.attributes.filter = filter_buf;
                }
                let muted =
                    self.players[ch.seq_player].muted && (ch.mute_behavior & MUTE_BEHAVIOR_3) != 0;
                let n = &mut self.notes[note];
                n.playback_state.attributes.comb_filter_gain = ch.comb_filter_gain;
                n.playback_state.attributes.comb_filter_size = ch.comb_filter_size;
                if muted {
                    n.sample_state.finished = true;
                }
                n.playback_state.attributes.stereo =
                    if l.stereo == 0 { ch.stereo } else { l.stereo };
                n.playback_state.priority = ch.some_other_priority;
            } else {
                let n = &mut self.notes[note];
                n.playback_state.attributes.stereo = l.stereo;
                n.playback_state.priority = 1;
            }

            let decay_rate = if l.adsr.decay_index == 0 {
                l.channel
                    .map(|c| self.adsr_decay_table[self.channels[c].adsr.decay_index as usize])
            } else {
                Some(self.adsr_decay_table[l.adsr.decay_index as usize])
            };
            let sustain = l
                .channel
                .map(|c| self.channels[c].adsr.sustain)
                .unwrap_or(0);
            let ps = &mut self.notes[note].playback_state;
            ps.prev_parent_layer = ps.parent_layer;
            ps.parent_layer = None;
            if target == ADSR_STATE_RELEASE {
                ps.adsr.fade_out_vel = ticks_inv;
                ps.adsr.release = true;
                ps.unk_04 = 2;
            } else {
                ps.unk_04 = 1;
                ps.adsr.decay = true;
                ps.adsr.fade_out_vel = decay_rate.unwrap_or(0.0);
                ps.adsr.sustain = (sustain as i32 as f32 * ps.adsr.current) / 256.0f32;
            }
        }

        if target == ADSR_STATE_DECAY {
            let item = self.notes[note].list_item;
            self.lists.remove(item);
            let pool = self.pool_of_note(note);
            self.lists.push_front(self.lists.pools[pool].decaying, item);
        }
    }

    /// `Audio_SeqLayerNoteDecay`
    pub(crate) fn seq_layer_note_decay(&mut self, layer: Option<usize>) {
        self.seq_layer_decay_release(layer, ADSR_STATE_DECAY);
    }

    /// `Audio_SeqLayerNoteRelease`
    pub(crate) fn seq_layer_note_release(&mut self, layer: Option<usize>) {
        self.seq_layer_decay_release(layer, ADSR_STATE_RELEASE);
    }

    /// `Audio_BuildSyntheticWave`: returns the harmonic index.
    fn build_synthetic_wave(&mut self, note: usize, layer: usize, wave_id: i32) -> u8 {
        let wave_id = wave_id.max(128);
        let l = self.layers[layer];
        let mut freq_scale = l.freq_scale;
        if l.portamento.mode != 0 && 0.0 < l.portamento.extent {
            freq_scale *= l.portamento.extent + 1.0;
        }
        let (harmonic, ratio): (u8, f32) = if freq_scale < 0.99999 {
            (0, 1.0465)
        } else if freq_scale < 1.99999 {
            (1, 1.0465f32 / 2.0)
        } else if freq_scale < 3.99999 {
            (2, (1.0465f32 as f64 / 4.0 + 1.005e-3) as f32)
        } else {
            (3, (1.0465f32 as f64 / 8.0 - 2.5e-6) as f32)
        };
        self.layers[layer].freq_scale *= ratio;
        let base = self
            .wave_samples
            .get((wave_id - 128) as usize)
            .copied()
            .unwrap_or(self.wave_samples[0]);
        let n = &mut self.notes[note];
        n.playback_state.wave_id = wave_id as u8;
        n.playback_state.harmonic_index = harmonic;
        n.sample_state.tuned_sample = base + (harmonic as u32 * WAVE_SAMPLE_COUNT as u32) * 2;
        harmonic
    }

    /// `Audio_InitSyntheticWave`
    pub(crate) fn init_synthetic_wave(&mut self, note: usize, layer: usize) {
        let mut wave_id = self.layers[layer].inst_or_wave as i32;
        if wave_id == 0xFF {
            wave_id = self.layers[layer]
                .channel
                .map(|c| self.channels[c].inst_or_wave as i32)
                .unwrap_or(0);
        }
        let prev = self.notes[note].playback_state.harmonic_index;
        let cur = self.build_synthetic_wave(note, layer, wave_id);
        if cur != prev {
            self.notes[note].sample_state.harmonic_index_cur_and_prev = (cur << 2) + prev;
        }
    }

    pub(crate) fn init_note_free_list(&mut self) {
        let pool = self.note_free_lists;
        self.lists.reset_pool(pool);
        for i in 0..self.num_notes {
            let item = self.notes[i].list_item;
            self.lists.nodes[item].prev = None;
            self.lists.push_back(self.lists.pools[pool].disabled, item);
        }
    }

    /// `Audio_NotePoolClear`: return every note to the global free lists.
    pub(crate) fn note_pool_clear(&mut self, pool: PoolId) {
        let p = self.lists.pools[pool];
        let free = self.lists.pools[self.note_free_lists];
        for (src, dst) in [
            (p.disabled, free.disabled),
            (p.decaying, free.decaying),
            (p.releasing, free.releasing),
            (p.active, free.active),
        ] {
            if src == dst {
                continue;
            }
            while let Some(cur) = self.lists.first(src) {
                self.lists.remove(cur);
                self.lists.push_back(dst, cur);
            }
        }
    }

    /// `Audio_NotePoolFill`
    pub(crate) fn note_pool_fill(&mut self, pool: PoolId, count: i32) {
        self.note_pool_clear(pool);
        let p = self.lists.pools[pool];
        let free = self.lists.pools[self.note_free_lists];
        let mut j = 0;
        for (src, dst) in [
            (free.disabled, p.disabled),
            (free.decaying, p.decaying),
            (free.releasing, p.releasing),
            (free.active, p.active),
        ] {
            while j < count {
                match self.lists.pop_back(src) {
                    Some(ListValue::Note(n)) => {
                        let item = self.notes[n].list_item;
                        self.lists.push_back(dst, item);
                        j += 1;
                    }
                    _ => break,
                }
            }
            if j >= count {
                return;
            }
        }
    }

    /// `Audio_FindNodeWithPrioLessThan`
    fn find_node_with_prio_less_than(&self, list: usize, limit: i32) -> Option<usize> {
        let mut best: Option<usize> = None;
        for v in self.lists.iter(list) {
            let ListValue::Note(n) = v else {
                continue;
            };
            match best {
                None => best = Some(n),
                Some(b) => {
                    if self.notes[b].playback_state.priority
                        >= self.notes[n].playback_state.priority
                    {
                        best = Some(n);
                    }
                }
            }
        }
        let best = best?;
        if limit <= self.notes[best].playback_state.priority as i32 {
            return None;
        }
        Some(best)
    }

    /// `Audio_NoteInitForLayer`
    pub(crate) fn note_init_for_layer(&mut self, note: usize, layer: usize) {
        let ch = self.layers[layer].channel.unwrap_or(0);
        {
            let ps = &mut self.notes[note].playback_state;
            ps.prev_parent_layer = None;
            ps.parent_layer = Some(layer);
            ps.priority = self.channels[ch].note_priority;
        }
        {
            let l = &mut self.layers[layer];
            l.note_properties_need_init = true;
            l.bit3 = true;
            l.note = Some(note);
            l.note_velocity = 0.0;
        }
        self.note_init(note);
        let mut inst_id = self.layers[layer].inst_or_wave as i32;
        if inst_id == 0xFF {
            inst_id = self.channels[ch].inst_or_wave as i32;
        }
        self.notes[note].sample_state.tuned_sample = self.layers[layer].tuned_sample;
        let synthetic = (0x80..0xC0).contains(&inst_id);
        self.notes[note].sample_state.is_synthetic_wave = synthetic;
        if synthetic {
            self.build_synthetic_wave(note, layer, inst_id);
        }
        let c = self.channels[ch];
        let n = &mut self.notes[note];
        n.playback_state.font_id = c.font_id;
        n.playback_state.stereo_headset_effects = c.stereo_headset_effects;
        n.sample_state.reverb_index = c.reverb_index & 3;
    }

    /// `func_800E82C0` (`__Nas_InterTrack`)
    fn note_release_and_hand_over(&mut self, note: usize, layer: usize) {
        let parent = self.notes[note].playback_state.parent_layer;
        self.seq_layer_note_release(parent);
        self.notes[note].playback_state.wanted_parent_layer = Some(layer);
    }

    /// `Audio_NoteReleaseAndTakeOwnership`
    fn note_release_and_take_ownership(&mut self, note: usize, layer: usize) {
        let prio = self.layers[layer]
            .channel
            .map(|c| self.channels[c].note_priority)
            .unwrap_or(0);
        let ticks_inv = self.bp.ticks_per_update_inv;
        let ps = &mut self.notes[note].playback_state;
        ps.wanted_parent_layer = Some(layer);
        ps.priority = prio;
        ps.adsr.fade_out_vel = ticks_inv;
        ps.adsr.release = true;
    }

    fn alloc_note_from_disabled(&mut self, pool: PoolId, layer: usize) -> Option<usize> {
        let Some(ListValue::Note(n)) = self.lists.pop_back(self.lists.pools[pool].disabled) else {
            return None;
        };
        self.note_init_for_layer(n, layer);
        let item = self.notes[n].list_item;
        self.lists.push_front(self.lists.pools[pool].active, item);
        Some(n)
    }

    fn alloc_note_from_decaying(&mut self, pool: PoolId, layer: usize) -> Option<usize> {
        let Some(ListValue::Note(n)) = self.lists.pop_back(self.lists.pools[pool].decaying) else {
            return None;
        };
        self.note_release_and_take_ownership(n, layer);
        let item = self.notes[n].list_item;
        self.lists.push_back(self.lists.pools[pool].releasing, item);
        Some(n)
    }

    fn alloc_note_from_active(&mut self, pool: PoolId, layer: usize) -> Option<usize> {
        let prio = self.layers[layer]
            .channel
            .map(|c| self.channels[c].note_priority)
            .unwrap_or(0) as i32;
        let p = self.lists.pools[pool];
        let r_note = self.find_node_with_prio_less_than(p.releasing, prio);
        let r_priority = r_note
            .map(|n| self.notes[n].playback_state.priority as i32)
            .unwrap_or(0x10);
        let a_note = self.find_node_with_prio_less_than(p.active, prio);
        let a_priority = a_note
            .map(|n| self.notes[n].playback_state.priority as i32)
            .unwrap_or(0x10);
        if r_note.is_none() && a_note.is_none() {
            return None;
        }
        if a_priority < r_priority {
            let a = a_note.unwrap();
            let item = self.notes[a].list_item;
            self.lists.remove(item);
            self.note_release_and_hand_over(a, layer);
            self.lists.push_back(p.releasing, item);
            self.notes[a].playback_state.priority = prio as u8;
            return Some(a);
        }
        let r = r_note.unwrap();
        self.notes[r].playback_state.wanted_parent_layer = Some(layer);
        self.notes[r].playback_state.priority = prio as u8;
        Some(r)
    }

    /// `Audio_AllocNote`
    pub(crate) fn alloc_note(&mut self, layer: usize) -> Option<usize> {
        let ch = self.layers[layer].channel.unwrap_or(0);
        let policy = self.channels[ch].note_alloc_policy;
        let chp = self.channels[ch].note_pool;
        let plp = self.players[self.channels[ch].seq_player].note_pool;
        let free = self.note_free_lists;

        if policy & 1 != 0
            && let Some(n) = self.layers[layer].note
        {
            let ps = self.notes[n].playback_state;
            if ps.prev_parent_layer == Some(layer) && ps.wanted_parent_layer.is_none() {
                self.note_release_and_take_ownership(n, layer);
                let item = self.notes[n].list_item;
                self.lists.remove(item);
                let pool = self.pool_of_note(n);
                self.lists.push_back(self.lists.pools[pool].releasing, item);
                return Some(n);
            }
        }

        type Step = fn(&mut AudioContext, PoolId, usize) -> Option<usize>;
        let d: Step = AudioContext::alloc_note_from_disabled;
        let y: Step = AudioContext::alloc_note_from_decaying;
        let a: Step = AudioContext::alloc_note_from_active;
        let order: &[(Step, PoolId)] = if policy & 2 != 0 {
            &[(d, chp), (y, chp), (a, chp)]
        } else if policy & 4 != 0 {
            &[(d, chp), (d, plp), (y, chp), (y, plp), (a, chp), (a, plp)]
        } else if policy & 8 != 0 {
            &[(d, free), (y, free), (a, free)]
        } else {
            &[
                (d, chp),
                (d, plp),
                (d, free),
                (y, chp),
                (y, plp),
                (y, free),
                (a, chp),
                (a, plp),
                (a, free),
            ]
        };
        for &(step, pool) in order {
            if let Some(n) = step(self, pool, layer) {
                return Some(n);
            }
        }
        self.layers[layer].bit3 = true;
        None
    }

    /// `Audio_NoteInitAll`
    pub(crate) fn note_init_all(&mut self) {
        for n in self.notes.iter_mut() {
            n.sample_state = NoteSampleState::default();
            let ps = &mut n.playback_state;
            ps.priority = 0;
            ps.unk_04 = 0;
            ps.parent_layer = None;
            ps.wanted_parent_layer = None;
            ps.prev_parent_layer = None;
            ps.wave_id = 0;
            ps.attributes.velocity = 0.0;
            ps.adsr.hang = false;
            ps.adsr.decay = false;
            ps.adsr.release = false;
            ps.adsr.state = 0;
            ps.vibrato_state.active = false;
            ps.portamento.cur = 0;
            ps.portamento.speed = 0;
            ps.stereo_headset_effects = false;
            n.start_sample_pos = 0;
        }
    }
}

/// `Audio_NoteSetResamplingRate`
fn set_resampling_rate(s: &mut NoteSampleState, input: f32) {
    let rate = if input < 2.0 {
        s.has_two_parts = false;
        input.min(1.99998)
    } else {
        s.has_two_parts = true;
        if input > 3.99996 {
            1.99998
        } else {
            input * 0.5
        }
    };
    s.resampling_rate_fixed_point = (rate * 32768.0f32) as i32 as u16;
}

/// `Audio_AdsrInit`
pub(crate) fn adsr_init(adsr: &mut super::AdsrState, envelope: u32) {
    adsr.hang = false;
    adsr.decay = false;
    adsr.release = false;
    adsr.state = 0;
    adsr.delay = 0;
    adsr.envelope = envelope;
    adsr.sustain = 0.0;
    adsr.current = 0.0;
}
