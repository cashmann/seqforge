//! High-level emulation of the RSP audio microcode OoT and MM use (the "NEAD"
//! ABI), executing the same 64-bit command lists the game builds.
//!
//! Command semantics are ported from mupen64plus-rsp-hle (`alist.c`,
//! `alist_nead.c`, `audio.c`; GPL-2.0-or-later, Copyright (C) 2014 Bobby
//! Smiles, 2009 Richard Goedeken, 2002 Hacktarux), which was reverse
//! engineered from the microcode itself. DMEM is modelled big-endian, so the
//! host byte-order tricks in that code (`^ S`) are not needed here.

use crate::ram::Ram;

pub const A_ADPCM: u32 = 1;
pub const A_CLEARBUFF: u32 = 2;
pub const A_UNK3: u32 = 3;
pub const A_ADDMIXER: u32 = 4;
pub const A_RESAMPLE: u32 = 5;
pub const A_RESAMPLE_ZOH: u32 = 6;
pub const A_FILTER: u32 = 7;
pub const A_SETBUFF: u32 = 8;
pub const A_DUPLICATE: u32 = 9;
pub const A_DMEMMOVE: u32 = 10;
pub const A_LOADADPCM: u32 = 11;
pub const A_MIXER: u32 = 12;
pub const A_INTERLEAVE: u32 = 13;
pub const A_HILOGAIN: u32 = 14;
pub const A_SETLOOP: u32 = 15;
pub const A_INTERL: u32 = 17;
pub const A_ENVSETUP1: u32 = 18;
pub const A_ENVMIXER: u32 = 19;
pub const A_LOADBUFF: u32 = 20;
pub const A_SAVEBUFF: u32 = 21;
pub const A_ENVSETUP2: u32 = 22;
pub const A_S8DEC: u32 = 23;
pub const A_UNK19: u32 = 25;

pub const A_INIT: u32 = 0x01;
pub const A_CONTINUE: u32 = 0x00;
pub const A_LOOP: u32 = 0x02;

pub type Acmd = (u32, u32);

fn shl(v: u32, shift: u32, width: u32) -> u32 {
    (v & ((1u32 << width) - 1)) << shift
}

// Command builders, mirroring the macros in the decomp's `ultra64/abi.h`
// bit for bit (including their truncations).

pub fn adpcm_dec(f: u32, state: u32) -> Acmd {
    (shl(A_ADPCM, 24, 8) | shl(f, 16, 8), state)
}

pub fn clear_buffer(dmem: u32, size: u32) -> Acmd {
    (shl(A_CLEARBUFF, 24, 8) | shl(dmem, 0, 24), size)
}

pub fn env_mixer(dmemi: u32, count: u32, swap_lr: u32, x: [u32; 4], dests: u32) -> Acmd {
    (
        shl(A_ENVMIXER, 24, 8)
            | shl(dmemi >> 4, 16, 8)
            | shl(count, 8, 8)
            | shl(swap_lr, 4, 1)
            | shl(x[0], 3, 1)
            | shl(x[1], 2, 1)
            | shl(x[2], 1, 1)
            | shl(x[3], 0, 1),
        dests,
    )
}

pub fn interleave(o: u32, l: u32, r: u32, c: u32) -> Acmd {
    (
        shl(A_INTERLEAVE, 24, 8) | shl(c >> 4, 16, 8) | shl(o, 0, 16),
        shl(l, 16, 16) | shl(r, 0, 16),
    )
}

pub fn interl(dmemi: u32, dmemo: u32, count: u32) -> Acmd {
    (
        shl(A_INTERL, 24, 8) | shl(count, 0, 16),
        shl(dmemi, 16, 16) | shl(dmemo, 0, 16),
    )
}

pub fn load_buffer(src: u32, dmem: u32, size: u32) -> Acmd {
    (
        shl(A_LOADBUFF, 24, 8) | shl(size >> 4, 16, 8) | shl(dmem, 0, 16),
        src,
    )
}

pub fn save_buffer(dmem: u32, dst: u32, size: u32) -> Acmd {
    (
        shl(A_SAVEBUFF, 24, 8) | shl(size >> 4, 16, 8) | shl(dmem, 0, 16),
        dst,
    )
}

