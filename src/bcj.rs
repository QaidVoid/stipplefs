//! The x86 BCJ pre-filter, ported from xz's `simple/x86.c` (0BSD, Igor
//! Pavlov and Lasse Collin, after Igor Pavlov's LZMA SDK).
//!
//! x86 and x86-64 code addresses relative call and jump targets with a
//! signed 32-bit displacement that changes whenever the code moves. Replacing
//! each displacement with the absolute target makes the operands across a
//! binary nearly uniform, which is what lets the compressor find long
//! matches between functions and libraries that happen to be laid out
//! differently. The transform is its own inverse family: encoding converts
//! relative to absolute, decoding absolute back to relative, and both make
//! exactly the same decisions on the same bytes.
//!
//! One call covers one compression unit, a block. State does not carry
//! between blocks and positions are block-relative, so a block decodes
//! without knowing where it sits in the stream. A candidate opcode needs four
//! operand bytes after it, so the final four bytes of a block are rewritten
//! only when the opcode sits at the fifth byte from the end; both directions
//! make that same decision.

/// Whether a byte can end an address, for the filter's overlap heuristic.
const fn is_ms_byte(b: u8) -> bool {
    b == 0 || b == 0xFF
}

/// How many of the low mask bits name the byte to fix up.
const MASK_TO_BIT_NUMBER: [u32; 5] = [0, 1, 2, 2, 3];

/// Convert call and jump operands in `buf` in place.
///
/// With `encode` set, relative displacements become absolute targets.
/// Otherwise absolute targets become relative displacements. Applying
/// `convert(encode)` and then `convert(decode)` returns the original bytes.
pub fn convert(buf: &mut [u8], encode: bool) {
    let size = buf.len();
    if size < 5 {
        return;
    }

    let mut prev_mask: u32 = 0;
    let mut prev_pos: u32 = 0u32.wrapping_sub(5);
    let limit = size - 5;
    let mut pos: usize = 0;

    while pos <= limit {
        let opcode = buf[pos];
        if opcode != 0xE8 && opcode != 0xE9 {
            pos += 1;
            continue;
        }

        let offset = (pos as u32).wrapping_sub(prev_pos);
        prev_pos = pos as u32;

        if offset > 5 {
            prev_mask = 0;
        } else {
            for _ in 0..offset {
                prev_mask &= 0x77;
                prev_mask <<= 1;
            }
        }

        let tail = buf[pos + 4];

        if is_ms_byte(tail) && (prev_mask >> 1) <= 4 && (prev_mask >> 1) != 3 {
            let mut src = ((tail as u32) << 24)
                | ((buf[pos + 3] as u32) << 16)
                | ((buf[pos + 2] as u32) << 8)
                | (buf[pos + 1] as u32);

            let dest = loop {
                let d = if encode {
                    src.wrapping_add(pos as u32 + 5)
                } else {
                    src.wrapping_sub(pos as u32 + 5)
                };
                if prev_mask == 0 {
                    break d;
                }
                let i = MASK_TO_BIT_NUMBER[(prev_mask >> 1) as usize];
                let b = (d >> (24 - i * 8)) as u8;
                if !is_ms_byte(b) {
                    break d;
                }
                src = d ^ ((1u32 << (32 - i * 8)).wrapping_sub(1));
            };

            buf[pos + 4] = if (dest >> 24) & 1 == 1 { 0xFF } else { 0 };
            buf[pos + 3] = (dest >> 16) as u8;
            buf[pos + 2] = (dest >> 8) as u8;
            buf[pos + 1] = dest as u8;
            pos += 5;
            prev_mask = 0;
        } else {
            pos += 1;
            prev_mask |= 1;
            if is_ms_byte(tail) {
                prev_mask |= 0x10;
            }
        }
    }
}

/// Apply the encoder direction, relative to absolute.
pub fn encode(buf: &mut [u8]) {
    convert(buf, true);
}

/// Apply the decoder direction, absolute back to relative.
pub fn decode(buf: &mut [u8]) {
    convert(buf, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic corpus of bytes.
    fn corpus(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect()
    }

    /// x86-shaped data: runs of plausible opcodes followed by call or jump
    /// opcodes with small relative operands, like a real text segment.
    fn code(n: usize, seed: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        let mut s = seed;
        while out.len() < n {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let pick = (s >> 24) as u8;
            match pick % 8 {
                0..=1 => {
                    out.push(0x90);
                    out.push(0x31);
                    out.push(0xC0);
                }
                2 => {
                    out.push(0xE8);
                    let rel = (s as i32) % (1 << 20);
                    out.extend_from_slice(&rel.to_le_bytes());
                }
                3 => {
                    out.push(0xE9);
                    out.extend_from_slice(&(-(out.len() as i32)).to_le_bytes());
                }
                4 => {
                    out.push(0x48);
                    out.push(0x89);
                    out.push(0xD8);
                }
                5 => {
                    out.push(0x0F);
                    out.push(0x85);
                    out.extend_from_slice(&(0x0010_0000u32).to_le_bytes());
                }
                6 => out.push(0xC3),
                _ => {
                    out.extend_from_slice(b"/lib64/ld-linux-x86-64.so.2\0");
                }
            }
        }
        out.truncate(n);
        out
    }

    #[test]
    fn roundtrips_on_x86_shaped_input() {
        for seed in 1..20u64 {
            let mut data = code(100_000, seed);
            let original = data.clone();
            encode(&mut data);
            decode(&mut data);
            assert_eq!(data, original, "seed {seed}");
        }
    }

    #[test]
    fn roundtrips_on_arbitrary_bytes() {
        for seed in 1..50u64 {
            let mut data = corpus(50_000 + seed as usize * 37, seed);
            let original = data.clone();
            encode(&mut data);
            decode(&mut data);
            assert_eq!(data, original, "seed {seed}");
        }
    }

    #[test]
    fn roundtrips_short_inputs_where_nothing_is_written() {
        for n in 0..12 {
            let mut data = corpus(n, 3);
            let original = data.clone();
            encode(&mut data);
            decode(&mut data);
            assert_eq!(data, original, "length {n}");
        }
    }

    #[test]
    fn encoding_changes_call_operands() {
        let mut data = code(100_000, 9);
        let original = data.clone();
        encode(&mut data);
        let changed = data
            .windows(1)
            .zip(original.windows(1))
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 1_000, "only {changed} bytes changed");
    }

    #[test]
    fn encoding_is_deterministic() {
        let mut a = code(60_000, 4);
        let mut b = a.clone();
        encode(&mut a);
        encode(&mut b);
        assert_eq!(a, b);
    }
}
