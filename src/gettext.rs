//! Byte-plane and offset-delta filter for the two string tables of a GNU MO
//! revision-zero catalog. Both the format header and all strings stay intact.
use std::ops::Range;

#[derive(Clone, Copy)]
struct Order {
    little: bool,
}

impl Order {
    fn read(self, data: &[u8]) -> u32 {
        let bytes = data[..4].try_into().unwrap();
        if self.little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    }

    fn bytes(self, n: u32) -> [u8; 4] {
        if self.little {
            n.to_le_bytes()
        } else {
            n.to_be_bytes()
        }
    }
}

struct Tables {
    order: Order,
    count: usize,
    ranges: [Range<usize>; 2],
}

fn tables(input: &[u8]) -> Option<Tables> {
    let header = input.get(..28)?;
    let little = match &header[..4] {
        [0xde, 0x12, 0x04, 0x95] => true,
        [0x95, 0x04, 0x12, 0xde] => false,
        _ => return None,
    };
    let order = Order { little };
    if order.read(&header[4..]) != 0 {
        return None;
    }
    let count = usize::try_from(order.read(&header[8..])).ok()?;
    if count < 4 {
        return None;
    }
    let size = count.checked_mul(8)?;
    let range = |offset| {
        let start = usize::try_from(order.read(&header[offset..])).ok()?;
        let end = start.checked_add(size)?;
        (start >= header.len() && end <= input.len()).then_some(start..end)
    };
    let first = range(12)?;
    let second = range(16)?;
    if first.start < second.end && second.start < first.end {
        return None;
    }
    Some(Tables {
        order,
        count,
        ranges: [first, second],
    })
}

pub(super) fn encode(input: &[u8]) -> Option<Vec<u8>> {
    let Tables {
        order,
        count,
        ranges,
    } = tables(input)?;
    let mut out = input.to_vec();
    for range in ranges {
        let table = &mut out[range.clone()];
        let mut after = 0;
        for (i, entry) in input[range].as_chunks::<8>().0.iter().enumerate() {
            let len = order.read(entry);
            let pos = order.read(&entry[4..]);
            let gap = pos.wrapping_sub(after);
            after = pos.wrapping_add(len).wrapping_add(1);
            let l = len.to_le_bytes();
            let g = gap.to_le_bytes();
            for lane in 0..4 {
                table[lane * count + i] = l[lane];
                table[(lane + 4) * count + i] = g[lane];
            }
        }
    }
    Some(out)
}

pub(super) fn decode(mut input: Vec<u8>) -> Result<Vec<u8>, String> {
    let Tables {
        order,
        count,
        ranges,
    } = tables(&input).ok_or("invalid gettext string table")?;
    let mut scratch = Vec::with_capacity(count * 8);
    for range in ranges {
        let table = &mut input[range];
        scratch.clear();
        scratch.extend_from_slice(table);
        let mut after = 0u32;
        for (i, entry) in table.as_chunks_mut::<8>().0.iter_mut().enumerate() {
            let len = u32::from_le_bytes(std::array::from_fn(|lane| scratch[lane * count + i]));
            let gap =
                u32::from_le_bytes(std::array::from_fn(|lane| scratch[(lane + 4) * count + i]));
            let pos = after.wrapping_add(gap);
            after = pos.wrapping_add(len).wrapping_add(1);
            entry[..4].copy_from_slice(&order.bytes(len));
            entry[4..].copy_from_slice(&order.bytes(pos));
        }
    }
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(order: Order) -> Vec<u8> {
        let mut b = vec![0u8; 149];
        for (at, val) in [(0, 0x9504_12de), (8, 4), (12, 96), (16, 32)] {
            b[at..at + 4].copy_from_slice(&order.bytes(val));
        }
        for at in [32, 96] {
            for (i, (len, pos)) in [(0, 130), (7, 131), (2, 2), (u32::MAX, u32::MAX)]
                .into_iter()
                .enumerate()
            {
                b[at + i * 8..at + i * 8 + 4].copy_from_slice(&order.bytes(len));
                b[at + i * 8 + 4..at + i * 8 + 8].copy_from_slice(&order.bytes(pos));
            }
        }
        b[128..].fill(0xab);
        b
    }

    #[test]
    fn both_orders_reversed_table_order_and_wrapping_offsets() {
        for little in [false, true] {
            let input = catalog(Order { little });
            let coded = encode(&input).unwrap();
            assert_eq!(&input[..32], &coded[..32]);
            assert_eq!(&input[64..96], &coded[64..96]);
            assert_eq!(&input[128..], &coded[128..]);
            assert_eq!(&coded[32..36], &[0, 7, 2, 255]);
            assert_eq!(decode(coded).unwrap(), input);
        }
    }

    #[test]
    fn reject_unrecognizable_or_overlapping_layout() {
        let good = catalog(Order { little: true });
        for (offset, val) in [
            (4, 1),
            (8, u32::MAX),
            (12, 0),
            (12, 33),
            (16, 124),
            (16, u32::MAX),
        ] {
            let mut bad = good.clone();
            bad[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
            assert!(encode(&bad).is_none());
            assert!(decode(bad).is_err());
        }
        assert!(encode(&good[..10]).is_none());
    }
}
