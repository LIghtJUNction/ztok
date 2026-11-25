//! Decoder for the version 0.1 block format and pinned Gemma vocabulary.
use super::{
    DYNAMIC, END, HISTORY, PLAIN, TOKENS, byte, get_size, invalid, plain_decompress, words, zstd,
};
use crate::dynamic;
use std::io::{self, Read, Write};

const BLOCK_SIZE: usize = 1 << 20;

struct Block {
    kind: u8,
    original: usize,
    length: usize,
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
pub(super) fn decompress(mut input: &[u8]) -> Result<Vec<u8>, String> {
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
pub(super) fn decompress_stream(mut reader: impl Read, mut writer: impl Write) -> io::Result<()> {
    let mut frame = Vec::new();
    while let Some(block) = next_block(&mut reader)? {
        frame.resize(block.length, 0);
        reader.read_exact(&mut frame)?;
        writer.write_all(&block.decode(&frame)?)?;
    }
    Ok(())
}
