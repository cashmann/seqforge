//! The sequence interpreter (`seqplayer.c`, original name `track.c`):
//! sequence, channel and layer scripts, one tick at a time.

use super::lists::ListValue;
use super::{
    AdsrSettings, AudioContext, CODEC_S16_INMEMORY, MEDIUM_RAM, MUTE_BEHAVIOR_4,
    MUTE_BEHAVIOR_SOFTEN, MUTE_BEHAVIOR_STOP_NOTES, MUTE_BEHAVIOR_STOP_SCRIPT, NoteEvent,
    SEQTICKS_PER_BEAT, SeqScriptState, SequenceChannel, SequenceLayer, SequencePlayer,
};
use crate::tables;

const PROCESS_SCRIPT_END: i32 = -1;

const PORTAMENTO_MODE_OFF: u8 = 0;
const PORTAMENTO_MODE_1: u8 = 1;
const PORTAMENTO_MODE_2: u8 = 2;
const PORTAMENTO_MODE_3: u8 = 3;
const PORTAMENTO_MODE_4: u8 = 4;
const PORTAMENTO_MODE_5: u8 = 5;

fn portamento_is_special(mode: u8) -> bool {
    mode & 0x80 != 0
}

fn portamento_mode(mode: u8) -> u8 {
    mode & !0x80
}

/// `sSeqInstructionArgsTable` for 0xB0..=0xFF: `abcUUUnn`, where n is the
/// argument count and a/b/c flag an s16 (vs u8) argument.
const fn args0() -> u8 {
    0
}
const fn args1(a: bool) -> u8 {
    (a as u8) << 7 | 1
}
const fn args2(a: bool, b: bool) -> u8 {
    (a as u8) << 7 | (b as u8) << 6 | 2
}
const fn args3(a: bool, b: bool, c: bool) -> u8 {
    (a as u8) << 7 | (b as u8) << 6 | (c as u8) << 5 | 3
}
const S16: bool = true;
const U8: bool = false;

static ARGS_TABLE: [u8; 0x50] = [
    args1(S16),        // B0 ldfilter
    args0(),           // B1 freefilter
    args1(S16),        // B2 ldseqtoptr
    args1(U8),         // B3 filter
    args0(),           // B4 ptrtodyntbl
    args0(),           // B5 dyntbltoptr
    args0(),           // B6 dyntblv
    args1(S16),        // B7 randtoptr
    args1(U8),         // B8 rand
    args1(U8),         // B9 randvel
    args1(U8),         // BA randgate
    args2(U8, S16),    // BB combfilter
    args1(S16),        // BC ptradd
    args2(S16, S16),   // BD randptr
    args0(),           // BE
    args0(),           // BF
    args0(),           // C0
    args1(U8),         // C1 instr
    args1(S16),        // C2 dyntbl
    args0(),           // C3 short
    args0(),           // C4 noshort
    args0(),           // C5 dyntbllookup
    args1(U8),         // C6 font
    args2(U8, S16),    // C7 stseq
    args1(U8),         // C8 sub
    args1(U8),         // C9 and
    args1(U8),         // CA mutebhv
    args1(S16),        // CB ldseq
    args1(U8),         // CC ldi
    args1(U8),         // CD stopchan
    args1(S16),        // CE ldptr
    args1(S16),        // CF stptrtoseq
    args1(U8),         // D0 effects
    args1(U8),         // D1 notealloc
    args1(U8),         // D2 sustain
    args1(U8),         // D3 bend
    args1(U8),         // D4 reverb
    args1(U8),         // D5
    args1(U8),         // D6
    args1(U8),         // D7 vibfreq
    args1(U8),         // D8 vibdepth
    args1(U8),         // D9 releaserate
    args1(S16),        // DA env
    args1(U8),         // DB transpose
    args1(U8),         // DC panweight
    args1(U8),         // DD pan
    args1(S16),        // DE freqscale
    args1(U8),         // DF vol
    args1(U8),         // E0 volexp
    args3(U8, U8, U8), // E1 vibfreqgrad
    args3(U8, U8, U8), // E2 vibdepthgrad
    args1(U8),         // E3 vibdelay
    args0(),           // E4 dyncall
    args1(U8),         // E5 reverbidx
    args1(U8),         // E6 samplebook
    args1(S16),        // E7 ldparams
    args3(U8, U8, U8), // E8 params
    args1(U8),         // E9 notepri
    args0(),           // EA stop
    args2(U8, U8),     // EB fontinstr
    args0(),           // EC vibreset
    args1(U8),         // ED gain
    args1(U8),         // EE bendfine
    args2(S16, U8),    // EF
    args0(),           // F0 freenotelist
    args1(U8),         // F1 allocnotelist
    args1(U8),         // F2 rbltz
    args1(U8),         // F3 rbeqz
    args1(U8),         // F4 rjump
    args1(S16),        // F5 bgez
    args0(),           // F6 break
    args0(),           // F7 loopend
    args1(U8),         // F8 loop
    args1(S16),        // F9 bltz
    args1(S16),        // FA beqz
    args1(S16),        // FB jump
    args1(S16),        // FC call
    args0(),           // FD delay
    args0(),           // FE delay1
    args0(),           // FF end
];

#[derive(Clone, Copy)]
pub(crate) enum Script {
    Player(usize),
    Channel(usize),
    Layer(usize),
}

impl AudioContext {
    fn state(&mut self, s: Script) -> &mut SeqScriptState {
        match s {
            Script::Player(i) => &mut self.players[i].script_state,
            Script::Channel(i) => &mut self.channels[i].script_state,
            Script::Layer(i) => &mut self.layers[i].script_state,
        }
    }

    fn read_u8(&mut self, s: Script) -> u8 {
        let st = self.state(s);
        let pc = st.pc;
        st.pc = pc.wrapping_add(1);
        self.ram.u8(pc)
    }

    fn read_s16(&mut self, s: Script) -> i16 {
        let hi = self.read_u8(s) as u16;
        let lo = self.read_u8(s) as u16;
        (hi << 8 | lo) as i16
    }

    fn read_compressed_u16(&mut self, s: Script) -> u16 {
        let mut ret = self.read_u8(s) as u16;
        if ret & 0x80 != 0 {
            ret = (ret << 8) & 0x7F00;
            ret |= self.read_u8(s) as u16;
        }
        ret
    }

    /// `AudioSeq_GetScriptControlFlowArgument`
    fn control_flow_argument(&mut self, s: Script, cmd: u8) -> u16 {
        let high = ARGS_TABLE[(cmd - 0xB0) as usize];
        if high & 3 == 1 {
            if high & 0x80 == 0 {
                self.read_u8(s) as u16
            } else {
                self.read_s16(s) as u16
            }
        } else {
            0
        }
    }

    /// `AudioSeq_HandleScriptFlowControl`: returns ticks to wait, 0 to keep
    /// going, or `PROCESS_SCRIPT_END`.
    fn flow_control(&mut self, s: Script, seq_data: u32, cmd: u8, arg: u32) -> i32 {
        match cmd {
            0xFF => {
                let st = self.state(s);
                if st.depth == 0 {
                    return PROCESS_SCRIPT_END;
                }
                st.depth -= 1;
                st.pc = st.stack[st.depth as usize & 3];
            }
            0xFD => return self.read_compressed_u16(s) as i32,
            0xFE => return 1,
            0xFC => {
                let st = self.state(s);
                st.stack[st.depth as usize & 3] = st.pc;
                st.depth = st.depth.wrapping_add(1);
                st.pc = seq_data.wrapping_add(arg as u16 as u32);
            }
            0xF8 => {
                let st = self.state(s);
                st.rem_loop_iters[st.depth as usize & 3] = arg as u8;
                st.stack[st.depth as usize & 3] = st.pc;
                st.depth = st.depth.wrapping_add(1);
            }
            0xF7 => {
                let st = self.state(s);
                let d = st.depth.wrapping_sub(1) as usize & 3;
                st.rem_loop_iters[d] = st.rem_loop_iters[d].wrapping_sub(1);
                if st.rem_loop_iters[d] != 0 {
                    st.pc = st.stack[d];
                } else {
                    st.depth = st.depth.wrapping_sub(1);
                }
            }
            0xF6 => {
                let st = self.state(s);
                st.depth = st.depth.wrapping_sub(1);
            }
            0xF5 | 0xF9 | 0xFA | 0xFB => {
                let st = self.state(s);
                if cmd == 0xFA && st.value != 0 {
                    return 0;
                }
                if cmd == 0xF9 && st.value >= 0 {
                    return 0;
                }
                if cmd == 0xF5 && st.value < 0 {
                    return 0;
                }
                st.pc = seq_data.wrapping_add(arg as u16 as u32);
            }
            0xF2..=0xF4 => {
                let st = self.state(s);
                if cmd == 0xF3 && st.value != 0 {
                    return 0;
                }
                if cmd == 0xF2 && st.value >= 0 {
                    return 0;
                }
                st.pc = st.pc.wrapping_add((arg & 0xFF) as u8 as i8 as i32 as u32);
            }
            _ => {}
        }
        0
    }