pub fn mix(f: u32, g: u32, i: u32, o: u32) -> Acmd {
    (
        shl(A_MIXER, 24, 8) | shl(f, 16, 8) | shl(g, 0, 16),
        shl(i, 16, 16) | shl(o, 0, 16),
    )
}

pub fn resample(f: u32, p: u32, state: u32) -> Acmd {
    (
        shl(A_RESAMPLE, 24, 8) | shl(f, 16, 8) | shl(p, 0, 16),
        state,
    )
}

pub fn set_buffer(f: u32, i: u32, o: u32, c: u32) -> Acmd {
    (
        shl(A_SETBUFF, 24, 8) | shl(f, 16, 8) | shl(i, 0, 16),
        shl(o, 16, 16) | shl(c, 0, 16),
    )
}

pub fn set_loop(a: u32) -> Acmd {
    (shl(A_SETLOOP, 24, 8), a)
}

pub fn dmem_move(i: u32, o: u32, c: u32) -> Acmd {
    (
        shl(A_DMEMMOVE, 24, 8) | shl(i, 0, 24),
        shl(o, 16, 16) | shl(c, 0, 16),
    )
}

pub fn load_adpcm(c: u32, d: u32) -> Acmd {
    (shl(A_LOADADPCM, 24, 8) | shl(c, 0, 24), d)
}

pub fn env_setup1(a: u32, b: u32, c: u32, d: u32) -> Acmd {
    (
        shl(A_ENVSETUP1, 24, 8) | shl(a, 16, 8) | shl(b, 0, 16),
        shl(c, 16, 16) | shl(d, 0, 16),
    )
}

pub fn env_setup2(vol_left: u32, vol_right: u32) -> Acmd {
    (
        shl(A_ENVSETUP2, 24, 8),
        shl(vol_left, 16, 16) | shl(vol_right, 0, 16),
    )
}

pub fn filter(f: u32, count_or_buf: u32, addr: u32) -> Acmd {
    (
        shl(A_FILTER, 24, 8) | shl(f, 16, 8) | shl(count_or_buf, 0, 16),
        addr,
    )
}

pub fn duplicate(num_copies: u32, src: u32, dst: u32) -> Acmd {
    (
        shl(A_DUPLICATE, 24, 8) | shl(num_copies, 16, 8) | shl(src, 0, 16),
        shl(dst, 16, 16) | shl(0x80, 0, 16),
    )
}

pub fn add_mixer(count: u32, dmemi: u32, dmemo: u32, a4: u32) -> Acmd {
    (
        shl(A_ADDMIXER, 24, 8) | shl(count >> 4, 16, 8) | shl(a4, 0, 16),
        shl(dmemi, 16, 16) | shl(dmemo, 0, 16),
    )
}

pub fn resample_zoh(pitch: u32, pitch_accu: u32) -> Acmd {
    (
        shl(A_RESAMPLE_ZOH, 24, 8) | shl(pitch, 0, 16),
        shl(pitch_accu, 0, 16),
    )
}

pub fn s8_dec(f: u32, state: u32) -> Acmd {
    (shl(A_S8DEC, 24, 8) | shl(f, 16, 8), state)
}

pub fn hilo_gain(gain: u32, dmem_in: u32, dmem_out: u32, size: u32) -> Acmd {
    (
        shl(A_HILOGAIN, 24, 8) | shl(gain, 16, 8) | shl(size, 0, 16),
        shl(dmem_in, 16, 16) | shl(dmem_out, 0, 16),
    )
}

pub fn unk_cmd3(a1: u32, a2: u32, size: u32) -> Acmd {
    (
        shl(A_UNK3, 24, 8) | shl(size, 0, 16),
        shl(a1, 16, 16) | shl(a2, 0, 16),
    )
}

pub fn unk_cmd19(a1: u32, a2: u32, size: u32, a4: u32) -> Acmd {
    (
        shl(A_UNK19, 24, 8) | shl(a4, 16, 8) | shl(size, 0, 16),
        shl(a1, 16, 16) | shl(a2, 0, 16),
    )
}

