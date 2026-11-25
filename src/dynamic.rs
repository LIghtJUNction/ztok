//! Block-local binary transform. The dictionary stores aligned four-byte words.
//! IDs 0..126 cost one byte. IDs 127..382 cost two. Tags 128..255 introduce
//! literal runs of 1..128 bytes. All integer fields use little-endian order.

mod x86;
pub(crate) use x86::{decode as x86_decode, encode as x86_encode};

/// Look for repeated aligned fields before spending time on a local dictionary.
pub(crate) fn candidate(input: &[u8]) -> bool {
    if input.len() < 64
        || input.starts_with(b"\x1f\x8b")
        || input.starts_with(b"\x89PNG\r\n\x1a\n")
        || input.starts_with(b"\xff\xd8\xff")
        || input.starts_with(b"PK\x03\x04")
        || input.starts_with(b"\x28\xb5\x2f\xfd")
        || input.starts_with(b"\x7fELF")
        || input.starts_with(b"%PDF-")
        || input.starts_with(b"\xfd7zXZ\0")
        || input.starts_with(b"BZh")
        || input.starts_with(b"wOF2")
    {
        return false;
    }
    let words = input.as_chunks::<4>().0;
    let n = words.len().min(256);
    let mut seen = [0u64; 64];
    let mut hits = 0;
    // Consecutive windows distributed through the block retain the mix of
    // fields in packed records even when their stride is a power of two.
    let window = n.min(16);
    let groups = n / window;
    for group in 0..groups {
        let start = (words.len() - window) * group / (groups - 1).max(1);
        for &bytes in &words[start..start + window] {
            let x = u32::from_le_bytes(bytes);
            let key = ((x ^ x.rotate_left(15)).wrapping_mul(0x9e3779b1) >> 20) as usize;
            let mask = 1u64 << (key & 63);
            let slot = &mut seen[key >> 6];
            hits += usize::from(*slot & mask != 0);
            if hits > n / 12 {
                return true;
            }
            *slot |= mask;
        }
    }
    false
}

const SHORT_IDS: usize = 127;
const MAX_ENTRIES: usize = SHORT_IDS + 256;
const SAMPLE_WORDS: usize = 4096;
const SAMPLE_WINDOWS: usize = 64;

fn word_values(bytes: &[u8]) -> impl Iterator<Item = u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .copied()
        .map(u32::from_le_bytes)
}

fn sample_words(input: &[u8]) -> Vec<u32> {
    let words = input.len() / 4;
    if words <= SAMPLE_WORDS {
        return word_values(input).collect();
    }
    let mut sample = Vec::with_capacity(SAMPLE_WORDS);
    let window = SAMPLE_WORDS / SAMPLE_WINDOWS;
    // Evenly spaced contiguous windows avoid the aliasing of a fixed word stride.
    for i in 0..SAMPLE_WINDOWS {
        let spread = words - window;
        let pos = (spread / (SAMPLE_WINDOWS - 1)) * i
            + (spread % (SAMPLE_WINDOWS - 1)) * i / (SAMPLE_WINDOWS - 1);
        sample.extend(word_values(&input[4 * pos..4 * (pos + window)]));
    }
    sample
}

fn ranked_candidates(input: &[u8]) -> Vec<(u32, usize)> {
    let mut sample = sample_words(input);
    sample.sort_unstable();
    let mut candidates = Vec::new();
    let mut remaining = sample.as_slice();
    while let Some(&word) = remaining.first() {
        let count = remaining.iter().take_while(|&&next| next == word).count();
        if count > 1 {
            candidates.push((word, count));
        }
        remaining = &remaining[count..];
    }
    rank(&mut candidates);
    candidates.truncate(MAX_ENTRIES);
    candidates
}

fn rank(candidates: &mut [(u32, usize)]) {
    candidates.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
}

const SLOTS: usize = 1024;
const EMPTY: u16 = u16::MAX;
const COLLISION: u16 = 1 << 15;

fn bucket(word: u32) -> usize {
    ((word ^ (word >> 16)).wrapping_mul(0x9e3779b1) >> 22) as usize
}

// Most lookups require a single slot. A sorted overflow array bounds the cost
// even when all input words hash to the same slot.
struct WordIndex {
    slots: [(u32, u16); SLOTS],
    overflow: Vec<(u32, u16)>,
}

impl WordIndex {
    fn new(words: &[(u32, usize)]) -> Self {
        let mut index = Self {
            slots: [(0, EMPTY); SLOTS],
            overflow: Vec::new(),
        };
        for (id, &(word, _)) in words.iter().enumerate() {
            let slot = &mut index.slots[bucket(word)];
            if slot.1 == EMPTY {
                *slot = (word, id as u16);
            } else {
                slot.1 |= COLLISION;
                index.overflow.push((word, id as u16));
            }
        }
        index.overflow.sort_unstable_by_key(|entry| entry.0);
        index
    }

    #[inline]
    fn get(&self, word: u32) -> Option<usize> {
        let (first, id) = self.slots[bucket(word)];
        if id == EMPTY {
            return None;
        }
        if first == word {
            return Some((id & !COLLISION) as usize);
        }
        if id & COLLISION == 0 {
            return None;
        }
        self.overflow
            .binary_search_by_key(&word, |entry| entry.0)
            .ok()
            .map(|i| self.overflow[i].1 as usize)
    }
}

fn literals(out: &mut Vec<u8>, bytes: &[u8]) {
    for run in bytes.chunks(128) {
        out.push((127 + run.len()) as u8);
        out.extend_from_slice(run);
    }
}