    /// `AudioSeq_InitSequenceChannel`
    fn init_sequence_channel(&mut self, ch: usize) {
        let default_envelope = self.default_envelope;
        let c = &mut self.channels[ch];
        c.enabled = false;
        c.finished = false;
        c.stop_script = false;
        c.muted = false;
        c.has_instrument = false;
        c.stereo_headset_effects = false;
        c.transposition = 0;
        c.large_notes = false;
        c.book_offset = 0;
        c.stereo = 0;
        c.change_freq_scale = true;
        c.change_volume = true;
        c.change_pan = true;
        c.script_state.depth = 0;
        c.new_pan = 0x40;
        c.pan_channel_weight = 0x80;
        c.velocity_random_variance = 0;
        c.gate_time_random_variance = 0;
        c.reverb_index = 0;
        c.target_reverb_vol = 0;
        c.gain = 0;
        c.note_priority = 3;
        c.some_other_priority = 1;
        c.delay = 0;
        c.adsr = AdsrSettings {
            envelope: default_envelope,
            decay_index: 0xF0,
            sustain: 0,
        };
        c.vibrato_rate_target = 0x800;
        c.vibrato_rate_start = 0x800;
        c.vibrato_depth_target = 0;
        c.vibrato_depth_start = 0;
        c.vibrato_rate_change_delay = 0;
        c.vibrato_depth_change_delay = 0;
        c.vibrato_delay = 0;
        c.filter = 0;
        c.comb_filter_gain = 0;
        c.comb_filter_size = 0;
        c.volume = 1.0;
        c.volume_scale = 1.0;
        c.freq_scale = 1.0;
        c.seq_script_io = [-1; 8];
        let pool = c.note_pool;
        self.lists.reset_pool(pool);
    }

    /// `AudioSeq_SeqChannelSetLayer`
    fn seq_channel_set_layer(&mut self, ch: usize, idx: usize) -> i32 {
        let layer = match self.channels[ch].layers[idx] {
            None => match self.lists.pop_back(self.layer_free_list) {
                Some(ListValue::Layer(l)) => {
                    self.channels[ch].layers[idx] = Some(l);
                    l
                }
                _ => {
                    self.channels[ch].layers[idx] = None;
                    return -1;
                }
            },
            Some(l) => {
                self.seq_layer_note_decay(Some(l));
                l
            }
        };
        let adsr = self.channels[ch].adsr;
        let l = &mut self.layers[layer];
        l.channel = Some(ch);
        l.adsr = adsr;
        l.adsr.decay_index = 0;
        l.enabled = true;
        l.finished = false;
        l.muted = false;
        l.continuous_notes = false;
        l.bit3 = false;
        l.ignore_drum_pan = false;
        l.bit1 = false;
        l.note_properties_need_init = false;
        l.stereo = 0;
        l.portamento.mode = PORTAMENTO_MODE_OFF;
        l.script_state.depth = 0;
        l.gate_time = 0x80;
        l.pan = 0x40;
        l.transposition = 0;
        l.delay = 0;
        l.gate_delay = 0;
        l.delay2 = 0;
        l.note = None;
        l.instrument = 0;
        l.freq_scale = 1.0;
        l.bend = 1.0;
        l.velocity_square2 = 0.0;
        l.inst_or_wave = 0xFF;
        0
    }

    /// `AudioSeq_SeqLayerDisable`
    fn seq_layer_disable(&mut self, layer: usize) {
        let finished = self.layers[layer]
            .channel
            .map(|c| self.players[self.channels[c].seq_player].finished)
            .unwrap_or(false);
        if finished {
            self.seq_layer_note_release(Some(layer));
        } else {
            self.seq_layer_note_decay(Some(layer));
        }
        self.layers[layer].enabled = false;
        self.layers[layer].finished = true;
    }

    /// `AudioSeq_SeqLayerFree`
    fn seq_layer_free(&mut self, ch: usize, idx: usize) {
        if let Some(l) = self.channels[ch].layers[idx] {
            let item = self.layers[l].list_item;
            self.lists.push_back(self.layer_free_list, item);
            self.seq_layer_disable(l);
            self.channels[ch].layers[idx] = None;
        }
    }

    /// `AudioSeq_SequenceChannelDisable`
    pub(crate) fn sequence_channel_disable(&mut self, ch: usize) {
        for i in 0..4 {
            self.seq_layer_free(ch, i);
        }
        let pool = self.channels[ch].note_pool;
        self.note_pool_clear(pool);
        self.channels[ch].enabled = false;
        self.channels[ch].finished = true;
    }

    /// `AudioSeq_SequencePlayerSetupChannels`
    fn sequence_player_setup_channels(&mut self, player: usize, mut bits: u16) {
        let p = self.players[player];
        for i in 0..16 {
            if bits & 1 != 0 {
                let c = &mut self.channels[p.channels[i]];
                c.font_id = p.default_font;
                c.mute_behavior = p.mute_behavior;
                c.note_alloc_policy = p.note_alloc_policy;
            }
            bits >>= 1;
        }
    }

    /// `AudioSeq_SequenceChannelEnable`
    fn sequence_channel_enable(&mut self, player: usize, idx: usize, script: u32) {
        let ch = self.players[player].channels[idx];
        let c = &mut self.channels[ch];
        c.enabled = true;
        c.finished = false;
        c.script_state.depth = 0;
        c.script_state.pc = script;
        c.delay = 0;
        for i in 0..4 {
            if self.channels[ch].layers[i].is_some() {
                self.seq_layer_free(ch, i);
            }
        }
    }

    /// `AudioSeq_SequencePlayerDisable`
    pub(crate) fn sequence_player_disable(&mut self, player: usize) {
        for i in 0..16 {
            let ch = self.players[player].channels[i];
            self.sequence_channel_disable(ch);
        }
        let pool = self.players[player].note_pool;
        self.note_pool_clear(pool);
        let p = &mut self.players[player];
        if !p.enabled {
            return;
        }
        p.enabled = false;
        p.finished = true;
    }

    /// `AudioSeq_SeqLayerProcessScript`
    fn seq_layer_process_script(&mut self, layer: usize) {
        let l = &mut self.layers[layer];
        if !l.enabled {
            return;
        }
        if l.delay > 1 {
            l.delay -= 1;
            if !l.muted && l.delay <= l.gate_delay {
                self.seq_layer_note_decay(Some(layer));
                self.layers[layer].muted = true;
            }
            return;
        }

        self.seq_layer_step1(layer);
        let mut cmd = self.seq_layer_step2(layer);
        if cmd == PROCESS_SCRIPT_END {
            return;
        }
        cmd = self.seq_layer_step3(layer, cmd);
        if cmd != PROCESS_SCRIPT_END {
            cmd = self.seq_layer_step4(layer, cmd);
        }
        if cmd != PROCESS_SCRIPT_END {
            self.seq_layer_step5(layer, cmd);
        }
        let l = self.layers[layer];
        if l.muted && (l.note.is_some() || l.continuous_notes) {
            self.seq_layer_note_decay(Some(layer));
        }
    }

    /// `AudioSeq_SeqLayerProcessScriptStep1` (`__Stop_Note`)
    fn seq_layer_step1(&mut self, layer: usize) {
        let l = self.layers[layer];
        if !l.continuous_notes {
            self.seq_layer_note_decay(Some(layer));
        } else if let Some(n) = l.note
            && self.notes[n].playback_state.wanted_parent_layer == Some(layer)
        {
            self.seq_layer_note_decay(Some(layer));
        }
        let l = &mut self.layers[layer];
        let mode = portamento_mode(l.portamento.mode);
        if mode == PORTAMENTO_MODE_1 || mode == PORTAMENTO_MODE_2 {
            l.portamento.mode = PORTAMENTO_MODE_OFF;
        }
        l.note_properties_need_init = true;
    }

