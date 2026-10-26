//! Per-tick channel/player volume, pan and pitch propagation, plus envelopes
//! (ADSR), vibrato and portamento (`effects.c`).

use super::{
    ADSR_STATE_DECAY, ADSR_STATE_DISABLED, ADSR_STATE_FADE, ADSR_STATE_HANG, ADSR_STATE_INITIAL,
    ADSR_STATE_LOOP, ADSR_STATE_RELEASE, ADSR_STATE_START_LOOP, ADSR_STATE_SUSTAIN, AudioContext,
    MUTE_BEHAVIOR_SOFTEN,
};
use crate::tables;

const ADSR_DISABLE: i16 = 0;
const ADSR_HANG: i16 = -1;
const ADSR_GOTO: i16 = -2;
const ADSR_RESTART: i16 = -3;

impl AudioContext {
    /// `Audio_SequenceChannelProcessSound`
    fn sequence_channel_process_sound(
        &mut self,
        ch: usize,
        recalculate_volume: bool,
        apply_bend: bool,
    ) {
        let p = self.players[self.channels[ch].seq_player];
        let c = &mut self.channels[ch];
        if c.change_volume || recalculate_volume {
            let mut v = c.volume * c.volume_scale * p.applied_fade_volume;
            if p.muted && (c.mute_behavior & MUTE_BEHAVIOR_SOFTEN) != 0 {
                v *= p.mute_volume_scale;
            }
            c.applied_volume = v * v;
        }
        if c.change_pan {
            c.pan = c.new_pan as i32 * c.pan_channel_weight as i32;
        }
        let mut chan_freq_scale = c.freq_scale;
        if apply_bend {
            chan_freq_scale *= p.bend;
            c.change_freq_scale = true;
        }
        let c = *c;
        for li in c.layers.into_iter().flatten() {
            let l = &mut self.layers[li];
            if !l.enabled || l.note.is_none() {
                continue;
            }
            let pan = ((c.pan + l.pan as i32 * (0x80 - c.pan_channel_weight as i32)) >> 7) as u8;
            if l.note_properties_need_init {
                l.note_freq_scale = l.freq_scale * chan_freq_scale;
                l.note_velocity = l.velocity_square2 * c.applied_volume;
                l.note_pan = pan;
                l.note_properties_need_init = false;
            } else {
                if c.change_freq_scale {
                    l.note_freq_scale = l.freq_scale * chan_freq_scale;
                }
                if c.change_volume || recalculate_volume {
                    l.note_velocity = l.velocity_square2 * c.applied_volume;
                }
                if c.change_pan {
                    l.note_pan = pan;
                }
            }
        }
        let c = &mut self.channels[ch];
        c.change_freq_scale = false;
        c.change_volume = false;
        c.change_pan = false;
    }

    /// `Audio_SequencePlayerProcessSound`
    pub(crate) fn sequence_player_process_sound(&mut self, player: usize) {
        {
            let p = &mut self.players[player];
            if p.fade_timer != 0 {
                p.fade_volume += p.fade_velocity;
                p.recalculate_volume = true;
                p.fade_volume = p.fade_volume.clamp(0.0, 1.0);
                p.fade_timer -= 1;
                if p.fade_timer == 0 && p.state == 2 {
                    self.sequence_player_disable(player);
                    return;
                }
            }
            if p.recalculate_volume {
                p.applied_fade_volume = p.fade_volume * p.fade_volume_scale;
            }
        }
        let p = self.players[player];
        for i in 0..16 {
            let ch = p.channels[i];
            if self.channels[ch].enabled {
                self.sequence_channel_process_sound(ch, p.recalculate_volume, p.apply_bend);
            }
        }
        self.players[player].recalculate_volume = false;
    }

    /// `Audio_NoteVibratoUpdate`
    pub(crate) fn note_vibrato_update(&mut self, note: usize) {
        if self.notes[note].playback_state.portamento.mode != 0 {
            let p = &mut self.notes[note].playback_state.portamento;
            p.cur = p.cur.wrapping_add(p.speed);
            let mut lo = ((p.cur >> 8) & 0xFF) as usize;
            if lo >= 127 {
                lo = 127;
                p.mode = 0;
            }
            let f = 1.0f32 + p.extent * (tables::BEND_PITCH_ONE_OCTAVE[lo + 128] - 1.0f32);
            self.notes[note].playback_state.portamento_freq_scale = f;
        }
        if self.notes[note].playback_state.vibrato_state.active {
            let f = self.vibrato_freq_scale(note);
            self.notes[note].playback_state.vibrato_freq_scale = f;
        }
    }

