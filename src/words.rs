//! A byte-oriented greedy text transform over the fixed v1 vocabulary.
include!(concat!(env!("OUT_DIR"), "/words.rs"));
pub static PRESET_V2: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/preset-v2.zdict"));
pub static HISTORY: &[u8] = PRESET_V2.split_at(HISTORY_START).1.split_at(HISTORY_LEN).0;

const SHORT: usize = 61;
const MAX_WORD: usize = 32;

fn word(rank: usize) -> &'static [u8] {
    &HISTORY[usize::from(WORD_OFFSET[rank + 1])..usize::from(WORD_OFFSET[rank])]
}

#[inline]
fn longest(input: &[u8]) -> Option<(usize, usize)> {
    if input.len() < 3 {
        return None;
    }
    let first = usize::from(input[0]);
    let offset = input[1].wrapping_sub(ROW_LOW[first]);
    if u16::from(offset) >= ROW_LEN[first] {
        return None;
    }
    let mut node = usize::from(HEAD[usize::from(ROW_BASE[first]) + usize::from(offset)]);
    let mut best = None;
    for (i, &byte) in input[2..].iter().enumerate() {
        let degree = usize::from(DEGREE[node]);
        if degree == 0 {
            break;
        }
        let start = usize::from(START[node]);
        let keys = &KEY[start..start + degree];
        let found = if degree < 8 {
            keys.iter().position(|&key| key == byte)
        } else {
            keys.binary_search(&byte).ok()
        };
        let Some(found) = found else { break };
        node = usize::from(TARGET[start + found]);
        let rank = usize::from(RANK[node]);
        if rank != 0 {
            best = Some((rank - 1, i + 3));
        }
    }
    best
}

#[inline]
fn scalar_len(lead: u8) -> usize {
    match lead {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    }
}

pub fn encode(input: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(input.len());
    let mut pos = 0;
    let mut literal_start = 0;
    while pos < input.len() {
        if let Some((rank, length)) = longest(&input[pos..]) {
            encoded.extend_from_slice(&input[literal_start..pos]);
            if rank < SHORT {
                encoded.push(0x80 + rank as u8);
            } else {
                let code = rank - SHORT;
                let page = (code >> 8) as u8;
                let lead = if page < 5 {
                    0xbd + page
                } else {
                    0xf5 + (page - 5)
                };
                encoded.extend_from_slice(&[lead, code as u8]);
            }
            pos += length;
            literal_start = pos;
        } else {
            pos += scalar_len(input[pos]);
        }
    }
    encoded.extend_from_slice(&input[literal_start..]);
    encoded
}

pub fn decode(input: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    if expected > input.len().saturating_mul(MAX_WORD) || expected < input.len() {
        return Err("impossible text block length".into());
    }
    let mut out = Vec::new();
    out.try_reserve_exact(expected)
        .map_err(|_| "text block too large")?;
    let mut pos = 0;
    while let Some(&lead) = input.get(pos) {
        pos += 1;
        let piece = match lead {
            0x00..=0x7f => {
                let start = pos - 1;
                while pos < input.len() && input[pos].is_ascii() {
                    pos += 1;
                }
                &input[start..pos]
            }
            0x80..=0xbc => word(usize::from(lead - 0x80)),
            0xc2..=0xf4 => {
                let start = pos - 1;
                let rest = &input[start..];
                // Token opcodes cannot start a UTF-8 scalar. The validator can
                // therefore locate the whole literal run, including following
                // ASCII, in one pass over multilingual text.
                let n = match std::str::from_utf8(rest) {
                    Ok(_) => rest.len(),
                    Err(e) if e.valid_up_to() != 0 => e.valid_up_to(),
                    Err(_) => return Err("invalid UTF-8 literal".into()),
                };
                pos = start + n;
                &rest[..n]
            }
            _ => {
                let page = if lead <= 0xc1 {
                    lead - 0xbd
                } else {
                    lead - 0xf5 + 5
                };
                let low = input.get(pos).ok_or("truncated extended text ID")?;
                pos += 1;
                let rank = SHORT + usize::from(page) * 256 + usize::from(*low);
                if rank >= WORD_OFFSET.len() - 1 {
                    return Err("unknown text ID".into());
                }
                word(rank)
            }
        };
        if piece.len() > expected - out.len() {
            return Err("text block exceeds declared length".into());
        }
        out.extend_from_slice(piece);
    }
    if out.len() != expected {
        return Err("text block shorter than declared length".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_ranks_and_literal_boundaries() {
        for rank in 0..WORD_OFFSET.len() - 1 {
            let piece = word(rank);
            let encoded = encode(piece);
            assert_eq!(encoded.len(), if rank < SHORT { 1 } else { 2 });
            assert_eq!(decode(&encoded, piece.len()).unwrap(), piece);
        }
        assert_eq!(encode(b" the"), [0x80]);
        for text in ["", "a\0z\r\n", "Aé🦀中文▁Ａe\u{301} the\tπ", "\u{10ffff}"] {
            let encoded = encode(text.as_bytes());
            assert_eq!(decode(&encoded, text.len()).unwrap(), text.as_bytes());
        }
    }

    #[test]
    fn rejects_corrupt_input() {
        for bad in [
            &[0xbd][..],
            &[0xff, 0xff],
            &[0xe2, 0x82],
            &[0xe0, 0x80, 0x80],
            &[0xed, 0xa0, 0x80],
            &[0xf4, 0x90, 0x80, 0x80],
        ] {
            assert!(decode(bad, 8).is_err());
        }
        assert!(decode(&[0x80], 1).is_err());
        assert!(decode(&[0x80], 5).is_err());
        assert!(decode(b"", usize::MAX).is_err());
    }
}