    /// `AudioSeq_SeqLayerProcessScriptStep5` (`__SetChannel`)
    fn seq_layer_step5(&mut self, layer: usize, same_tuned_sample: i32) -> i32 {
        let l = self.layers[layer];
        if !l.muted && l.tuned_sample != 0 {
            let sample = self.tuned_sample_sample(l.tuned_sample);
            if self.sample_codec(sample) == CODEC_S16_INMEMORY
                && self.sample_medium(sample) != MEDIUM_RAM
            {
                self.layers[layer].muted = true;
                return PROCESS_SCRIPT_END;
            }
        }
        if l.continuous_notes && l.bit1 {
            return 0;
        }
        // The note, if this layer still owns it.
        let owned = |ctx: &AudioContext, n: Option<usize>| {
            n.filter(|&n| ctx.notes[n].playback_state.parent_layer == Some(layer))
        };
        let reuse = l.continuous_notes && l.bit3 && same_tuned_sample == 1;
        if let Some(n) = owned(self, l.note).filter(|_| reuse) {
            if l.tuned_sample == 0 {
                self.init_synthetic_wave(n, layer);
            }
        } else {
            if same_tuned_sample == 0 {
                self.seq_layer_note_decay(Some(layer));
            }
            let note = self.alloc_note(layer);
            self.layers[layer].note = note;
            if let Some(n) = owned(self, note) {
                self.note_vibrato_init(n);
            }
        }
        if let Some(n) = owned(self, self.layers[layer].note) {
            self.note_portamento_init(n);
            self.report_note(layer);
        }
        0
    }

    fn report_note(&mut self, layer: usize) {
        if !self.record_events {
            return;
        }
        let l = self.layers[layer];
        let Some(ch) = l.channel else {
            return;
        };
        let player = self.channels[ch].seq_player;
        self.events.push(NoteEvent {
            player,
            channel: ch % 16,
            layer: self.channels[ch]
                .layers
                .iter()
                .position(|&x| x == Some(layer))
                .unwrap_or(0),
            tick: self.players[player].ticks_elapsed,
            semitone: l.semitone,
            velocity: l.velocity_square2,
            duration: l.delay.max(0) as u16,
        });
    }

    /// `AudioSeq_SeqLayerProcessScriptStep2` (`__Command_Seq`)
    fn seq_layer_step2(&mut self, layer: usize) -> i32 {
        let s = Script::Layer(layer);
        let ch = self.layers[layer].channel.unwrap_or(0);
        let player = self.channels[ch].seq_player;
        let seq_data = self.players[player].seq_data;
        loop {
            let cmd = self.read_u8(s);
            if cmd <= 0xC0 {
                return cmd as i32;
            }
            if cmd >= 0xF2 {
                let arg = self.control_flow_argument(s, cmd);
                if self.flow_control(s, seq_data, cmd, arg as u32) == 0 {
                    continue;
                }
                self.seq_layer_disable(layer);
                return PROCESS_SCRIPT_END;
            }
            match cmd {
                0xC1 | 0xCA => {
                    let v = self.read_u8(s);
                    if cmd == 0xC1 {
                        self.layers[layer].velocity_square =
                            (v as f32 * v as f32) / (127.0f32 * 127.0f32);
                    } else {
                        self.layers[layer].pan = v;
                    }
                }
                0xC9 | 0xC2 => {
                    let v = self.read_u8(s);
                    if cmd == 0xC9 {
                        self.layers[layer].gate_time = v;
                    } else {
                        self.layers[layer].transposition = v as i16;
                    }
                }
                0xC4 | 0xC5 => {
                    self.layers[layer].continuous_notes = cmd == 0xC4;
                    self.layers[layer].bit1 = false;
                    self.seq_layer_note_decay(Some(layer));
                }
                0xC3 => {
                    let v = self.read_compressed_u16(s);
                    self.layers[layer].short_note_default_delay = v as i16;
                }
                0xC6 => {
                    let id = self.read_u8(s);
                    if id >= 0x7E {
                        let l = &mut self.layers[layer];
                        if id == 0x7E {
                            l.inst_or_wave = 1;
                        } else if id == 0x7F {
                            l.inst_or_wave = 0;
                        } else {
                            l.inst_or_wave = id;
                            l.instrument = 0;
                        }
                        if id == 0xFF {
                            l.adsr.decay_index = 0;
                        }
                    } else {
                        let mut adsr = self.layers[layer].adsr;
                        let (v, inst) = self.get_instrument(ch, id, &mut adsr);
                        let l = &mut self.layers[layer];
                        l.adsr = adsr;
                        l.instrument = inst;
                        l.inst_or_wave = if v == 0 { 0xFF } else { v };
                    }
                }
                0xC7 => {
                    let mode = self.read_u8(s);
                    self.layers[layer].portamento.mode = mode;
                    let mut note = self.read_u8(s);
                    note = note.wrapping_add(self.channels[ch].transposition as u8);
                    note = note.wrapping_add(self.layers[layer].transposition as u8);
                    note = note.wrapping_add(self.players[player].transposition as u8);
                    if note >= 0x80 {
                        note = 0;
                    }
                    self.layers[layer].portamento_target_note = note;
                    if portamento_is_special(mode) {
                        let t = self.read_u8(s);
                        self.layers[layer].portamento_time = t as u16;
                    } else {
                        let t = self.read_compressed_u16(s);
                        self.layers[layer].portamento_time = t;
                    }
                }
                0xC8 => self.layers[layer].portamento.mode = PORTAMENTO_MODE_OFF,
                0xCB | 0xCF => {
                    if cmd == 0xCB {
                        let off = self.read_s16(s) as u16;
                        self.layers[layer].adsr.envelope = seq_data.wrapping_add(off as u32);
                    }
                    let d = self.read_u8(s);
                    self.layers[layer].adsr.decay_index = d;
                }
                0xCC => self.layers[layer].ignore_drum_pan = true,
                0xCD => {
                    let v = self.read_u8(s);
                    self.layers[layer].stereo = v;
                }
                0xCE => {
                    let v = self.read_u8(s);
                    self.layers[layer].bend =
                        tables::BEND_PITCH_TWO_SEMITONES[v.wrapping_add(0x80) as usize];
                }
                _ => match cmd & 0xF0 {
                    0xD0 => {
                        let table = self.players[player].short_note_velocity_table;
                        let v = self.ram.u8(table + (cmd & 0xF) as u32) as f32;
                        self.layers[layer].velocity_square = (v * v) / (127.0f32 * 127.0f32);
                    }
                    0xE0 => {
                        let table = self.players[player].short_note_gate_time_table;
                        self.layers[layer].gate_time = self.ram.u8(table + (cmd & 0xF) as u32);
                    }
                    _ => {}
                },
            }
        }
    }