    /// `Audio_GetVibratoFreqScale`
    fn vibrato_freq_scale(&mut self, note: usize) -> f32 {
        let mut vib = self.notes[note].playback_state.vibrato_state;
        if vib.delay != 0 {
            vib.delay -= 1;
            self.notes[note].playback_state.vibrato_state = vib;
            return 1.0;
        }
        if let Some(ch) = vib.channel {
            let c = self.channels[ch];
            if vib.depth_change_timer != 0 {
                if vib.depth_change_timer == 1 {
                    vib.depth = c.vibrato_depth_target as i32 as f32;
                } else {
                    vib.depth += (c.vibrato_depth_target as i32 as f32 - vib.depth)
                        / vib.depth_change_timer as i32 as f32;
                }
                vib.depth_change_timer -= 1;
            } else if c.vibrato_depth_target as i32 != vib.depth as i32 {
                vib.depth_change_timer = c.vibrato_depth_change_delay;
                if vib.depth_change_timer == 0 {
                    vib.depth = c.vibrato_depth_target as i32 as f32;
                }
            }
            if vib.rate_change_timer != 0 {
                if vib.rate_change_timer == 1 {
                    vib.rate = c.vibrato_rate_target as i32 as f32;
                } else {
                    vib.rate += (c.vibrato_rate_target as i32 as f32 - vib.rate)
                        / vib.rate_change_timer as i32 as f32;
                }
                vib.rate_change_timer -= 1;
            } else if c.vibrato_rate_target as i32 != vib.rate as i32 {
                vib.rate_change_timer = c.vibrato_rate_change_delay;
                if vib.rate_change_timer == 0 {
                    vib.rate = c.vibrato_rate_target as i32 as f32;
                }
            }
        }
        if vib.depth == 0.0 {
            self.notes[note].playback_state.vibrato_state = vib;
            return 1.0;
        }
        // Audio_GetVibratoPitchChange
        vib.time = vib.time.wrapping_add(vib.rate as i32 as u32);
        let index = (vib.time >> 10) & 0x3F;
        let pitch_change = self.ram.s16(vib.curve + 2 * index) as f32 + 32768.0f32;
        self.notes[note].playback_state.vibrato_state = vib;

        let temp = vib.depth / 4096.0f32;
        let depth = temp + 1.0f32;
        let inv_depth = 1.0f32 / depth;
        1.0f32 / ((depth - inv_depth) * pitch_change / 65536.0f32 + inv_depth)
    }

    /// `Audio_NoteVibratoInit`
    pub(crate) fn note_vibrato_init(&mut self, note: usize) {
        let sine = self.wave_samples[2];
        let ch = self.notes[note]
            .playback_state
            .parent_layer
            .and_then(|l| self.layers[l].channel);
        let ps = &mut self.notes[note].playback_state;
        ps.vibrato_freq_scale = 1.0;
        let vib = &mut ps.vibrato_state;
        vib.active = true;
        vib.time = 0;
        vib.curve = sine;
        vib.channel = ch;
        let Some(ch) = ch else {
            return;
        };
        let c = self.channels[ch];
        vib.depth_change_timer = c.vibrato_depth_change_delay;
        vib.depth = if vib.depth_change_timer == 0 {
            c.vibrato_depth_target as i32 as f32
        } else {
            c.vibrato_depth_start as i32 as f32
        };
        vib.rate_change_timer = c.vibrato_rate_change_delay;
        vib.rate = if vib.rate_change_timer == 0 {
            c.vibrato_rate_target as i32 as f32
        } else {
            c.vibrato_rate_start as i32 as f32
        };
        vib.delay = c.vibrato_delay;
    }

    /// `Audio_NotePortamentoInit`
    pub(crate) fn note_portamento_init(&mut self, note: usize) {
        let parent = self.notes[note].playback_state.parent_layer;
        let ps = &mut self.notes[note].playback_state;
        ps.portamento_freq_scale = 1.0;
        if let Some(l) = parent {
            ps.portamento = self.layers[l].portamento;
        }
    }

