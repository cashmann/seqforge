//! A flat big-endian address space standing in for the console's RDRAM.
//!
//! The engine keeps everything the game keeps in RAM here, at real addresses:
//! sequence data, relocated soundfonts, sample banks, envelopes, filters and the
//! RSP's state buffers. Pointers in the engine are `u32` addresses into it, so
//! the game's pointer tricks (sequences writing into their own data, filters
//! the microcode rewrites in place, envelopes that live inside a sequence)
//! behave exactly as on hardware. Addresses are handed out in KSEG0
//! (`0x8000_0000 | offset`) like the game's, so its "is this still an offset or
//! already a pointer" checks during soundfont relocation work unchanged.
//! Address 0 is never handed out, so it can stand for NULL.

pub const KSEG0: u32 = 0x8000_0000;

fn phys(a: u32) -> usize {
    (a & 0x7FFF_FFFF) as usize
}

pub struct Ram {
    mem: Vec<u8>,
}

impl Ram {
    pub fn new() -> Ram {
        Ram {
            mem: vec![0; 0x100],
        }
    }

    /// Bump-allocate `size` zeroed bytes aligned to `align`.
    pub fn alloc(&mut self, size: usize, align: usize) -> u32 {
        let at = self.mem.len().next_multiple_of(align);
        self.mem.resize(at + size.max(1), 0);
        at as u32 | KSEG0
    }

    pub fn alloc_bytes(&mut self, data: &[u8], align: usize) -> u32 {
        let at = self.alloc(data.len(), align);
        self.write(at, data);
        at
    }

    pub fn len(&self) -> usize {
        self.mem.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mem.is_empty()
    }

    /// Reads past the end return zeros and writes past the end are dropped,
    /// like open bus. Garbage sequences must not crash the player.
    pub fn u8(&self, a: u32) -> u8 {
        self.mem.get(phys(a)).copied().unwrap_or(0)
    }

    pub fn set_u8(&mut self, a: u32, v: u8) {
        if let Some(b) = self.mem.get_mut(phys(a)) {
            *b = v;
        }
    }

    pub fn u16(&self, a: u32) -> u16 {
        (self.u8(a) as u16) << 8 | self.u8(a.wrapping_add(1)) as u16
    }

    pub fn s16(&self, a: u32) -> i16 {
        self.u16(a) as i16
    }

    pub fn set_u16(&mut self, a: u32, v: u16) {
        self.set_u8(a, (v >> 8) as u8);
        self.set_u8(a.wrapping_add(1), v as u8);
    }

    pub fn u32(&self, a: u32) -> u32 {
        (self.u16(a) as u32) << 16 | self.u16(a.wrapping_add(2)) as u32
    }

    pub fn set_u32(&mut self, a: u32, v: u32) {
        self.set_u16(a, (v >> 16) as u16);
        self.set_u16(a.wrapping_add(2), v as u16);
    }

    pub fn f32(&self, a: u32) -> f32 {
        f32::from_bits(self.u32(a))
    }

    pub fn write(&mut self, a: u32, data: &[u8]) {
        let a = phys(a);
        if a >= self.mem.len() {
            return;
        }
        let n = data.len().min(self.mem.len() - a);
        self.mem[a..a + n].copy_from_slice(&data[..n]);
    }

    pub fn read_into(&self, a: u32, out: &mut [u8]) {
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.u8(a.wrapping_add(i as u32));
        }
    }
}

impl Default for Ram {
    fn default() -> Ram {
        Ram::new()
    }
}