#[rustfmt::skip]
const RESAMPLE_LUT: [i16; 256] = [
    0x0c39, 0x66ad, 0x0d46, -0x0021, 0x0b39, 0x6696, 0x0e5f, -0x0028,
    0x0a44, 0x6669, 0x0f83, -0x0030, 0x095a, 0x6626, 0x10b4, -0x0038,
    0x087d, 0x65cd, 0x11f0, -0x0041, 0x07ab, 0x655e, 0x1338, -0x004a,
    0x06e4, 0x64d9, 0x148c, -0x0054, 0x0628, 0x643f, 0x15eb, -0x005f,
    0x0577, 0x638f, 0x1756, -0x006a, 0x04d1, 0x62cb, 0x18cb, -0x0076,
    0x0435, 0x61f3, 0x1a4c, -0x0082, 0x03a4, 0x6106, 0x1bd7, -0x008f,
    0x031c, 0x6007, 0x1d6c, -0x009c, 0x029f, 0x5ef5, 0x1f0b, -0x00aa,
    0x022a, 0x5dd0, 0x20b3, -0x00b8, 0x01be, 0x5c9a, 0x2264, -0x00c6,
    0x015b, 0x5b53, 0x241e, -0x00d4, 0x0101, 0x59fc, 0x25e0, -0x00e2,
    0x00ae, 0x5896, 0x27a9, -0x00f0, 0x0063, 0x5720, 0x297a, -0x00fe,
    0x001f, 0x559d, 0x2b50, -0x010c, -0x001e, 0x540d, 0x2d2c, -0x0118,
    -0x0054, 0x5270, 0x2f0d, -0x0125, -0x0084, 0x50c7, 0x30f3, -0x0130,
    -0x00ad, 0x4f14, 0x32dc, -0x013a, -0x00d2, 0x4d57, 0x34c8, -0x0143,
    -0x00f1, 0x4b91, 0x36b6, -0x014a, -0x010b, 0x49c2, 0x38a5, -0x0150,
    -0x0121, 0x47ed, 0x3a95, -0x0154, -0x0132, 0x4611, 0x3c85, -0x0155,
    -0x0140, 0x4430, 0x3e74, -0x0154, -0x014a, 0x424a, 0x4060, -0x0151,
    -0x0151, 0x4060, 0x424a, -0x014a, -0x0154, 0x3e74, 0x4430, -0x0140,
    -0x0155, 0x3c85, 0x4611, -0x0132, -0x0154, 0x3a95, 0x47ed, -0x0121,
    -0x0150, 0x38a5, 0x49c2, -0x010b, -0x014a, 0x36b6, 0x4b91, -0x00f1,
    -0x0143, 0x34c8, 0x4d57, -0x00d2, -0x013a, 0x32dc, 0x4f14, -0x00ad,
    -0x0130, 0x30f3, 0x50c7, -0x0084, -0x0125, 0x2f0d, 0x5270, -0x0054,
    -0x0118, 0x2d2c, 0x540d, -0x001e, -0x010c, 0x2b50, 0x559d, 0x001f,
    -0x00fe, 0x297a, 0x5720, 0x0063, -0x00f0, 0x27a9, 0x5896, 0x00ae,
    -0x00e2, 0x25e0, 0x59fc, 0x0101, -0x00d4, 0x241e, 0x5b53, 0x015b,
    -0x00c6, 0x2264, 0x5c9a, 0x01be, -0x00b8, 0x20b3, 0x5dd0, 0x022a,
    -0x00aa, 0x1f0b, 0x5ef5, 0x029f, -0x009c, 0x1d6c, 0x6007, 0x031c,
    -0x008f, 0x1bd7, 0x6106, 0x03a4, -0x0082, 0x1a4c, 0x61f3, 0x0435,
    -0x0076, 0x18cb, 0x62cb, 0x04d1, -0x006a, 0x1756, 0x638f, 0x0577,
    -0x005f, 0x15eb, 0x643f, 0x0628, -0x0054, 0x148c, 0x64d9, 0x06e4,
    -0x004a, 0x1338, 0x655e, 0x07ab, -0x0041, 0x11f0, 0x65cd, 0x087d,
    -0x0038, 0x10b4, 0x6626, 0x095a, -0x0030, 0x0f83, 0x6669, 0x0a44,
    -0x0028, 0x0e5f, 0x6696, 0x0b39, -0x0021, 0x0d46, 0x66ad, 0x0c39,
];

fn clamp16(x: i64) -> i16 {
    x.clamp(i16::MIN as i64, i16::MAX as i64) as i16
}

