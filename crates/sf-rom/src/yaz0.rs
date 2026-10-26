//! Yaz0 decompression (the compression used for files in retail OoT/MM ROMs).

/// Decompress a Yaz0 stream. Returns `None` if the header is missing or the
/// stream ends early.
pub fn decompress(src: &[u8]) -> Option<Vec<u8>> {
    if src.len() < 16 || &src[..4] != b"Yaz0" {
        return None;
    }
    let size = u32::from_be_bytes(src[4..8].try_into().ok()?) as usize;
    let mut out = Vec::with_capacity(size);
    let mut pos = 16;
    while out.len() < size {
        let header = *src.get(pos)?;
        pos += 1;
        for bit in (0..8).rev() {
            if out.len() >= size {
                break;
            }
            if header & (1 << bit) != 0 {
                out.push(*src.get(pos)?);
                pos += 1;
            } else {
                let b0 = *src.get(pos)? as usize;
                let b1 = *src.get(pos + 1)? as usize;
                pos += 2;
                let dist = ((b0 & 0xF) << 8 | b1) + 1;
                let len = if b0 >> 4 == 0 {
                    let n = *src.get(pos)? as usize + 0x12;
                    pos += 1;
                    n
                } else {
                    (b0 >> 4) + 2
                };
                let start = out.len().checked_sub(dist)?;
                // Copies may overlap their own output, so go byte by byte.
                for i in 0..len {
                    let b = out[start + i];
                    out.push(b);
                }
            }
        }
    }
    out.truncate(size);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_and_back_reference() {
        // "abcabcabc": 3 literals, then a 6-byte copy from distance 3.
        let mut src = b"Yaz0".to_vec();
        src.extend_from_slice(&9u32.to_be_bytes());
        src.extend_from_slice(&[0; 8]);
        src.push(0b1110_0000);
        src.extend_from_slice(b"abc");
        src.extend_from_slice(&[0x40, 0x02]); // len 4+2=6, dist 2+1=3
        assert_eq!(decompress(&src).unwrap(), b"abcabcabc");
    }
}
