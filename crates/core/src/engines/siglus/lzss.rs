//! SiglusEngine's LZSS: `[u32 packed size, header included][u32 unpacked
//! size]`, then groups of a flag byte and eight items. A set flag bit is a
//! literal byte; a clear one a little-endian u16 `(distance << 4) | (len - 2)`
//! copying 2-17 bytes from 1-4095 bytes back.
//!
//! Format as read by siglus_rs (xmoezzz, MPL-2.0) and written by SiglusExtract.

use crate::error::{Error, Result};

/// Nothing Yakusu reads (a scene, a whole Scene.pck) is anywhere near this.
pub const MAX_SCENE_BYTES: usize = 64 * 1024 * 1024;

const MIN_MATCH: usize = 2;
const MAX_MATCH: usize = 17;
const WINDOW: usize = 4095;

fn bad(msg: impl Into<String>) -> Error {
    Error::User(format!("A SiglusEngine scene couldn't be unpacked ({}).", msg.into()))
}

/// The unpacked size a packed buffer says it has.
pub fn unpacked_size(src: &[u8]) -> Option<usize> {
    Some(u32::from_le_bytes(src.get(4..8)?.try_into().ok()?) as usize)
}

pub fn unpack(src: &[u8]) -> Result<Vec<u8>> {
    if src.len() < 8 {
        return Err(bad("too short"));
    }
    let packed = u32::from_le_bytes(src[0..4].try_into().unwrap()) as usize;
    let size = u32::from_le_bytes(src[4..8].try_into().unwrap()) as usize;
    if size > MAX_SCENE_BYTES {
        return Err(bad(format!("it claims {size} bytes")));
    }
    if packed < 8 {
        return Err(bad("bad header"));
    }
    // A packed size past the end is tolerated (as the engine does); the data
    // must still unpack to exactly the size it claims.
    let end = packed.min(src.len());
    let mut out = Vec::with_capacity(size.min(src.len().saturating_mul(18)));
    let mut pos = 8;
    while out.len() < size && pos < end {
        let flags = src[pos];
        pos += 1;
        for bit in 0..8 {
            if out.len() >= size || pos >= end {
                break;
            }
            if flags >> bit & 1 != 0 {
                out.push(src[pos]);
                pos += 1;
            } else {
                if pos + 2 > end {
                    return Err(bad("cut off"));
                }
                let token = u16::from_le_bytes([src[pos], src[pos + 1]]) as usize;
                pos += 2;
                let dist = token >> 4;
                let len = (token & 0xF) + MIN_MATCH;
                if dist == 0 || dist > out.len() {
                    return Err(bad("a back-reference points outside the data"));
                }
                let from = out.len() - dist;
                for k in 0..len {
                    if out.len() >= size {
                        break;
                    }
                    out.push(out[from + k]);
                }
            }
        }
    }
    if out.len() != size {
        return Err(bad(format!("{} of {size} bytes", out.len())));
    }
    Ok(out)
}

/// Pack `data`. Matches are found through a hash of the next three bytes
/// with a bounded chain, so big scenes pack in milliseconds.
pub fn pack(data: &[u8]) -> Vec<u8> {
    const HASH_BITS: usize = 14;
    const CHAIN: usize = 48;
    let hash = |i: usize| -> usize {
        let v = (data[i] as usize) << 16 | (data[i + 1] as usize) << 8 | data[i + 2] as usize;
        (v.wrapping_mul(2654435761) >> 8) & ((1 << HASH_BITS) - 1)
    };
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; data.len()];
    let insert = |i: usize, head: &mut Vec<usize>, prev: &mut Vec<usize>| {
        if i + 2 < data.len() {
            let h = hash(i);
            prev[i] = head[h];
            head[h] = i;
        }
    };

    let mut out = vec![0u8; 8];
    let mut i = 0;
    while i < data.len() {
        let flag_at = out.len();
        out.push(0);
        let mut flags = 0u8;
        for bit in 0..8 {
            if i >= data.len() {
                break;
            }
            // Longest match within the window.
            let (mut best_len, mut best_dist) = (0usize, 0usize);
            let max_len = MAX_MATCH.min(data.len() - i);
            if max_len >= MIN_MATCH {
                if i + 2 < data.len() {
                    let mut cand = head[hash(i)];
                    let mut steps = 0;
                    while cand != usize::MAX && i - cand <= WINDOW && steps < CHAIN {
                        let mut l = 0;
                        while l < max_len && data[cand + l] == data[i + l] {
                            l += 1;
                        }
                        if l > best_len {
                            best_len = l;
                            best_dist = i - cand;
                            if l == max_len {
                                break;
                            }
                        }
                        cand = prev[cand];
                        steps += 1;
                    }
                }
                // Two-byte matches the hash can't see: the byte just before.
                if best_len < MIN_MATCH && i >= 1 && data[i - 1] == data[i] && data.get(i + 1) == Some(&data[i]) {
                    best_len = MIN_MATCH;
                    best_dist = 1;
                }
            }
            if best_len >= MIN_MATCH {
                let token = (best_dist << 4 | (best_len - MIN_MATCH)) as u16;
                out.extend_from_slice(&token.to_le_bytes());
                for k in 0..best_len {
                    insert(i + k, &mut head, &mut prev);
                }
                i += best_len;
            } else {
                flags |= 1 << bit;
                out.push(data[i]);
                insert(i, &mut head, &mut prev);
                i += 1;
            }
        }
        out[flag_at] = flags;
    }
    let packed = out.len() as u32;
    out[0..4].copy_from_slice(&packed.to_le_bytes());
    out[4..8].copy_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let mut rnd = 0x1234_5678u32;
        let random: Vec<u8> = (0..20_000)
            .map(|_| {
                rnd ^= rnd << 13;
                rnd ^= rnd >> 17;
                rnd ^= rnd << 5;
                rnd as u8
            })
            .collect();
        let text: Vec<u8> =
            "「おはよう」と彼女は言った。".repeat(400).encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        for data in [vec![], vec![7], vec![0; 5000], b"abababababab".to_vec(), random, text] {
            let p = pack(&data);
            assert_eq!(unpack(&p).unwrap(), data);
            assert_eq!(unpacked_size(&p), Some(data.len()));
        }
    }
}