    /// `Audio_AdsrUpdate`: advance a note's envelope one tick.
    pub(crate) fn adsr_update(&mut self, note: usize) -> f32 {
        let ticks_scaled = self.bp.ticks_per_update_scaled;
        let mut adsr = self.notes[note].playback_state.adsr;
        let env_delay = |ram: &crate::ram::Ram, env: u32, i: u8| ram.s16(env + 4 * i as u32);
        let env_arg = |ram: &crate::ram::Ram, env: u32, i: u8| ram.s16(env + 4 * i as u32 + 2);
        let state = adsr.state;

        let mut run_loop = false;
        match state {
            ADSR_STATE_DISABLED => return 0.0,
            ADSR_STATE_INITIAL => {
                if adsr.hang {
                    adsr.state = ADSR_STATE_HANG;
                } else {
                    adsr.env_index = 0;
                    adsr.state = ADSR_STATE_LOOP;
                    run_loop = true;
                }
            }
            ADSR_STATE_START_LOOP => {
                adsr.env_index = 0;
                adsr.state = ADSR_STATE_LOOP;
                run_loop = true;
            }
            ADSR_STATE_LOOP => run_loop = true,
            ADSR_STATE_FADE => {
                adsr.current += adsr.velocity;
                adsr.delay -= 1;
                if adsr.delay <= 0 {
                    adsr.state = ADSR_STATE_LOOP;
                }
            }
            ADSR_STATE_HANG => {}
            ADSR_STATE_DECAY | ADSR_STATE_RELEASE => {
                adsr.current -= adsr.fade_out_vel;
                if adsr.sustain != 0.0 && state == ADSR_STATE_DECAY {
                    if adsr.current < adsr.sustain {
                        adsr.current = adsr.sustain;
                        adsr.delay = 128;
                        adsr.state = ADSR_STATE_SUSTAIN;
                    }
                } else if adsr.current < 0.00001 {
                    adsr.current = 0.0;
                    adsr.state = ADSR_STATE_DISABLED;
                }
            }
            ADSR_STATE_SUSTAIN => {
                adsr.delay -= 1;
                if adsr.delay == 0 {
                    adsr.state = ADSR_STATE_RELEASE;
                }
            }
            _ => {}
        }

        if run_loop {
            // `retry:` — ADSR_GOTO jumps back here. A self-referencing goto
            // hangs the console; cap it instead.
            let mut guard = 0;
            loop {
                adsr.delay = env_delay(&self.ram, adsr.envelope, adsr.env_index);
                match adsr.delay {
                    ADSR_DISABLE => adsr.state = ADSR_STATE_DISABLED,
                    ADSR_HANG => adsr.state = ADSR_STATE_HANG,
                    ADSR_GOTO => {
                        adsr.env_index = env_arg(&self.ram, adsr.envelope, adsr.env_index) as u8;
                        guard += 1;
                        if guard < 32 {
                            continue;
                        }
                        adsr.state = ADSR_STATE_HANG;
                    }
                    ADSR_RESTART => adsr.state = ADSR_STATE_INITIAL,
                    _ => {
                        adsr.delay = (adsr.delay as f32 * ticks_scaled) as i32 as i16;
                        if adsr.delay == 0 {
                            adsr.delay = 1;
                        }
                        let t =
                            env_arg(&self.ram, adsr.envelope, adsr.env_index) as f32 / 32767.0f32;
                        adsr.target = t * t;
                        adsr.velocity = (adsr.target - adsr.current) / adsr.delay as f32;
                        adsr.state = ADSR_STATE_FADE;
                        adsr.env_index = adsr.env_index.wrapping_add(1);
                    }
                }
                break;
            }
            if adsr.state == ADSR_STATE_FADE {
                adsr.current += adsr.velocity;
                adsr.delay -= 1;
                if adsr.delay <= 0 {
                    adsr.state = ADSR_STATE_LOOP;
                }
            }
        }

        if adsr.decay {
            adsr.state = ADSR_STATE_DECAY;
            adsr.decay = false;
        }
        if adsr.release {
            adsr.state = ADSR_STATE_RELEASE;
            adsr.release = false;
        }
        self.notes[note].playback_state.adsr = adsr;
        adsr.current.clamp(0.0, 1.0)
    }
}