/// The RSP's 4 KB data memory plus the microcode's persistent registers.
pub struct Rsp {
    dmem: Box<[u8; 0x1000]>,
    in_: u16,
    out: u16,
    count: u16,
    loop_addr: u32,
    table: [i16; 128],
    env_values: [u16; 3],
    env_steps: [u16; 3],
    filter_count: u16,
    filter_lut: [u32; 2],
}

impl Default for Rsp {
    fn default() -> Rsp {
        Rsp::new()
    }
}

impl Rsp {
    pub fn new() -> Rsp {
        Rsp {
            dmem: Box::new([0; 0x1000]),
            in_: 0,
            out: 0,
            count: 0,
            loop_addr: 0,
            table: [0; 128],
            env_values: [0; 3],
            env_steps: [0; 3],
            filter_count: 0,
            filter_lut: [0; 2],
        }
    }

    fn u8(&self, a: u16) -> u8 {
        self.dmem[(a & 0xFFF) as usize]
    }

    fn set_u8(&mut self, a: u16, v: u8) {
        self.dmem[(a & 0xFFF) as usize] = v;
    }

    /// Sample at byte address `a` (rounded down to an even address).
    fn s16(&self, a: u16) -> i16 {
        let a = (a & 0xFFE) as usize;
        i16::from_be_bytes([self.dmem[a], self.dmem[a + 1]])
    }

    fn set_s16(&mut self, a: u16, v: i16) {
        let a = (a & 0xFFE) as usize;
        self.dmem[a..a + 2].copy_from_slice(&v.to_be_bytes());
    }

    /// Sample by index (the resampler works in sample positions).
    fn sample(&self, pos: u16) -> i16 {
        self.s16(pos.wrapping_mul(2))
    }

    fn set_sample(&mut self, pos: u16, v: i16) {
        self.set_s16(pos.wrapping_mul(2), v);
    }

    pub fn run(&mut self, ram: &mut Ram, cmds: &[Acmd]) {
        for &(w1, w2) in cmds {
            self.exec(ram, w1, w2);
        }
    }

