//! Lossless, block-streaming compression with a built-in Gemma 4 vocabulary.
mod dynamic;
mod words;
mod zstd;

use std::io::{self, Read, Write};
pub use zstd::{plain_compress, plain_decompress};

const MAGIC: &[u8; 4] = b"ZTK\x01";
pub const BLOCK_SIZE: usize = 1 << 20;
const END: u8 = 255;
const PLAIN: u8 = 0;
const TOKENS: u8 = 1;
const DYNAMIC: u8 = 2;
const HISTORY: u8 = 3;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn put_size(mut value: usize, writer: &mut impl Write) -> io::Result<()> {
    let mut bytes = [0; 5];
    let mut n = 0;
    while value >= 128 {
        bytes[n] = (value as u8) | 128;
        value >>= 7;
        n += 1;
    }
    bytes[n] = value as u8;
    writer.write_all(&bytes[..=n])
}

fn byte(reader: &mut impl Read) -> io::Result<u8> {
    let mut b = [0];
    reader.read_exact(&mut b)?;
    Ok(b[0])
}

fn get_size(reader: &mut impl Read, limit: usize) -> io::Result<usize> {
    let mut n = 0usize;
    for shift in [0, 7, 14, 21] {
        let x = byte(reader)?;
        n |= ((x & 127) as usize) << shift;
        if n > limit {
            break;
        }
        if x < 128 {
            if shift != 0 && x == 0 {
                return Err(invalid("noncanonical block length"));
            }
            return Ok(n);
        }
    }
    Err(invalid("block length exceeds format limit"))
}

fn text_block(input: &[u8]) -> bool {
    // Reject binary control bytes, including NULs in UTF-16 and object files.
    !input.iter().any(|&b| b < 32 && !matches!(b, 9..=13)) && std::str::from_utf8(input).is_ok()
}

fn repeated_words(input: &[u8]) -> bool {
    if input.len() < 64 {
        return false;
    }
    if input.starts_with(b"\x1f\x8b")
        || input.starts_with(b"\x89PNG\r\n\x1a\n")
        || input.starts_with(b"\xff\xd8\xff")
        || input.starts_with(b"PK\x03\x04")
        || input.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
    {
        return false;
    }
    let count = (input.len() / 4).min(256);
    let mut seen = [0u64; 64];
    let mut hits = 0;
    for j in 0..count {
        let offset = (j * (input.len() / 4) / count) * 4;
        let x = u32::from_le_bytes(input[offset..offset + 4].try_into().unwrap());
        let key = ((x ^ x.rotate_left(15)).wrapping_mul(0x9e3779b1) >> 20) as usize;
        let mask = 1u64 << (key & 63);
        let slot = &mut seen[key >> 6];
        hits += (*slot & mask != 0) as usize;
        if hits > count / 16 {
            return true;
        }
        *slot |= mask;
    }
    false
}

fn select(input: &[u8], level: i32) -> Result<(u8, Vec<u8>), String> {
    if text_block(input) {
        if input.len() <= 2048 {
            let tokens = words::encode(input);
            // A token hit saves at least one byte. With meaningful coverage a
            // short block needs only one zstd call and no native CDict setup.
            if input.len() - tokens.len() >= input.len().div_ceil(16) {
                return Ok((TOKENS, plain_compress(&tokens, level)?));
            }
        }
        return Ok((HISTORY, zstd::compress_frame(input, level, true)?));
    }
    let plain = plain_compress(input, level)?;
    if repeated_words(input)
        && let Some(stream) = dynamic::encode(input)
    {
        let packed = plain_compress(&stream, level)?;
        if packed.len() < plain.len() {
            return Ok((DYNAMIC, packed));
        }
    }
    Ok((PLAIN, plain))
}

fn write_block(input: &[u8], level: i32, writer: &mut impl Write) -> io::Result<()> {
    let (kind, frame) = select(input, level).map_err(invalid)?;
    writer.write_all(&[kind])?;
    put_size(input.len(), writer)?;
    put_size(frame.len(), writer)?;
    if matches!(kind, HISTORY | TOKENS) {
        writer.write_all(&words::ID)?;
    }
    writer.write_all(&frame)
}

fn check_level(level: i32) -> io::Result<()> {
    if (1..=19).contains(&level) {
        Ok(())
    } else {
        Err(invalid("level must be between 1 and 19"))
    }
}

// If a block ends part-way through a valid UTF-8 scalar, carry at most three
// bytes into the next block. Binary input is still reconstructed exactly.
fn split_at(input: &[u8]) -> usize {
    for i in (input.len().saturating_sub(3)..input.len()).rev() {
        match input[i] {
            0x80..=0xbf => continue,
            0xc2..=0xdf if input.len() - i < 2 => return i,
            0xe0..=0xef if input.len() - i < 3 => return i,
            0xf0..=0xf4 if input.len() - i < 4 => return i,
            _ => break,
        }
    }
    input.len()
}