    /// `AudioSeq_SeqLayerProcessScriptStep4` (`__SetVoice`); returns
    /// `sameTunedSample` or `PROCESS_SCRIPT_END`.
    fn seq_layer_step4(&mut self, layer: usize, cmd: i32) -> i32 {
        let mut same_tuned_sample = 1;
        let mut semitone = cmd as u8;
        let ch = self.layers[layer].channel.unwrap_or(0);
        let player = self.channels[ch].seq_player;
        let mut inst_or_wave = self.layers[layer].inst_or_wave as i32;

        if inst_or_wave == 0xFF {
            if !self.channels[ch].has_instrument {
                return PROCESS_SCRIPT_END;
            }
            inst_or_wave = self.channels[ch].inst_or_wave as i32;
        }

        let c = self.channels[ch];
        match inst_or_wave {
            0 => {
                // Drums
                semitone = semitone
                    .wrapping_add(c.transposition as u8)
                    .wrapping_add(self.layers[layer].transposition as u8);
                self.layers[layer].semitone = semitone;
                let drum = self.get_drum(c.font_id, semitone);
                if drum == 0 {
                    let l = &mut self.layers[layer];
                    l.muted = true;
                    l.delay2 = l.delay;
                    return PROCESS_SCRIPT_END;
                }
                let ts = drum + 4;
                let tuning = self.tuned_sample_tuning(ts);
                let env = self.ram.u32(drum + 0xC);
                let decay = self.ram.u8(drum);
                let pan = self.ram.u8(drum + 1);
                let l = &mut self.layers[layer];
                l.adsr.envelope = env;
                l.adsr.decay_index = decay;
                if !l.ignore_drum_pan {
                    l.pan = pan;
                }
                l.tuned_sample = ts;
                l.freq_scale = tuning;
            }
            1 => {
                // Sound effects
                self.layers[layer].semitone = semitone;
                let sfx_id = ((self.layers[layer].transposition as i32) << 6) + semitone as i32;
                let se = self.get_sound_effect(c.font_id, sfx_id as u16);
                if se == 0 {
                    let l = &mut self.layers[layer];
                    l.muted = true;
                    l.delay2 = l.delay.wrapping_add(1);
                    return PROCESS_SCRIPT_END;
                }
                let tuning = self.tuned_sample_tuning(se);
                let l = &mut self.layers[layer];
                l.tuned_sample = se;
                l.freq_scale = tuning;
            }
            _ => {
                semitone = semitone
                    .wrapping_add(self.players[player].transposition as u8)
                    .wrapping_add(c.transposition as u8)
                    .wrapping_add(self.layers[layer].transposition as u8);
                let semitone2 = semitone as usize;
                self.layers[layer].semitone = semitone;
                if semitone >= 0x80 {
                    self.layers[layer].muted = true;
                    return PROCESS_SCRIPT_END;
                }
                let instrument = if self.layers[layer].inst_or_wave == 0xFF {
                    c.instrument
                } else {
                    self.layers[layer].instrument
                };
                let instrument = if instrument <= 2 { 0 } else { instrument };

                if self.layers[layer].portamento.mode != PORTAMENTO_MODE_OFF {
                    let target = self.layers[layer].portamento_target_note;
                    let vel = semitone.max(target) as i32;
                    let tuning;
                    if instrument != 0 {
                        let ts = self.instrument_tuned_sample(instrument, vel);
                        same_tuned_sample = (self.layers[layer].tuned_sample == ts) as i32;
                        self.layers[layer].tuned_sample = ts;
                        tuning = self.tuned_sample_tuning(ts);
                    } else {
                        self.layers[layer].tuned_sample = 0;
                        tuning = 1.0f32;
                        if inst_or_wave >= 0xC0 {
                            self.layers[layer].tuned_sample = self.synthesis_reverbs
                                [(inst_or_wave - 0xC0) as usize & 3]
                                .tuned_sample;
                        }
                    }
                    let f2 = tables::PITCH_FREQUENCIES[semitone2] * tuning;
                    let f14 = tables::PITCH_FREQUENCIES[target as usize & 0x7F] * tuning;
                    let mode = portamento_mode(self.layers[layer].portamento.mode);
                    let (freq_scale, freq_scale2) = match mode {
                        PORTAMENTO_MODE_1 | PORTAMENTO_MODE_3 | PORTAMENTO_MODE_5 => (f14, f2),
                        PORTAMENTO_MODE_2 | PORTAMENTO_MODE_4 => (f2, f14),
                        _ => (f2, f2),
                    };
                    let l = &mut self.layers[layer];
                    l.portamento.extent = (freq_scale2 / freq_scale) - 1.0f32;
                    let mut speed: i32 = if portamento_is_special(l.portamento.mode) {
                        let mut speed =
                            self.players[player].tempo as i32 * 0x8000 / self.max_tempo as i32;
                        if l.delay != 0 {
                            let div = l.delay as i32 * l.portamento_time as i32;
                            speed = if div != 0 {
                                speed * 0x100 / div
                            } else {
                                0x7FFF
                            };
                        }
                        speed
                    } else {
                        let div = l.portamento_time as i32 * self.bp.ticks_per_update as i32;
                        if div != 0 { 0x20000 / div } else { 0x7FFF }
                    };
                    speed = speed.clamp(1, 0x7FFF);
                    l.portamento.speed = speed as u16;
                    l.portamento.cur = 0;
                    l.freq_scale = freq_scale;
                    if portamento_mode(l.portamento.mode) == PORTAMENTO_MODE_5 {
                        l.portamento_target_note = semitone;
                    }
                } else if instrument != 0 {
                    let ts = self.instrument_tuned_sample(instrument, semitone as i32);
                    same_tuned_sample = (ts == self.layers[layer].tuned_sample) as i32;
                    let tuning = self.tuned_sample_tuning(ts);
                    let l = &mut self.layers[layer];
                    l.tuned_sample = ts;
                    l.freq_scale = tables::PITCH_FREQUENCIES[semitone2] * tuning;
                } else {
                    let l = &mut self.layers[layer];
                    l.tuned_sample = 0;
                    l.freq_scale = tables::PITCH_FREQUENCIES[semitone2];
                    if inst_or_wave >= 0xC0 {
                        l.tuned_sample =
                            self.synthesis_reverbs[(inst_or_wave - 0xC0) as usize & 3].tuned_sample;
                    }
                }
            }
        }

        let l = &mut self.layers[layer];
        l.delay2 = l.delay;
        l.freq_scale *= l.bend;

        if l.delay == 0 {
            let mut time = if l.tuned_sample != 0 {
                let sample = self.ram.u32(l.tuned_sample);
                self.ram.u32(self.ram.u32(sample + 8) + 4) as f32
            } else {
                0.0f32
            };
            time *= self.players[player].tempo as f32;
            time *= self.unk_2870;
            time /= l.freq_scale;
            if time > 32766.0 {
                time = 32766.0;
            }
            l.gate_delay = 0;
            l.delay = ((time as i32) as u16).wrapping_add(1) as i16;

            if l.portamento.mode != PORTAMENTO_MODE_OFF && portamento_is_special(l.portamento.mode)
            {
                let mut speed2 = self.players[player].tempo as i32 * 0x8000 / self.max_tempo as i32;
                let div = l.delay as i32 * l.portamento_time as i32;
                speed2 = if div != 0 {
                    speed2 * 0x100 / div
                } else {
                    0x7FFF
                };
                l.portamento.speed = speed2.clamp(1, 0x7FFF) as u16;
            }
        }
        same_tuned_sample
    }

    /// `AudioSeq_SeqLayerProcessScriptStep3` (`__SetNote`)
    fn seq_layer_step3(&mut self, layer: usize, cmd: i32) -> i32 {
        let s = Script::Layer(layer);
        let ch = self.layers[layer].channel.unwrap_or(0);
        let player = self.channels[ch].seq_player;

        if cmd == 0xC0 {
            let d = self.read_compressed_u16(s);
            let l = &mut self.layers[layer];
            l.delay = d as i16;
            l.muted = true;
            l.bit1 = false;
            return PROCESS_SCRIPT_END;
        }

        self.layers[layer].muted = false;
        let delay: u16;
        let mut cmd = cmd;
        if self.channels[ch].large_notes {
            let mut velocity: i32;
            match cmd & 0xC0 {
                0x00 => {
                    delay = self.read_compressed_u16(s);
                    velocity = self.read_u8(s) as i32;
                    self.layers[layer].gate_time = self.read_u8(s);
                    self.layers[layer].last_delay = delay as i16;
                }
                0x40 => {
                    delay = self.read_compressed_u16(s);
                    velocity = self.read_u8(s) as i32;
                    self.layers[layer].gate_time = 0;
                    self.layers[layer].last_delay = delay as i16;
                }
                _ => {
                    delay = self.layers[layer].last_delay as u16;
                    velocity = self.read_u8(s) as i32;
                    self.layers[layer].gate_time = self.read_u8(s);
                }
            }
            if !(0..=0x7F).contains(&velocity) {
                velocity = 0x7F;
            }
            self.layers[layer].velocity_square =
                (velocity as f32 * velocity as f32) / (127.0f32 * 127.0f32);
            cmd -= cmd & 0xC0;
        } else {
            match cmd & 0xC0 {
                0x00 => {
                    delay = self.read_compressed_u16(s);
                    self.layers[layer].last_delay = delay as i16;
                }
                0x40 => delay = self.layers[layer].short_note_default_delay as u16,
                _ => delay = self.layers[layer].last_delay as u16,
            }
            cmd -= cmd & 0xC0;
        }

        let c = self.channels[ch];
        if c.velocity_random_variance != 0 {
            let l = &mut self.layers[layer];
            let mut delta = l.velocity_square
                * (self.audio_random % c.velocity_random_variance as u32) as f32
                / 100.0f32;
            if self.audio_random & 0x8000 != 0 {
                delta = -delta;
            }
            l.velocity_square2 = (l.velocity_square + delta).clamp(0.0, 1.0);
        } else {
            let l = &mut self.layers[layer];
            l.velocity_square2 = l.velocity_square;
        }

        let l = &mut self.layers[layer];
        l.delay = delay as i16;
        l.gate_delay = ((l.gate_time as i32 * delay as i32) >> 8) as i16;

        if c.gate_time_random_variance != 0 {
            // The game uses velocityRandomVariance here (a bug); a zero there
            // would divide by zero on hardware.
            let modulus = c.velocity_random_variance as u32;
            let mut delta = if modulus != 0 {
                (l.gate_delay as i32 * (self.audio_random % modulus) as i32) / 100
            } else {
                0
            };
            if self.audio_random & 0x4000 != 0 {
                delta = -delta;
            }
            l.gate_delay = l.gate_delay.wrapping_add(delta as i16);
            if l.gate_delay < 0 {
                l.gate_delay = 0;
            } else if l.gate_delay > l.delay {
                l.gate_delay = l.delay;
            }
        }

        let p = self.players[player];
        if (p.muted && (c.mute_behavior & (MUTE_BEHAVIOR_STOP_NOTES | MUTE_BEHAVIOR_4)) != 0)
            || c.muted
        {
            self.layers[layer].muted = true;
            return PROCESS_SCRIPT_END;
        }
        if p.skip_ticks != 0 {
            self.layers[layer].muted = true;
            return PROCESS_SCRIPT_END;
        }
        cmd
    }