    fn exec(&mut self, ram: &mut Ram, w1: u32, w2: u32) {
        match (w1 >> 24) & 0x7F {
            A_ADPCM => {
                let flags = (w1 >> 16) as u8;
                let count = (self.count.wrapping_add(0x1F)) & !0x1F;
                self.adpcm(
                    ram,
                    flags & 1 != 0,
                    flags & 2 != 0,
                    flags & 4 != 0,
                    count,
                    w2,
                );
            }
            A_CLEARBUFF => {
                let dmem = w1 as u16;
                let count = (w2 & 0xFFF) as u16;
                for i in 0..count {
                    self.set_u8(dmem.wrapping_add(i), 0);
                }
            }
            A_ADDMIXER => {
                let count = ((w1 >> 12) & 0xFF0) as u16;
                let dmemi = (w2 >> 16) as u16;
                let dmemo = w2 as u16;
                for i in 0..count / 2 {
                    let d = self.s16(dmemo + 2 * i) as i64 + self.s16(dmemi + 2 * i) as i64;
                    self.set_s16(dmemo + 2 * i, clamp16(d));
                }
            }
            A_RESAMPLE => {
                let flags = (w1 >> 16) as u8;
                let pitch = (w1 as u16 as u32) << 1;
                let count = (self.count.wrapping_add(0xF)) & !0xF;
                self.resample(ram, flags & 1 != 0, self.out, self.in_, count, pitch, w2);
            }
            A_RESAMPLE_ZOH => {
                let pitch = (w1 as u16 as u32) << 1;
                let mut accu = w2 & 0xFFFF;
                let mut ipos = self.in_ >> 1;
                let mut opos = self.out >> 1;
                for _ in 0..self.count >> 1 {
                    let s = self.sample(ipos);
                    self.set_sample(opos, s);
                    opos = opos.wrapping_add(1);
                    accu += pitch;
                    ipos = ipos.wrapping_add((accu >> 16) as u16);
                    accu &= 0xFFFF;
                }
            }
            A_FILTER => {
                let flags = (w1 >> 16) as u8;
                if flags > 1 {
                    self.filter_count = w1 as u16;
                    self.filter_lut[0] = w2;
                } else {
                    self.filter_lut[1] = w2.wrapping_add(0x10);
                    self.filter(ram, w1 as u16, w2);
                }
            }
            A_SETBUFF => {
                self.in_ = w1 as u16;
                self.out = (w2 >> 16) as u16;
                self.count = w2 as u16;
            }
            A_DUPLICATE => {
                let count = (w1 >> 16) as u8;
                let dmemi = w1 as u16;
                let mut dmemo = (w2 >> 16) as u16;
                let mut buf = [0u8; 128];
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = self.u8(dmemi + i as u16);
                }
                for _ in 0..count {
                    for (i, &b) in buf.iter().enumerate() {
                        self.set_u8(dmemo + i as u16, b);
                    }
                    dmemo = dmemo.wrapping_add(128);
                }
            }
            A_DMEMMOVE => {
                let dmemi = w1 as u16;
                let dmemo = (w2 >> 16) as u16;
                let count = w2 as u16;
                if count != 0 {
                    let count = (count + 3) & !3;
                    for i in 0..count {
                        let b = self.u8(dmemi.wrapping_add(i));
                        self.set_u8(dmemo.wrapping_add(i), b);
                    }
                }
            }
            A_LOADADPCM => {
                let count = (w1 as u16 >> 1) as usize;
                for i in 0..count.min(self.table.len()) {
                    self.table[i] = ram.s16(w2 + 2 * i as u32);
                }
            }
            A_MIXER => {
                let count = ((w1 >> 12) & 0xFF0) as u16;
                let gain = w1 as i16 as i64;
                let dmemi = (w2 >> 16) as u16;
                let dmemo = w2 as u16;
                for i in 0..count / 2 {
                    let src = self.s16(dmemi + 2 * i) as i64;
                    let dst = self.s16(dmemo + 2 * i) as i64;
                    self.set_s16(dmemo + 2 * i, clamp16(dst + ((src * gain) >> 15)));
                }
            }
            A_INTERLEAVE => {
                let count = ((w1 >> 12) & 0xFF0) as u16;
                let dmemo = w1 as u16;
                let left = (w2 >> 16) as u16;
                let right = w2 as u16;
                // Interleave into a copy first: the output may overlap the inputs.
                let n = count / 2;
                let mut out = Vec::with_capacity(2 * n as usize);
                for i in 0..n {
                    out.push(self.s16(left + 2 * i));
                    out.push(self.s16(right + 2 * i));
                }
                for (i, v) in out.into_iter().enumerate() {
                    self.set_s16(dmemo + 2 * i as u16, v);
                }
            }
            A_HILOGAIN => {
                let gain = (w1 >> 16) as u8 as i8 as i64;
                let count = (w1 & 0xFFF) as u16;
                let dmem = (w2 >> 16) as u16;
                for i in 0..count / 2 {
                    let v = self.s16(dmem + 2 * i) as i64;
                    self.set_s16(dmem + 2 * i, clamp16((v * gain) >> 4));
                }
            }
            A_SETLOOP => self.loop_addr = w2,
            A_INTERL => {
                let count = w1 as u16;
                let dmemi = (w2 >> 16) as u16;
                let dmemo = w2 as u16;
                for i in 0..count {
                    let v = self.s16(dmemi.wrapping_add(4 * i));
                    self.set_s16(dmemo.wrapping_add(2 * i), v);
                }
            }
            A_ENVSETUP1 => {
                self.env_values[2] = ((w1 >> 8) & 0xFF00) as u16;
                self.env_steps[2] = w1 as u16;
                self.env_steps[0] = (w2 >> 16) as u16;
                self.env_steps[1] = w2 as u16;
            }
            A_ENVMIXER => {
                let dmemi = ((w1 >> 12) & 0xFF0) as u16;
                let count = (w1 >> 8) as u8 as u16;
                let swap_wet = (w1 >> 4) & 1 != 0;
                let dl = ((w2 >> 20) & 0xFF0) as u16;
                let dr = ((w2 >> 12) & 0xFF0) as u16;
                let mut wl = ((w2 >> 4) & 0xFF0) as u16;
                let mut wr = ((w2 << 4) & 0xFF0) as u16;
                // As in the reference; the wet masks come out as 0xFFFC/0xFFFE
                // rather than 0xFFFF, a difference of a few LSBs.
                let xors = [
                    -(((w1 & 0x2) >> 1) as i16),
                    -((w1 & 0x1) as i16),
                    -(((w1 & 0x8) >> 1) as i16),
                    -(((w1 & 0x4) >> 1) as i16),
                ];
                if swap_wet {
                    std::mem::swap(&mut wl, &mut wr);
                }
                self.envmix(dl, dr, wl, wr, dmemi, count, xors);
            }
            A_LOADBUFF => {
                let count = ((w1 >> 12) & 0xFFF) as u16;
                let dmem = (w1 & 0xFFF) as u16 & !3;
                let addr = w2 & !7;
                let count = (count + 7) & !7;
                for i in 0..count {
                    self.set_u8(dmem + i, ram.u8(addr + i as u32));
                }
            }
            A_SAVEBUFF => {
                let count = ((w1 >> 12) & 0xFFF) as u16;
                let dmem = (w1 & 0xFFF) as u16 & !3;
                let addr = w2 & !7;
                let count = (count + 7) & !7;
                for i in 0..count {
                    ram.set_u8(addr + i as u32, self.u8(dmem + i));
                }
            }
            A_ENVSETUP2 => {
                self.env_values[0] = (w2 >> 16) as u16;
                self.env_values[1] = w2 as u16;
            }
            A_S8DEC => {
                // Not implemented by the HLE reference, and unused by vanilla
                // data. Decode 8-bit PCM the obvious way so custom samples in
                // this codec are audible rather than silent.
                let count = (self.count.wrapping_add(0x1F)) & !0x1F;
                let (mut i, mut o) = (self.in_, self.out);
                for _ in 0..16 {
                    self.set_s16(o, 0);
                    o += 2;
                }
                for _ in 0..count / 2 {
                    let v = (self.u8(i) as i8 as i16) << 8;
                    self.set_s16(o, v);
                    i += 1;
                    o += 2;
                }
                let _ = w2;
            }
            // A_SPNOOP, A_UNK3, A_UNK19 and anything else: no effect here.
            _ => {}
        }
    }

    fn adpcm(
        &mut self,
        ram: &mut Ram,
        init: bool,
        looping: bool,
        two_bit: bool,
        count: u16,
        state: u32,
    ) {
        let mut last = [0i16; 16];
        if !init {
            let from = if looping { self.loop_addr } else { state };
            for (i, s) in last.iter_mut().enumerate() {
                *s = ram.s16(from + 2 * i as u32);
            }
        }
        let mut dmemo = self.out;
        let mut dmemi = self.in_;
        for &s in &last {
            self.set_s16(dmemo, s);
            dmemo = dmemo.wrapping_add(2);
        }
        let mut remaining = count;
        while remaining != 0 {
            let code = self.u8(dmemi);
            dmemi = dmemi.wrapping_add(1);
            let scale = (code >> 4) as u32;
            let cb = ((code & 0xF) as usize) << 4;
            let mut frame = [0i16; 16];
            if two_bit {
                let rshift = 14u32.saturating_sub(scale);
                for k in 0..4 {
                    let byte = self.u8(dmemi);
                    dmemi = dmemi.wrapping_add(1);
                    frame[4 * k] = predict(byte, 0xC0, 8, rshift);
                    frame[4 * k + 1] = predict(byte, 0x30, 10, rshift);
                    frame[4 * k + 2] = predict(byte, 0x0C, 12, rshift);
                    frame[4 * k + 3] = predict(byte, 0x03, 14, rshift);
                }
            } else {
                let rshift = 12u32.saturating_sub(scale);
                for k in 0..8 {
                    let byte = self.u8(dmemi);
                    dmemi = dmemi.wrapping_add(1);
                    frame[2 * k] = predict(byte, 0xF0, 8, rshift);
                    frame[2 * k + 1] = predict(byte, 0x0F, 12, rshift);
                }
            }
            let book: [i16; 16] =
                std::array::from_fn(|i| self.table.get(cb + i).copied().unwrap_or(0));
            let (l1, l2) = (last[14], last[15]);
            residuals(&mut last[..8], &frame[..8], &book, l1, l2);
            let (l1, l2) = (last[6], last[7]);
            residuals(&mut last[8..], &frame[8..], &book, l1, l2);
            for &s in &last {
                self.set_s16(dmemo, s);
                dmemo = dmemo.wrapping_add(2);
            }
            remaining = remaining.saturating_sub(32);
        }
        for (i, &s) in last.iter().enumerate() {
            ram.set_u16(state + 2 * i as u32, s as u16);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn resample(
        &mut self,
        ram: &mut Ram,
        init: bool,
        dmemo: u16,
        dmemi: u16,
        count: u16,
        pitch: u32,
        state: u32,
    ) {
        let mut ipos = (dmemi >> 1).wrapping_sub(4);
        let mut opos = dmemo >> 1;
        let mut accu: u32;
        if init {
            for k in 0..4 {
                self.set_sample(ipos + k, 0);
            }
            accu = 0;
        } else {
            for k in 0..4 {
                self.set_sample(ipos + k, ram.s16(state + 2 * k as u32));
            }
            accu = ram.u16(state + 8) as u32;
        }
        for _ in 0..count >> 1 {
            let lut = ((accu & 0xFC00) >> 8) as usize;
            let mut v = 0i64;
            for k in 0..4 {
                v += self.sample(ipos + k as u16) as i64 * RESAMPLE_LUT[lut + k] as i64;
            }
            self.set_sample(opos, clamp16(v >> 15));
            opos = opos.wrapping_add(1);
            accu += pitch;
            ipos = ipos.wrapping_add((accu >> 16) as u16);
            accu &= 0xFFFF;
        }
        for k in 0..4 {
            ram.set_u16(state + 2 * k as u32, self.sample(ipos + k as u16) as u16);
        }
        ram.set_u16(state + 8, accu as u16);
    }

    #[allow(clippy::too_many_arguments)]
    fn envmix(
        &mut self,
        dl: u16,
        dr: u16,
        wl: u16,
        wr: u16,
        dmemi: u16,
        count: u16,
        xors: [i16; 4],
    ) {
        let mut count = (count + 7) & !7;
        let mut off = 0u16;
        while count != 0 {
            for i in 0..8u16 {
                let at = off + 2 * i;
                let input = self.s16(dmemi + at) as i32;
                let l = (((input * self.env_values[0] as i32) >> 16) as i16) ^ xors[0];
                let r = (((input * self.env_values[1] as i32) >> 16) as i16) ^ xors[1];
                let l2 = (((l as i32 * self.env_values[2] as i32) >> 16) as i16) ^ xors[2];
                let r2 = (((r as i32 * self.env_values[2] as i32) >> 16) as i16) ^ xors[3];
                let v = clamp16(self.s16(dl + at) as i64 + l as i64);
                self.set_s16(dl + at, v);
                let v = clamp16(self.s16(dr + at) as i64 + r as i64);
                self.set_s16(dr + at, v);
                let v = clamp16(self.s16(wl + at) as i64 + l2 as i64);
                self.set_s16(wl + at, v);
                let v = clamp16(self.s16(wr + at) as i64 + r2 as i64);
                self.set_s16(wr + at, v);
            }
            for k in 0..3 {
                self.env_values[k] = self.env_values[k].wrapping_add(self.env_steps[k]);
            }
            off += 16;
            count -= 8;
        }
    }

    fn filter(&mut self, ram: &mut Ram, dmem: u16, address: u32) {
        let count = self.filter_count;
        let lut6_at = self.filter_lut[0];
        let lut5_at = self.filter_lut[1];
        // The microcode averages the two coefficient sets and writes the result
        // back to both, in RAM.
        let mut lut6 = [0i32; 8];
        for (x, c) in lut6.iter_mut().enumerate() {
            let a = ram.s16(lut5_at + 2 * x as u32) as i32;
            let b = ram.s16(lut6_at + 2 * x as u32) as i32;
            let v = (a + b) >> 1;
            ram.set_u16(lut5_at + 2 * x as u32, v as u16);
            ram.set_u16(lut6_at + 2 * x as u32, v as u16);
            *c = v as i16 as i32;
        }
        // in1 starts as the 8 saved samples in RAM, then walks the DMEM input.
        let mut prev: [i32; 8] = std::array::from_fn(|i| ram.s16(address + 2 * i as u32) as i32);
        let n_blocks = count.div_ceil(16) as usize;
        let mut out = Vec::with_capacity(n_blocks * 8);
        let mut in2_at = dmem;
        let mut last = prev;
        for _ in 0..n_blocks {
            let in1 = prev;
            let in2: [i32; 8] = std::array::from_fn(|i| self.s16(in2_at + 2 * i as u16) as i32);
            let c = &lut6;
            let mut v = [0i32; 8];
            v[1] = in1[0] * c[6]
                + in1[3] * c[7]
                + in1[2] * c[4]
                + in1[5] * c[5]
                + in1[4] * c[2]
                + in1[7] * c[3]
                + in1[6] * c[0]
                + in2[1] * c[1];
            v[0] = in1[3] * c[6]
                + in1[2] * c[7]
                + in1[5] * c[4]
                + in1[4] * c[5]
                + in1[7] * c[2]
                + in1[6] * c[3]
                + in2[1] * c[0]
                + in2[0] * c[1];
            v[3] = in1[2] * c[6]
                + in1[5] * c[7]
                + in1[4] * c[4]
                + in1[7] * c[5]
                + in1[6] * c[2]
                + in2[1] * c[3]
                + in2[0] * c[0]
                + in2[3] * c[1];
            v[2] = in1[5] * c[6]
                + in1[4] * c[7]
                + in1[7] * c[4]
                + in1[6] * c[5]
                + in2[1] * c[2]
                + in2[0] * c[3]
                + in2[3] * c[0]
                + in2[2] * c[1];
            v[5] = in1[4] * c[6]
                + in1[7] * c[7]
                + in1[6] * c[4]
                + in2[1] * c[5]
                + in2[0] * c[2]
                + in2[3] * c[3]
                + in2[2] * c[0]
                + in2[5] * c[1];
            v[4] = in1[7] * c[6]
                + in1[6] * c[7]
                + in2[1] * c[4]
                + in2[0] * c[5]
                + in2[3] * c[2]
                + in2[2] * c[3]
                + in2[5] * c[0]
                + in2[4] * c[1];
            v[7] = in1[6] * c[6]
                + in2[1] * c[7]
                + in2[0] * c[4]
                + in2[3] * c[5]
                + in2[2] * c[2]
                + in2[5] * c[3]
                + in2[4] * c[0]
                + in2[7] * c[1];
            v[6] = in2[1] * c[6]
                + in2[0] * c[7]
                + in2[3] * c[4]
                + in2[2] * c[5]
                + in2[5] * c[2]
                + in2[4] * c[3]
                + in2[7] * c[0]
                + in2[6] * c[1];
            for x in v {
                out.push(((x + 0x4000) >> 15) as i16);
            }
            prev = in2;
            last = in2;
            in2_at = in2_at.wrapping_add(16);
        }
        // Save the last 8 input samples for the next call.
        for (i, &s) in last.iter().enumerate() {
            ram.set_u16(address + 2 * i as u32, s as u16);
        }
        // The reference copies `count` bytes of output back.
        for i in 0..(count as usize / 2).min(out.len()) {
            self.set_s16(dmem + 2 * i as u16, out[i]);
        }
    }

    /// Read `n` interleaved stereo frames that `save_buffer` wrote to RAM.
    pub fn read_output(ram: &Ram, addr: u32, frames: usize, out: &mut Vec<[i16; 2]>) {
        for i in 0..frames as u32 {
            out.push([ram.s16(addr + 4 * i), ram.s16(addr + 4 * i + 2)]);
        }
    }
}

fn predict(byte: u8, mask: u8, lshift: u32, rshift: u32) -> i16 {
    let sample = (((byte & mask) as u16) << lshift) as i16;
    sample >> rshift
}

fn residuals(dst: &mut [i16], src: &[i16], book: &[i16; 16], l1: i16, l2: i16) {
    let (book1, book2) = book.split_at(8);
    for i in 0..8 {
        let mut accu = (src[i] as i32) << 11;
        accu += book1[i] as i32 * l1 as i32 + book2[i] as i32 * l2 as i32;
        // rdot(i, book2, src): sum of book2[k] * src[i-1-k]
        for k in 0..i {
            accu += book2[k] as i32 * src[i - 1 - k] as i32;
        }
        dst[i] = clamp16((accu >> 11) as i64);
    }
}