/// Compress a byte slice. For large files use the bounded streaming API.
pub fn compress(mut input: &[u8], level: i32) -> Result<Vec<u8>, String> {
    check_level(level).map_err(|e| e.to_string())?;
    let mut out = Vec::from(MAGIC.as_slice());
    while !input.is_empty() {
        let n = if input.len() >= BLOCK_SIZE {
            split_at(&input[..BLOCK_SIZE])
        } else {
            input.len()
        };
        write_block(&input[..n], level, &mut out).map_err(|e| e.to_string())?;
        input = &input[n..];
    }
    out.push(END);
    Ok(out)
}

/// Compress a reader in independent blocks of at most one MiB.
pub fn compress_stream(
    mut reader: impl Read,
    mut writer: impl Write,
    level: i32,
) -> io::Result<()> {
    check_level(level)?;
    writer.write_all(MAGIC)?;
    let mut block = vec![0u8; BLOCK_SIZE];
    let mut used = 0;
    loop {
        let n = loop {
            match reader.read(&mut block[used..]) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                other => break other?,
            }
        };
        used += n;
        if n == 0 {
            if used > 0 {
                write_block(&block[..used], level, &mut writer)?;
            }
            break;
        }
        if used == BLOCK_SIZE {
            let end = split_at(&block);
            write_block(&block[..end], level, &mut writer)?;
            block.copy_within(end.., 0);
            used -= end;
        }
    }
    writer.write_all(&[END])
}

struct Block {
    kind: u8,
    original: usize,
    length: usize,
}

fn check_magic(reader: &mut impl Read) -> io::Result<()> {
    let mut magic = [0; 4];
    reader.read_exact(&mut magic)?;
    if magic != *MAGIC {
        return Err(invalid("unknown ztok format or version"));
    }
    Ok(())
}

fn next_block(reader: &mut impl Read) -> io::Result<Option<Block>> {
    let kind = byte(reader)?;
    if kind == END {
        return match reader.read_exact(&mut [0]) {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e),
            Ok(()) => Err(invalid("trailing bytes after archive")),
        };
    }
    if kind > HISTORY {
        return Err(invalid("unknown block mode"));
    }
    let original = get_size(reader, BLOCK_SIZE)?;
    let length = get_size(reader, 2 * BLOCK_SIZE)?;
    if original == 0 || length == 0 {
        return Err(invalid("empty archive block"));
    }
    if matches!(kind, TOKENS | HISTORY) {
        let mut id = [0; 4];
        reader.read_exact(&mut id)?;
        if id != words::ID {
            return Err(invalid("Gemma vocabulary version mismatch"));
        }
    }
    Ok(Some(Block {
        kind,
        original,
        length,
    }))
}

impl Block {
    fn decode(&self, frame: &[u8]) -> io::Result<Vec<u8>> {
        let original = self.original;
        match self.kind {
            PLAIN | HISTORY => zstd::decompress_frame(frame, original, self.kind == HISTORY),
            TOKENS | DYNAMIC => {
                let size = zstd::content_len(frame, original).map_err(invalid)?;
                let stream = plain_decompress(frame, size).map_err(invalid)?;
                if self.kind == TOKENS {
                    words::decode(&stream, original)
                } else {
                    dynamic::decode(&stream, original)
                }
            }
            _ => unreachable!(),
        }
        .map_err(invalid)
    }
}

/// Decompress an archive into memory. The streaming API keeps memory bounded.
pub fn decompress(mut input: &[u8]) -> Result<Vec<u8>, String> {
    check_magic(&mut input).map_err(|e| e.to_string())?;
    let mut output = Vec::new();
    while let Some(block) = next_block(&mut input).map_err(|e| e.to_string())? {
        let (frame, rest) = input
            .split_at_checked(block.length)
            .ok_or("truncated ztok frame")?;
        input = rest;
        let decoded = block.decode(frame).map_err(|e| e.to_string())?;
        if output.is_empty() {
            output = decoded;
        } else {
            output.extend_from_slice(&decoded);
        }
    }
    Ok(output)
}

/// Validate and decode one complete archive. Fail on missing end marker/trailing data.
pub fn decompress_stream(mut reader: impl Read, mut writer: impl Write) -> io::Result<()> {
    check_magic(&mut reader)?;
    let mut frame = Vec::new();
    while let Some(block) = next_block(&mut reader)? {
        frame.resize(block.length, 0);
        reader.read_exact(&mut frame)?;
        writer.write_all(&block.decode(&frame)?)?;
    }
    Ok(())
}
