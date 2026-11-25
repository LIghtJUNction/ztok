//! Convert relative near-call/jump operands to offsets within a block, allowing
//! calls to the same target to match. Only the first block of ELF64 x86-64
//! files has the program headers needed to choose executable bytes.
use std::ops::Range;

struct Executable {
    segments: Vec<Range<usize>>,
}

fn executable(input: &[u8]) -> Option<Executable> {
    if input.get(..7)? != b"\x7fELF\x02\x01\x01"
        || input.get(18..20)? != [62, 0]
        || input.len() < 64
    {
        return None;
    }
    let table = usize::try_from(u64::from_le_bytes(input[32..40].try_into().ok()?)).ok()?;
    let stride = usize::from(u16::from_le_bytes([input[54], input[55]]));
    let count = usize::from(u16::from_le_bytes([input[56], input[57]]));
    if table < 64 || stride != 56 || count == 0 || count > 128 {
        return None;
    }
    let end = table.checked_add(stride * count)?;
    let mut segments = Vec::new();
    let mut prev_end = end;
    for ph in input.get(table..end)?.as_chunks::<56>().0 {
        // PT_LOAD with PF_X.
        if ph[..4] != [1, 0, 0, 0] || ph[4] & 1 == 0 {
            continue;
        }
        let pos = u64::from_le_bytes(ph[8..16].try_into().unwrap());
        let n = u64::from_le_bytes(ph[32..40].try_into().unwrap());
        let stop = pos.checked_add(n)?.min(input.len() as u64) as usize;
        let start = pos.min(input.len() as u64) as usize;
        // Never mutate the metadata used to reconstruct the ranges.
        let start = start.max(end);
        if start < stop {
            if start < prev_end {
                return None;
            }
            segments.push(start..stop);
            prev_end = stop;
        }
    }
    (!segments.is_empty()).then_some(Executable { segments })
}

const LOW: u64 = 0x0101_0101_0101_0101;
const HIGH: u64 = LOW << 7;
const MASK: u64 = !LOW;
const OPCODE: u64 = LOW * 0xe8;

fn convert(data: &mut [u8], exec: Executable, inverse: bool) -> usize {
    let mut changes = 0;
    for segment in exec.segments {
        let mut i = segment.start;
        while i + 5 <= segment.end {
            if i + 8 <= segment.end {
                // Skip eight bytes at once when none is E8/E9.
                let x = (u64::from_le_bytes(data[i..i + 8].try_into().unwrap()) & MASK) ^ OPCODE;
                let found = x.wrapping_sub(LOW) & !x & HIGH;
                if found == 0 {
                    i += 8;
                    continue;
                }
                i += (found.trailing_zeros() / 8) as usize;
                if i + 5 > segment.end {
                    break;
                }
            }
            if data[i] & 0xfe == 0xe8 {
                let next = i + 5;
                let bytes: &mut [u8; 4] = (&mut data[i + 1..next]).try_into().unwrap();
                let value = u32::from_le_bytes(*bytes);
                // A wrapping 32-bit translation is bijective for every byte
                // sequence, even if this opcode was data rather than code.
                *bytes = if inverse {
                    value.wrapping_sub(next as u32)
                } else {
                    value.wrapping_add(next as u32)
                }
                .to_le_bytes();
                changes += 1;
                i = next;
            } else {
                i += 1;
            }
        }
    }
    changes
}

pub(crate) fn encode(input: &[u8]) -> Option<Vec<u8>> {
    let exec = executable(input)?;
    let mut out = input.to_vec();
    (convert(&mut out, exec, false) >= 16).then_some(out)
}

pub(crate) fn decode(mut input: Vec<u8>) -> Result<Vec<u8>, String> {
    let exec = executable(&input).ok_or("invalid x86 executable layout")?;
    convert(&mut input, exec, true);
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf() -> Vec<u8> {
        let mut bytes = vec![0x90; 640];
        bytes[..120].fill(0);
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[18] = 62;
        bytes[32] = 64;
        bytes[54] = 56;
        bytes[56] = 1;
        bytes[64] = 1;
        bytes[68] = 5;
        bytes[72..80].copy_from_slice(&128u64.to_le_bytes());
        bytes[96..104].copy_from_slice(&384u64.to_le_bytes());
        bytes
    }

    #[test]
    fn operands_in_code_only_including_wraparound() {
        let mut original = elf();
        let mut i = 128;
        for j in 0..25 {
            original[i] = 0xe8 | (j % 2) as u8;
            // Includes apparent opcodes in the operand; the next real opcode
            // is always found identically in both directions.
            original[i + 1..i + 5].copy_from_slice(&0xffff_e9e8u32.to_le_bytes());
            i += 5 + (j % 8);
        }
        original[510..514].fill(0xe8);
        original[600..605].fill(0xe9);
        let data = encode(&original).unwrap();
        assert_ne!(original, data);
        assert_eq!(original[..128], data[..128]);
        assert_eq!(original[510..], data[510..]);
        assert_eq!(decode(data).unwrap(), original);
        assert!(decode(original[..80].to_vec()).is_err());
    }

    #[test]
    fn no_execution_or_bad_header() {
        assert!(encode(&elf()).is_none());
        let mut b = elf();
        b[32..40].fill(255);
        assert!(decode(b).is_err());
    }
}
