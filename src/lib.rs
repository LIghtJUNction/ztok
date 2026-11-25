//! Lossless, block-streaming compression with a built-in Gemma 4 vocabulary.
mod codec;
mod dynamic;
mod gettext;
mod legacy;
mod words;
mod zstd;

use std::io::{self, Read, Write};
pub use zstd::{plain_compress, plain_decompress};

const V1: &[u8; 4] = b"ZTK\x01";
const V2: &[u8; 4] = b"ZTK\x02";
pub const BLOCK_SIZE: usize = 4 << 20;
/// Maximum independent input for the size-priority encoder.
pub const BEST_BLOCK_SIZE: usize = 16 << 20;
const MAX_FRAME: usize = BEST_BLOCK_SIZE + BEST_BLOCK_SIZE / 128 + 128;
const END: u8 = 255;
const PLAIN: u8 = 0;
const TOKENS: u8 = 1;
const DYNAMIC: u8 = 2;
const HISTORY: u8 = 3;
const PRESET: u8 = 4;
const X86: u8 = 5;
const GETTEXT: u8 = 6;
const TOKEN_PRESET: u8 = 7;

// These immutable dispatch bytes identify the transform *and* its version.
// The standard zstd prefix represents a portable, dictionary-free block.
const TAGS: [u8; 8] = [0x28, 0xa7, 0x59, 0xc3, 0x96, 0x3c, 0x4e, 0xf0];

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

fn check_level(level: i32) -> Result<(), String> {
    if (1..=19).contains(&level) {
        Ok(())
    } else {
        Err("level must be between 1 and 19".into())
    }
}

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

/// Encode a file with automatic routing, reusing the prepared dictionaries.
pub fn compress(input: &[u8], level: i32) -> Result<Vec<u8>, String> {
    encode_slice(input, level, false)
}

/// Select the smallest complete representation among applicable encoders per block.
pub fn compress_best(input: &[u8], level: i32) -> Result<Vec<u8>, String> {
    encode_slice(input, level, true)
}

fn encode_slice(mut input: &[u8], level: i32, best: bool) -> Result<Vec<u8>, String> {
    check_level(level)?;
    let limit = if best { BEST_BLOCK_SIZE } else { BLOCK_SIZE };
    if input.is_empty() {
        return Ok([V2.as_slice(), &[0]].concat());
    }
    if input.len() <= limit {
        return codec::encode(input, level, best);
    }
    let mut output = Vec::from(V2.as_slice());
    while !input.is_empty() {
        let n = if input.len() > limit {
            split_at(&input[..limit])
        } else {
            input.len()
        };
        let frame = codec::encode(&input[..n], level, best)?;
        put_size(frame.len(), &mut output).expect("Vec write");
        output.extend_from_slice(&frame);
        input = &input[n..];
    }
    output.push(0);
    Ok(output)
}

/// Encode a reader with bounded working memory (four MiB maximum input per block).
pub fn compress_stream(reader: impl Read, writer: impl Write, level: i32) -> io::Result<()> {
    encode_stream(reader, writer, level, false)
}

/// Streaming equivalent of `compress_best`.
pub fn compress_best_stream(reader: impl Read, writer: impl Write, level: i32) -> io::Result<()> {
    encode_stream(reader, writer, level, true)
}

fn encode_stream(
    mut reader: impl Read,
    mut writer: impl Write,
    level: i32,
    best: bool,
) -> io::Result<()> {
    check_level(level).map_err(invalid)?;
    let mut block = Vec::with_capacity(8192);
    let limit = if best { BEST_BLOCK_SIZE } else { BLOCK_SIZE };
    let mut multi = false;
    loop {
        let space = limit + 1 - block.len();
        reader.by_ref().take(space as u64).read_to_end(&mut block)?;
        let last = block.len() <= limit;
        if !multi && (block.is_empty() || !last) {
            writer.write_all(V2)?;
            multi = true;
        }
        let n = if last {
            block.len()
        } else {
            split_at(&block[..limit])
        };
        if n > 0 {
            let frame = codec::encode(&block[..n], level, best).map_err(invalid)?;
            if multi {
                put_size(frame.len(), &mut writer)?;
            }
            writer.write_all(&frame)?;
        }
        if last {
            break;
        }
        block.copy_within(n.., 0);
        block.truncate(block.len() - n);
    }
    if multi {
        writer.write_all(&[0])?;
    }
    Ok(())
}

fn mode(prefix: u8) -> Option<u8> {
    TAGS.iter().position(|&tag| tag == prefix).map(|i| i as u8)
}

/// Return the first block's encoding for inspection (0xff is an empty archive).
/// This only inspects a prefix. Decompression validates the complete archive.
pub fn first_mode(mut input: &[u8]) -> Option<u8> {
    if input.starts_with(V1) {
        return input.get(4).copied();
    }
    if input.starts_with(V2) {
        input = &input[4..];
        let n = get_size(&mut input, MAX_FRAME).ok()?;
        if n == 0 {
            return Some(END);
        }
    }
    mode(*input.first()?)
}

fn unpack(mut frame: &[u8]) -> Result<Vec<u8>, String> {
    if frame.len() > MAX_FRAME {
        return Err("oversized compact block".into());
    }
    let kind =
        mode(*frame.first().ok_or("empty compact block")?).ok_or("unknown compact block mode")?;
    if kind != PLAIN {
        frame = &frame[1..];
    }
    let original = if matches!(kind, TOKENS | DYNAMIC | TOKEN_PRESET) {
        Some(get_size(&mut frame, BEST_BLOCK_SIZE).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let size = zstd::content_len(frame, original.unwrap_or(BEST_BLOCK_SIZE))?;
    codec::decode(kind, frame, size, original.unwrap_or(size))
}

/// Decode a complete archive including version 0.1 archives.
pub fn decompress(mut input: &[u8]) -> Result<Vec<u8>, String> {
    if let Some(rest) = input.strip_prefix(V1) {
        return legacy::decompress(rest);
    }
    if !input.starts_with(V2) {
        return unpack(input);
    }
    input = &input[4..];
    let mut output = Vec::new();
    loop {
        let n = get_size(&mut input, MAX_FRAME).map_err(|e| e.to_string())?;
        if n == 0 {
            return if input.is_empty() {
                Ok(output)
            } else {
                Err("trailing data after archive".into())
            };
        }
        let (frame, rest) = input.split_at_checked(n).ok_or("truncated compact block")?;
        output.extend_from_slice(&unpack(frame)?);
        input = rest;
    }
}

/// Decode a complete archive without accumulating the output file in memory.
pub fn decompress_stream(mut reader: impl Read, mut writer: impl Write) -> io::Result<()> {
    let mut magic = [0; 4];
    reader.read_exact(&mut magic)?;
    if &magic == V1 {
        return legacy::decompress_stream(reader, writer);
    }
    let mut frame = Vec::new();
    if &magic != V2 {
        frame.extend_from_slice(&magic);
        reader
            .take((MAX_FRAME - 3) as u64)
            .read_to_end(&mut frame)?;
        return writer.write_all(&unpack(&frame).map_err(invalid)?);
    }
    loop {
        let n = get_size(&mut reader, MAX_FRAME)?;
        if n == 0 {
            return match reader.read_exact(&mut [0]) {
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(()),
                Err(e) => Err(e),
                Ok(()) => Err(invalid("trailing data after archive")),
            };
        }
        frame.resize(n, 0);
        reader.read_exact(&mut frame)?;
        writer.write_all(&unpack(&frame).map_err(invalid)?)?;
    }
}