    /// `AudioSeq_SetChannelPriorities`
    fn set_channel_priorities(&mut self, ch: usize, priority: u8) {
        if priority & 0xF != 0 {
            self.channels[ch].note_priority = priority & 0xF;
        }
        let p = priority >> 4;
        if p != 0 {
            self.channels[ch].some_other_priority = p;
        }
    }

    /// `AudioSeq_GetInstrument`: (instOrWave, `Instrument*`).
    fn get_instrument(&mut self, ch: usize, inst_id: u8, adsr: &mut AdsrSettings) -> (u8, u32) {
        let inst = self.get_instrument_inner(self.channels[ch].font_id, inst_id);
        if inst == 0 {
            return (0, 0);
        }
        adsr.envelope = self.ram.u32(inst + 4);
        adsr.decay_index = self.ram.u8(inst + 3);
        (inst_id.wrapping_add(2), inst)
    }

    /// `AudioSeq_SetInstrument`
    fn set_instrument(&mut self, ch: usize, inst_id: u8) {
        if inst_id >= 0x80 {
            self.channels[ch].inst_or_wave = inst_id as i16;
            self.channels[ch].instrument = 0;
        } else if inst_id == 0x7F {
            self.channels[ch].inst_or_wave = 0;
            self.channels[ch].instrument = 1;
        } else if inst_id == 0x7E {
            self.channels[ch].inst_or_wave = 1;
            self.channels[ch].instrument = 2;
        } else {
            let mut adsr = self.channels[ch].adsr;
            let (v, inst) = self.get_instrument(ch, inst_id, &mut adsr);
            let c = &mut self.channels[ch];
            c.adsr = adsr;
            c.instrument = inst;
            c.inst_or_wave = v as i16;
            if v == 0 {
                c.has_instrument = false;
                return;
            }
        }
        self.channels[ch].has_instrument = true;
    }

    fn seq_font_for(&self, player: usize, index: u8) -> u8 {
        let p = self.players[player];
        if p.default_font == 0xFF {
            return index;
        }
        let t = &self.data.seq_font_table;
        let id = p.seq_id as usize;
        if t.len() < 2 * id + 2 {
            return index;
        }
        let at = u16::from_be_bytes([t[2 * id], t[2 * id + 1]]) as usize;
        let count = t.get(at).copied().unwrap_or(0) as usize;
        t.get((at + count).wrapping_sub(index as usize))
            .copied()
            .unwrap_or(index)
    }