/// Return a self-contained transform only if the complete representation shrinks.
pub fn encode(input: &[u8]) -> Option<Vec<u8>> {
    if input.len() < 64 {
        return None;
    }
    let mut words = ranked_candidates(input);
    if words.is_empty() {
        return None;
    }
    if input.len() / 4 > SAMPLE_WORDS {
        let index = WordIndex::new(&words);
        for entry in &mut words {
            entry.1 = 0;
        }
        for word in word_values(input) {
            if let Some(id) = index.get(word) {
                words[id].1 += 1;
            }
        }
        rank(&mut words);
    }
    // A two-byte ID needs three occurrences to repay a four-byte entry.
    if words.len() > SHORT_IDS {
        let long_ids = words[SHORT_IDS..].partition_point(|entry| entry.1 >= 3);
        words.truncate(SHORT_IDS + long_ids);
    }
    let index = WordIndex::new(&words);
    let mut out = Vec::with_capacity(input.len());
    out.extend_from_slice(&(words.len() as u16).to_le_bytes());
    for &(word, _) in &words {
        out.extend_from_slice(&word.to_le_bytes());
    }
    let mut literal_start = 0;
    for (i, word) in word_values(input).enumerate() {
        if let Some(id) = index.get(word) {
            literals(&mut out, &input[literal_start..4 * i]);
            if id < SHORT_IDS {
                out.push(id as u8);
            } else {
                out.push(SHORT_IDS as u8);
                out.push((id - SHORT_IDS) as u8);
            }
            literal_start = 4 * (i + 1);
        }
    }
    literals(&mut out, &input[literal_start..]);
    (out.len() < input.len()).then_some(out)
}

/// Decode a single transformed block with an exact output limit.
pub fn decode(input: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    let header = input.get(..2).ok_or("missing binary dictionary count")?;
    let entries = u16::from_le_bytes([header[0], header[1]]) as usize;
    if !(1..=MAX_ENTRIES).contains(&entries) {
        return Err("invalid binary dictionary count".into());
    }
    let end = 2 + 4 * entries;
    let dictionary = input.get(2..end).ok_or("truncated binary dictionary")?;
    let mut codes = &input[end..];
    if expected == 0 || expected > codes.len().saturating_mul(4) {
        return Err("impossible binary output size".into());
    }
    let mut out = Vec::new();
    out.try_reserve_exact(expected)
        .map_err(|_| "binary output allocation failed")?;
    while let Some((&tag, rest)) = codes.split_first() {
        codes = rest;
        let bytes = if tag >= 128 {
            let len = tag as usize - 127;
            let bytes = codes.get(..len).ok_or("truncated binary literal")?;
            codes = &codes[len..];
            bytes
        } else {
            let id = if tag == SHORT_IDS as u8 {
                let (&extension, rest) = codes.split_first().ok_or("truncated binary word ID")?;
                codes = rest;
                SHORT_IDS + extension as usize
            } else {
                tag as usize
            };
            dictionary
                .get(4 * id..4 * (id + 1))
                .ok_or("invalid binary word ID")?
        };
        if bytes.len() > expected - out.len() {
            return Err("binary output exceeds declared size".into());
        }
        out.extend_from_slice(bytes);
    }
    if out.len() != expected {
        return Err("incorrect binary output size".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_literals_and_unaligned_tail() {
        let mut input = vec![0x66; 512];
        input.extend(0..=255);
        input.extend([0x42, 0xff, 0x0a]);
        let encoded = encode(&input).unwrap();
        assert_eq!(decode(&encoded, input.len()).unwrap(), input);
        assert_eq!(&encoded[2..6], &[0x66; 4]);
        assert_eq!(encode(&input).unwrap(), encoded);
        assert!(decode(&encoded, input.len() - 1).is_err());
    }

    #[test]
    fn extended_ids_and_large_block() {
        let mut input = Vec::new();
        for _ in 0..40 {
            for word in 0..MAX_ENTRIES as u32 {
                input.extend_from_slice(&word.to_le_bytes());
            }
            input.extend_from_slice(&900u32.to_le_bytes());
            input.extend_from_slice(&900u32.to_le_bytes());
        }
        let encoded = encode(&input).unwrap();
        assert_eq!(
            u16::from_le_bytes([encoded[0], encoded[1]]) as usize,
            MAX_ENTRIES
        );
        assert_eq!(&encoded[2..6], &900u32.to_le_bytes());
        assert_eq!(decode(&encoded, input.len()).unwrap(), input);
    }

    #[test]
    fn inverse_rejects_invalid_lengths_codes_and_truncation() {
        let dict = [1, 0, 7, 3, 2, 1];
        for (suffix, length) in [
            (&[128][..], 1),
            (&[127][..], 4),
            (&[127, 0][..], 4),
            (&[1][..], 4),
            (&[0][..], 3),
            (&[0][..], 5),
            (&[0, 0][..], 4),
        ] {
            assert!(decode(&[dict.as_slice(), suffix].concat(), length).is_err());
        }
        assert!(decode(&[0, 0], 0).is_err());
        assert!(decode(&[255, 255], 4).is_err());
        assert!(decode(&[1, 0, 0], 4).is_err());
        assert!(decode(&[], usize::MAX).is_err());
    }

    #[test]
    fn skip_tiny_and_distinct_words() {
        let distinct: Vec<u8> = (0u32..2048).flat_map(u32::to_le_bytes).collect();
        assert!(encode(&distinct).is_none());
        assert!(encode(&[]).is_none());
    }
}