    /// `AudioSeq_SequenceChannelProcessScript`
    fn sequence_channel_process_script(&mut self, ch: usize) {
        let s = Script::Channel(ch);
        let player = self.channels[ch].seq_player;
        let seq_data = self.players[player].seq_data;

        'script: {
            if self.channels[ch].stop_script {
                break 'script;
            }
            if self.players[player].muted
                && (self.channels[ch].mute_behavior & MUTE_BEHAVIOR_STOP_SCRIPT) != 0
            {
                return;
            }
            if self.channels[ch].delay >= 2 {
                self.channels[ch].delay -= 1;
                break 'script;
            }

            loop {
                let cmd = self.read_u8(s);
                if cmd >= 0xB0 {
                    let mut high = ARGS_TABLE[(cmd - 0xB0) as usize];
                    let n = high & 3;
                    let mut args = [0u32; 3];
                    for a in args.iter_mut().take(n as usize) {
                        *a = if high & 0x80 == 0 {
                            self.read_u8(s) as u32
                        } else {
                            self.read_s16(s) as i32 as u32
                        };
                        high <<= 1;
                    }

                    if cmd >= 0xF2 {
                        let delay = self.flow_control(s, seq_data, cmd, args[0]);
                        if delay != 0 {
                            if delay == PROCESS_SCRIPT_END {
                                self.sequence_channel_disable(ch);
                            } else {
                                self.channels[ch].delay = delay as u16;
                            }
                            break;
                        }
                        continue;
                    }

                    self.channel_command(ch, cmd, args, seq_data);
                    if cmd == 0xEA {
                        break 'script;
                    }
                    continue;
                }

                if cmd >= 0x70 {
                    let mut low = (cmd & 7) as usize;
                    if (cmd & 0xF8) != 0x70 && low >= 4 {
                        low = 0;
                    }
                    match cmd & 0xF8 {
                        0x80 => {
                            let v = match self.channels[ch].layers[low] {
                                Some(l) => self.layers[l].finished as i8,
                                None => -1,
                            };
                            self.channels[ch].script_state.value = v;
                        }
                        0x88 => {
                            let off = self.read_s16(s) as u16;
                            if self.seq_channel_set_layer(ch, low) == 0 {
                                let l = self.channels[ch].layers[low].unwrap();
                                self.layers[l].script_state.pc = seq_data.wrapping_add(off as u32);
                            }
                        }
                        0x90 => self.seq_layer_free(ch, low),
                        0x98 => {
                            let value = self.channels[ch].script_state.value;
                            if value != -1 && self.seq_channel_set_layer(ch, low) != -1 {
                                let data = self.channels[ch]
                                    .dyn_table
                                    .wrapping_add(2 * value as i32 as u32);
                                let off =
                                    (self.ram.u8(data) as u32) << 8 | self.ram.u8(data + 1) as u32;
                                let l = self.channels[ch].layers[low].unwrap();
                                self.layers[l].script_state.pc =
                                    seq_data.wrapping_add(off & 0xFFFF);
                            }
                        }
                        0x70 => {
                            let v = self.channels[ch].script_state.value;
                            self.channels[ch].seq_script_io[low] = v;
                        }
                        0x78 => {
                            let rel = self.read_s16(s);
                            if self.seq_channel_set_layer(ch, low) == 0 {
                                let pc = self.channels[ch].script_state.pc;
                                let l = self.channels[ch].layers[low].unwrap();
                                self.layers[l].script_state.pc = pc.wrapping_add(rel as i32 as u32);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                let low = (cmd & 0xF) as usize;
                match cmd & 0xF0 {
                    0x00 => {
                        self.channels[ch].delay = low as u16;
                        break 'script;
                    }
                    0x10 => {
                        // ldsample: an asynchronous sample load. Loads finish
                        // instantly here; report success the way the game's
                        // slow-load completion does.
                        let io = if low < 8 { low } else { low - 8 };
                        self.channels[ch].seq_script_io[io] = 1;
                    }
                    0x60 => {
                        let c = &mut self.channels[ch];
                        c.script_state.value = c.seq_script_io[low & 7];
                        if low < 2 {
                            c.seq_script_io[low] = -1;
                        }
                    }
                    0x50 => {
                        let c = &mut self.channels[ch];
                        c.script_state.value =
                            c.script_state.value.wrapping_sub(c.seq_script_io[low & 7]);
                    }
                    0x20 => {
                        let off = self.read_s16(s) as u16;
                        self.sequence_channel_enable(
                            player,
                            low,
                            seq_data.wrapping_add(off as u32),
                        );
                    }
                    0x30 => {
                        let port = self.read_u8(s) as usize;
                        let v = self.channels[ch].script_state.value;
                        let target = self.players[player].channels[low];
                        if let Some(io) = self.channels[target].seq_script_io.get_mut(port) {
                            *io = v;
                        }
                    }
                    0x40 => {
                        let port = self.read_u8(s) as usize;
                        let target = self.players[player].channels[low];
                        let v = self.channels[target]
                            .seq_script_io
                            .get(port)
                            .copied()
                            .unwrap_or(0);
                        self.channels[ch].script_state.value = v;
                    }
                    _ => {}
                }
            }
        }

        for i in 0..4 {
            if let Some(l) = self.channels[ch].layers[i] {
                self.seq_layer_process_script(l);
            }
        }
    }

    /// The `cmd >= 0xB0` (non-control-flow) channel commands.
    fn channel_command(&mut self, ch: usize, cmd: u8, args: [u32; 3], seq_data: u32) {
        let player = self.channels[ch].seq_player;
        let a0u8 = args[0] as u8;
        let a0u16 = args[0] as u16;
        match cmd {
            0xEA => self.channels[ch].stop_script = true,
            0xF1 => {
                let pool = self.channels[ch].note_pool;
                self.note_pool_clear(pool);
                self.note_pool_fill(pool, a0u8 as i32);
            }
            0xF0 => {
                let pool = self.channels[ch].note_pool;
                self.note_pool_clear(pool);
            }
            0xC2 => self.channels[ch].dyn_table = seq_data.wrapping_add(a0u16 as u32),
            0xC5 => {
                let value = self.channels[ch].script_state.value;
                if value != -1 {
                    let data = self.channels[ch]
                        .dyn_table
                        .wrapping_add(2 * value as i32 as u32);
                    let off = ((self.ram.u8(data) as u32) << 8) + self.ram.u8(data + 1) as u32;
                    self.channels[ch].dyn_table = seq_data.wrapping_add(off & 0xFFFF);
                }
            }
            0xEB | 0xC1 => {
                let mut inst = a0u8;
                if cmd == 0xEB {
                    let font = self.seq_font_for(player, a0u8);
                    if self.is_font_loaded(font) {
                        self.channels[ch].font_id = font;
                    }
                    inst = args[1] as u8;
                }
                self.set_instrument(ch, inst);
            }
            0xC3 => self.channels[ch].large_notes = false,
            0xC4 => self.channels[ch].large_notes = true,
            0xDF => {
                self.channels[ch].volume = a0u8 as i32 as f32 / 127.0f32;
                self.channels[ch].change_volume = true;
            }
            0xE0 => {
                self.channels[ch].volume_scale = a0u8 as i32 as f32 / 128.0f32;
                self.channels[ch].change_volume = true;
            }
            0xDE => {
                self.channels[ch].freq_scale = a0u16 as i32 as f32 / 32768.0f32;
                self.channels[ch].change_freq_scale = true;
            }
            0xD3 => {
                self.channels[ch].freq_scale =
                    tables::BEND_PITCH_ONE_OCTAVE[a0u8.wrapping_add(0x80) as usize];
                self.channels[ch].change_freq_scale = true;
            }
            0xEE => {
                self.channels[ch].freq_scale =
                    tables::BEND_PITCH_TWO_SEMITONES[a0u8.wrapping_add(0x80) as usize];
                self.channels[ch].change_freq_scale = true;
            }
            0xDD => {
                self.channels[ch].new_pan = a0u8;
                self.channels[ch].change_pan = true;
            }
            0xDC => {
                self.channels[ch].pan_channel_weight = a0u8;
                self.channels[ch].change_pan = true;
            }
            0xDB => self.channels[ch].transposition = a0u8 as i8 as i16,
            0xDA => self.channels[ch].adsr.envelope = seq_data.wrapping_add(a0u16 as u32),
            0xD9 => self.channels[ch].adsr.decay_index = a0u8,
            0xD8 => {
                let c = &mut self.channels[ch];
                c.vibrato_depth_target = a0u8 as u16 * 8;
                c.vibrato_depth_start = 0;
                c.vibrato_depth_change_delay = 0;
            }
            0xD7 => {
                let c = &mut self.channels[ch];
                c.vibrato_rate_change_delay = 0;
                c.vibrato_rate_target = a0u8 as u16 * 32;
                c.vibrato_rate_start = a0u8 as u16 * 32;
            }
            0xE2 => {
                let c = &mut self.channels[ch];
                c.vibrato_depth_start = a0u8 as u16 * 8;
                c.vibrato_depth_target = args[1] as u8 as u16 * 8;
                c.vibrato_depth_change_delay = args[2] as u8 as u16 * 16;
            }
            0xE1 => {
                let c = &mut self.channels[ch];
                c.vibrato_rate_start = a0u8 as u16 * 32;
                c.vibrato_rate_target = args[1] as u8 as u16 * 32;
                c.vibrato_rate_change_delay = args[2] as u8 as u16 * 16;
            }
            0xE3 => self.channels[ch].vibrato_delay = a0u8 as u16 * 16,
            0xD4 => self.channels[ch].target_reverb_vol = a0u8,
            0xC6 => {
                let font = self.seq_font_for(player, a0u8);
                if self.is_font_loaded(font) {
                    self.channels[ch].font_id = font;
                }
            }
            0xC7 => {
                let addr = seq_data.wrapping_add(args[1] as u16 as u32);
                let v = (self.channels[ch].script_state.value as u8).wrapping_add(a0u8);
                self.ram.set_u8(addr, v);
            }
            0xC8 | 0xCC | 0xC9 => {
                let v = a0u8 as i8;
                let st = &mut self.channels[ch].script_state;
                match cmd {
                    0xC8 => st.value = st.value.wrapping_sub(v),
                    0xCC => st.value = v,
                    _ => st.value &= v,
                }
            }
            0xCD => {
                let target = self.players[player].channels[a0u8 as usize & 0xF];
                self.sequence_channel_disable(target);
            }
            0xCA => {
                self.channels[ch].mute_behavior = a0u8;
                self.channels[ch].change_volume = true;
            }
            0xCB => {
                let value = self.channels[ch].script_state.value as i32;
                let addr = seq_data.wrapping_add((a0u16 as i32 + value) as u32);
                self.channels[ch].script_state.value = self.ram.u8(addr) as i8;
            }
            0xCE => self.channels[ch].unk_22 = a0u16,
            0xCF => {
                let addr = seq_data.wrapping_add(a0u16 as u32);
                let v = self.channels[ch].unk_22;
                self.ram.set_u8(addr, (v >> 8) as u8);
                self.ram.set_u8(addr + 1, v as u8);
            }
            0xD0 => {
                let c = &mut self.channels[ch];
                c.stereo_headset_effects = a0u8 & 0x80 != 0;
                c.stereo = a0u8 & 0x7F;
            }
            0xD1 => self.channels[ch].note_alloc_policy = a0u8,
            0xD2 => self.channels[ch].adsr.sustain = a0u8,
            0xE5 => self.channels[ch].reverb_index = a0u8,
            0xE4 => {
                let st = self.channels[ch].script_state;
                if st.value != -1 {
                    let data = self.channels[ch]
                        .dyn_table
                        .wrapping_add(2 * st.value as i32 as u32);
                    let off = ((self.ram.u8(data) as u32) << 8) + self.ram.u8(data + 1) as u32;
                    let st = &mut self.channels[ch].script_state;
                    st.stack[st.depth as usize & 3] = st.pc;
                    st.depth = st.depth.wrapping_add(1);
                    st.pc = seq_data.wrapping_add(off & 0xFFFF);
                }
            }
            0xE6 => self.channels[ch].book_offset = a0u8,
            0xE7 => {
                let data = seq_data.wrapping_add(a0u16 as u32);
                let b: [u8; 8] = std::array::from_fn(|i| self.ram.u8(data + i as u32));
                self.channels[ch].mute_behavior = b[0];
                self.channels[ch].note_alloc_policy = b[1];
                self.set_channel_priorities(ch, b[2]);
                let c = &mut self.channels[ch];
                c.transposition = b[3] as i8 as i16;
                c.new_pan = b[4];
                c.pan_channel_weight = b[5];
                c.target_reverb_vol = b[6];
                c.reverb_index = b[7];
                c.change_pan = true;
            }
            0xE8 => {
                self.channels[ch].mute_behavior = a0u8;
                self.channels[ch].note_alloc_policy = args[1] as u8;
                self.set_channel_priorities(ch, args[2] as u8);
                let s = Script::Channel(ch);
                let transposition = self.read_u8(s) as i8 as i16;
                let new_pan = self.read_u8(s);
                let weight = self.read_u8(s);
                let reverb = self.read_u8(s);
                let reverb_index = self.read_u8(s);
                let c = &mut self.channels[ch];
                c.transposition = transposition;
                c.new_pan = new_pan;
                c.pan_channel_weight = weight;
                c.target_reverb_vol = reverb;
                c.reverb_index = reverb_index;
                c.change_pan = true;
            }
            0xEC => {
                let c = &mut self.channels[ch];
                c.vibrato_depth_target = 0;
                c.vibrato_depth_start = 0;
                c.vibrato_depth_change_delay = 0;
                c.vibrato_rate_target = 0;
                c.vibrato_rate_start = 0;
                c.vibrato_rate_change_delay = 0;
                c.filter = 0;
                c.gain = 0;
                c.adsr.sustain = 0;
                c.velocity_random_variance = 0;
                c.gate_time_random_variance = 0;
                c.comb_filter_size = 0;
                c.comb_filter_gain = 0;
                c.book_offset = 0;
                c.freq_scale = 1.0;
            }
            0xE9 => self.set_channel_priorities(ch, a0u8),
            0xED => self.channels[ch].gain = a0u8,
            0xB0 => self.channels[ch].filter = seq_data.wrapping_add(a0u16 as u32),
            0xB1 => self.channels[ch].filter = 0,
            0xB3 => {
                let filter = self.channels[ch].filter;
                if filter != 0 {
                    self.load_filter(filter, (a0u8 >> 4) as usize, (a0u8 & 0xF) as usize);
                }
            }
            0xB2 => {
                let value = self.channels[ch].script_state.value as i32;
                let addr = seq_data.wrapping_add((a0u16 as i32 + value * 2) as u32);
                self.channels[ch].unk_22 = self.ram.u16(addr);
            }
            0xB4 => {
                self.channels[ch].dyn_table = seq_data.wrapping_add(self.channels[ch].unk_22 as u32)
            }
            0xB5 => {
                let value = self.channels[ch].script_state.value as i32;
                let addr = self.channels[ch].dyn_table.wrapping_add((value * 2) as u32);
                self.channels[ch].unk_22 = self.ram.u16(addr);
            }
            0xB6 => {
                let value = self.channels[ch].script_state.value as i32;
                let addr = self.channels[ch].dyn_table.wrapping_add(value as u32);
                self.channels[ch].script_state.value = self.ram.u8(addr) as i8;
            }
            0xB7 => {
                let r = self.audio_random;
                self.channels[ch].unk_22 = if args[0] == 0 {
                    (r & 0xFFFF) as u16
                } else {
                    (r % args[0]) as u16
                };
            }
            0xB8 => {
                let r = self.audio_random;
                self.channels[ch].script_state.value = if args[0] == 0 {
                    r as i8
                } else {
                    (r % args[0]) as i8
                };
            }
            0xBD => {
                let r = self.next_random();
                let c = &mut self.channels[ch];
                c.unk_22 = if args[0] == 0 {
                    (r & 0xFFFF) as u16
                } else {
                    (r % args[0]) as u16
                };
                c.unk_22 = c.unk_22.wrapping_add(args[1] as u16);
                let hi = (c.unk_22 / 0x100).wrapping_add(0x80);
                let lo = c.unk_22 % 0x100;
                c.unk_22 = hi << 8 | lo;
            }
            0xB9 => self.channels[ch].velocity_random_variance = a0u8,
            0xBA => self.channels[ch].gate_time_random_variance = a0u8,
            0xBB => {
                self.channels[ch].comb_filter_size = a0u8;
                self.channels[ch].comb_filter_gain = args[1] as u16;
            }
            0xBC => {
                self.channels[ch].unk_22 = self.channels[ch].unk_22.wrapping_add(args[0] as u16)
            }
            _ => {}
        }
    }

    /// `AudioSeq_SequencePlayerProcessSequence`
    fn sequence_player_process_sequence(&mut self, player: usize) {
        let s = Script::Player(player);
        if !self.players[player].enabled {
            return;
        }
        if !self.is_font_loaded(self.players[player].default_font) {
            self.sequence_player_disable(player);
            return;
        }
        {
            let p = &self.players[player];
            if p.muted && (p.mute_behavior & MUTE_BEHAVIOR_STOP_SCRIPT) != 0 {
                return;
            }
        }
        let max_tempo = self.max_tempo as u16;
        {
            let p = &mut self.players[player];
            p.script_counter = p.script_counter.wrapping_add(1);
            p.tempo_acc = p.tempo_acc.wrapping_add(p.tempo);
            p.tempo_acc = p.tempo_acc.wrapping_add(p.tempo_change as i16 as u16);
            if p.tempo_acc < max_tempo {
                return;
            }
            p.tempo_acc = p.tempo_acc.wrapping_sub(max_tempo);
            if p.stop_script {
                return;
            }
            p.ticks_elapsed = p.ticks_elapsed.wrapping_add(1);
        }
        let seq_data = self.players[player].seq_data;

        if self.players[player].delay > 1 {
            self.players[player].delay -= 1;
        } else {
            self.players[player].recalculate_volume = true;
            loop {
                let cmd = self.read_u8(s);
                if cmd >= 0xF2 {
                    let arg = self.control_flow_argument(s, cmd);
                    let delay = self.flow_control(s, seq_data, cmd, arg as u32);
                    if delay != 0 {
                        if delay == -1 {
                            self.sequence_player_disable(player);
                        } else {
                            self.players[player].delay = delay as u16;
                        }
                        break;
                    }
                    continue;
                }

                if cmd >= 0xC0 {
                    match cmd {
                        0xF1 => {
                            let pool = self.players[player].note_pool;
                            self.note_pool_clear(pool);
                            let n = self.read_u8(s);
                            self.note_pool_fill(pool, n as i32);
                        }
                        0xF0 => {
                            let pool = self.players[player].note_pool;
                            self.note_pool_clear(pool);
                        }
                        0xDF | 0xDE => {
                            if cmd == 0xDF {
                                self.players[player].transposition = 0;
                            }
                            let v = self.read_u8(s) as i8 as i16;
                            let p = &mut self.players[player];
                            p.transposition = p.transposition.wrapping_add(v);
                        }
                        0xDD => {
                            let v = self.read_u8(s) as u16;
                            let p = &mut self.players[player];
                            p.tempo = v.wrapping_mul(SEQTICKS_PER_BEAT);
                            if p.tempo > max_tempo {
                                p.tempo = max_tempo;
                            }
                            if p.tempo as i16 <= 0 {
                                p.tempo = 1;
                            }
                        }
                        0xDC => {
                            let v = self.read_u8(s) as i8 as i16;
                            self.players[player].tempo_change =
                                v.wrapping_mul(SEQTICKS_PER_BEAT as i16) as u16;
                        }
                        0xDA => {
                            let mode = self.read_u8(s);
                            let t = self.read_s16(s) as u16;
                            let p = &mut self.players[player];
                            match mode {
                                0 | 1 => {
                                    if p.state != 2 {
                                        p.fade_timer_unk_eu = t;
                                        p.state = mode;
                                    }
                                }
                                2 => {
                                    p.fade_timer = t;
                                    p.state = mode;
                                    p.fade_velocity =
                                        (0.0f32 - p.fade_volume) / (p.fade_timer as i32 as f32);
                                }
                                _ => {}
                            }
                        }
                        0xDB => {
                            let value = self.read_u8(s) as f32;
                            let p = &mut self.players[player];
                            match p.state {
                                0 | 1 => {
                                    if p.state == 1 {
                                        p.state = 0;
                                        p.fade_volume = 0.0;
                                    }
                                    p.fade_timer = p.fade_timer_unk_eu;
                                    if p.fade_timer_unk_eu != 0 {
                                        p.fade_velocity = ((value / 127.0f32) - p.fade_volume)
                                            / (p.fade_timer as i32 as f32);
                                    } else {
                                        p.fade_volume = value / 127.0f32;
                                    }
                                }
                                _ => {}
                            }
                        }
                        0xD9 => {
                            let v = self.read_u8(s) as i8 as f32;
                            self.players[player].fade_volume_scale = v / 127.0f32;
                        }
                        0xD7 => {
                            let bits = self.read_s16(s) as u16;
                            self.sequence_player_setup_channels(player, bits);
                        }
                        0xD6 => {
                            self.read_s16(s);
                        }
                        0xD5 => {
                            let v = self.read_u8(s) as i8 as f32;
                            self.players[player].mute_volume_scale = v / 127.0f32;
                        }
                        0xD4 => self.players[player].muted = true,
                        0xD3 => {
                            let v = self.read_u8(s);
                            self.players[player].mute_behavior = v;
                        }
                        0xD1 | 0xD2 => {
                            let off = self.read_s16(s) as u16;
                            let table = seq_data.wrapping_add(off as u32);
                            if cmd == 0xD2 {
                                self.players[player].short_note_velocity_table = table;
                            } else {
                                self.players[player].short_note_gate_time_table = table;
                            }
                        }
                        0xD0 => {
                            let v = self.read_u8(s);
                            self.players[player].note_alloc_policy = v;
                        }
                        0xCE => {
                            let m = self.read_u8(s);
                            let r = self.audio_random >> 2;
                            self.players[player].script_state.value = if m == 0 {
                                (r & 0xFF) as i8
                            } else {
                                (r % m as u32) as i8
                            };
                        }
                        0xCD => {
                            let table = self.read_s16(s) as u16;
                            let st = self.players[player].script_state;
                            if st.value != -1 && st.depth != 3 {
                                let data = seq_data
                                    .wrapping_add((table as i32 + ((st.value as i32) << 1)) as u32);
                                let off = ((self.ram.u8(data) as u32) << 8)
                                    + self.ram.u8(data + 1) as u32;
                                let st = &mut self.players[player].script_state;
                                st.stack[st.depth as usize & 3] = st.pc;
                                st.depth += 1;
                                st.pc = seq_data.wrapping_add(off & 0xFFFF);
                            }
                        }
                        0xCC => {
                            let v = self.read_u8(s);
                            self.players[player].script_state.value = v as i8;
                        }
                        0xC9 => {
                            let v = self.read_u8(s);
                            self.players[player].script_state.value &= v as i8;
                        }
                        0xC8 => {
                            let v = self.read_u8(s);
                            let st = &mut self.players[player].script_state;
                            st.value = st.value.wrapping_sub(v as i8);
                        }
                        0xC7 => {
                            let add = self.read_u8(s);
                            let off = self.read_s16(s) as u16;
                            let v =
                                (self.players[player].script_state.value as u8).wrapping_add(add);
                            self.ram.set_u8(seq_data.wrapping_add(off as u32), v);
                        }
                        0xC6 => {
                            self.players[player].stop_script = true;
                            return;
                        }
                        0xC5 => {
                            let v = self.read_s16(s) as u16;
                            self.players[player].script_counter = v as u32;
                        }
                        0xEF => {
                            self.read_s16(s);
                            self.read_u8(s);
                        }
                        0xC4 => {
                            let mut target = self.read_u8(s);
                            if target == 0xFF {
                                target = self.players[player].player_idx as u8;
                            }
                            let seq = self.read_u8(s);
                            self.init_seq_player(target as usize, seq as usize);
                            if target as usize == player {
                                return;
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                let low = (cmd & 0x0F) as usize;
                match cmd & 0xF0 {
                    0x00 => {
                        let ch = self.players[player].channels[low];
                        self.players[player].script_state.value =
                            (self.channels[ch].enabled as i8) ^ 1;
                    }
                    0x50 => {
                        let p = &mut self.players[player];
                        p.script_state.value =
                            p.script_state.value.wrapping_sub(p.seq_script_io[low & 7]);
                    }
                    0x70 => {
                        let p = &mut self.players[player];
                        p.seq_script_io[low & 7] = p.script_state.value;
                    }
                    0x80 => {
                        let p = &mut self.players[player];
                        p.script_state.value = p.seq_script_io[low & 7];
                        if low < 2 {
                            p.seq_script_io[low] = -1;
                        }
                    }
                    0x40 => {
                        let ch = self.players[player].channels[low];
                        self.sequence_channel_disable(ch);
                    }
                    0x90 => {
                        let off = self.read_s16(s) as u16;
                        self.sequence_channel_enable(
                            player,
                            low,
                            seq_data.wrapping_add(off as u32),
                        );
                    }
                    0xA0 => {
                        let rel = self.read_s16(s);
                        let pc = self.players[player].script_state.pc;
                        self.sequence_channel_enable(
                            player,
                            low,
                            pc.wrapping_add(rel as i32 as u32),
                        );
                    }
                    0xB0 => {
                        // ldseq: an asynchronous load of another sequence's
                        // bytes; not supported, just consume the arguments.
                        self.read_u8(s);
                        self.read_s16(s);
                    }
                    0x60 => {
                        // ldres: asynchronous resource load; report completion.
                        self.read_u8(s);
                        self.read_u8(s);
                        self.players[player].seq_script_io[low & 7] = 0;
                    }
                    _ => {}
                }
            }
        }

        for i in 0..16 {
            let ch = self.players[player].channels[i];
            if self.channels[ch].enabled {
                self.sequence_channel_process_script(ch);
            }
        }
    }

    /// `AudioSeq_ProcessSequences`
    pub(crate) fn process_sequences(&mut self, arg0: i32) {
        self.sample_state_offset =
            ((self.bp.ticks_per_update as i32 - arg0 - 1) as usize) * self.num_notes;
        for i in 0..self.bp.num_sequence_players as usize {
            if self.players[i].enabled {
                self.sequence_player_process_sequence(i);
                self.sequence_player_process_sound(i);
            }
        }
        self.process_notes();
    }

    /// `AudioSeq_SkipForwardSequence`
    pub(crate) fn skip_forward_sequence(&mut self, player: usize) {
        while self.players[player].skip_ticks > 0 {
            self.sequence_player_process_sequence(player);
            self.sequence_player_process_sound(player);
            self.players[player].skip_ticks -= 1;
        }
    }

    /// `AudioSeq_ResetSequencePlayer`
    pub(crate) fn reset_sequence_player(&mut self, player: usize) {
        self.sequence_player_disable(player);
        let vel = self.default_short_velocity;
        let gate = self.default_short_gate;
        let p = &mut self.players[player];
        p.stop_script = false;
        p.delay = 0;
        p.state = 1;
        p.fade_timer = 0;
        p.fade_timer_unk_eu = 0;
        p.tempo_acc = 0;
        p.tempo = 120 * SEQTICKS_PER_BEAT;
        p.tempo_change = 0;
        p.transposition = 0;
        p.note_alloc_policy = 0;
        p.short_note_velocity_table = vel;
        p.short_note_gate_time_table = gate;
        p.script_counter = 0;
        p.fade_volume = 1.0;
        p.fade_velocity = 0.0;
        p.volume = 0.0;
        p.mute_volume_scale = 0.5;
        for i in 0..16 {
            let ch = self.players[player].channels[i];
            self.init_sequence_channel(ch);
        }
    }

    /// `AudioSeq_InitSequencePlayers` and friends: build all players, their
    /// channels and the layer free list.
    pub(crate) fn init_sequence_players(&mut self) {
        // Layers.
        self.layers.clear();
        self.lists.reset_head(self.layer_free_list);
        for i in 0..super::NUM_LAYERS {
            let item = self.lists.new_item(ListValue::Layer(i));
            self.layers.push(SequenceLayer {
                list_item: item,
                ..Default::default()
            });
            self.lists.push_back(self.layer_free_list, item);
        }

        // Players and channels.
        self.players.clear();
        self.channels.clear();
        for pi in 0..super::NUM_PLAYERS {
            let pool = self.lists.new_pool();
            let mut channels = [0usize; 16];
            for (i, slot) in channels.iter_mut().enumerate() {
                let cpool = self.lists.new_pool();
                let id = self.channels.len();
                self.channels.push(new_channel(pi, cpool));
                *slot = id;
                let _ = i;
            }
            self.players.push(SequencePlayer {
                enabled: false,
                finished: false,
                muted: false,
                recalculate_volume: false,
                stop_script: false,
                apply_bend: false,
                state: 0,
                note_alloc_policy: 0,
                mute_behavior: MUTE_BEHAVIOR_SOFTEN | MUTE_BEHAVIOR_STOP_NOTES,
                seq_id: 0,
                default_font: 0,
                player_idx: pi as i8,
                tempo: 0,
                tempo_acc: 0,
                tempo_change: 0,
                transposition: 0,
                delay: 0,
                fade_timer: 0,
                fade_timer_unk_eu: 0,
                seq_data: 0,
                fade_volume: 0.0,
                fade_velocity: 0.0,
                volume: 0.0,
                mute_volume_scale: 0.0,
                fade_volume_scale: 1.0,
                applied_fade_volume: 0.0,
                bend: 1.0,
                channels,
                script_state: SeqScriptState::default(),
                short_note_velocity_table: 0,
                short_note_gate_time_table: 0,
                note_pool: pool,
                skip_ticks: 0,
                script_counter: 0,
                seq_script_io: [-1; 8],
                ticks_elapsed: 0,
            });
        }
        for pi in 0..super::NUM_PLAYERS {
            self.reset_sequence_player(pi);
        }
    }
}

fn new_channel(player: usize, pool: usize) -> SequenceChannel {
    SequenceChannel {
        enabled: false,
        finished: false,
        stop_script: false,
        muted: false,
        has_instrument: false,
        stereo_headset_effects: false,
        large_notes: false,
        change_freq_scale: false,
        change_volume: false,
        change_pan: false,
        note_alloc_policy: 0,
        mute_behavior: 0,
        target_reverb_vol: 0,
        note_priority: 0,
        some_other_priority: 0,
        font_id: 0,
        reverb_index: 0,
        book_offset: 0,
        new_pan: 0,
        pan_channel_weight: 0,
        gain: 0,
        velocity_random_variance: 0,
        gate_time_random_variance: 0,
        comb_filter_size: 0,
        vibrato_rate_start: 0,
        vibrato_depth_start: 0,
        vibrato_rate_target: 0,
        vibrato_depth_target: 0,
        vibrato_rate_change_delay: 0,
        vibrato_depth_change_delay: 0,
        vibrato_delay: 0,
        delay: 0,
        comb_filter_gain: 0,
        unk_22: 0,
        inst_or_wave: 0,
        transposition: 0,
        volume_scale: 0.0,
        volume: 0.0,
        pan: 0,
        applied_volume: 0.0,
        freq_scale: 0.0,
        dyn_table: 0,
        instrument: 0,
        seq_player: player,
        layers: [None; 4],
        script_state: SeqScriptState::default(),
        adsr: AdsrSettings::default(),
        note_pool: pool,
        seq_script_io: [-1; 8],
        filter: 0,
        stereo: 0,
    }
}
