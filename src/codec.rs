use super::{
    DYNAMIC, GETTEXT, HISTORY, PLAIN, PRESET, TAGS, TOKEN_PRESET, TOKENS, X86, dynamic, gettext,
    plain_compress, plain_decompress, put_size, words, zstd,
};

use words::PRESET_V2 as DICT;
static TOKEN_DICT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/token-preset-v2.zdict"));

// Compare complete representations, including their wire-format prefix.
struct Choice {
    kind: u8,
    frame: Vec<u8>,
    original: usize,
}

impl Choice {
    fn prefix(&self, bytes: &mut [u8; 5]) -> usize {
        if self.kind == PLAIN {
            return 0;
        }
        bytes[0] = TAGS[self.kind as usize];
        if matches!(self.kind, TOKENS | DYNAMIC | TOKEN_PRESET) {
            let mut tail = &mut bytes[1..];
            put_size(self.original, &mut tail).expect("one block fits four length bytes");
            5 - tail.len()
        } else {
            1
        }
    }
    fn size(&self) -> usize {
        self.frame.len() + self.prefix(&mut [0; 5])
    }
    fn consider(&mut self, kind: u8, frame: Vec<u8>) {
        let other = Self {
            kind,
            frame,
            original: self.original,
        };
        if other.size() < self.size() {
            *self = other;
        }
    }
    fn finish(mut self) -> Vec<u8> {
        let mut prefix = [0; 5];
        let n = self.prefix(&mut prefix);
        if n > 0 {
            let len = self.frame.len();
            self.frame.resize(len + n, 0);
            self.frame.copy_within(..len, n);
            self.frame[..n].copy_from_slice(&prefix[..n]);
        }
        self.frame
    }
}

fn looks_text(input: &[u8]) -> bool {
    let sample = &input[..input.len().min(1024)];
    !sample.iter().any(|&b| b < 32 && !matches!(b, 9..=13))
        && !input.starts_with(b"%PDF-")
        && std::str::from_utf8(input).is_ok()
}

fn text_choice(input: &[u8], level: i32) -> Result<Choice, String> {
    let (kind, frame) = if input.len() <= 2048 {
        (PRESET, zstd::compress_with_dict(input, level, DICT)?)
    } else if input.len() <= 256 * 1024 {
        let tokens = words::encode(input);
        if input.len() - tokens.len() >= input.len().div_ceil(8) {
            (
                TOKEN_PRESET,
                zstd::compress_with_dict(&tokens, level, TOKEN_DICT)?,
            )
        } else {
            (PRESET, zstd::compress_with_dict(input, level, DICT)?)
        }
    } else {
        (PLAIN, plain_compress(input, level)?)
    };
    Ok(Choice {
        kind,
        frame,
        original: input.len(),
    })
}

pub(super) fn encode(input: &[u8], level: i32, best: bool) -> Result<Vec<u8>, String> {
    let binary_filter = || {
        if let Some(stream) = gettext::encode(input) {
            return Some((GETTEXT, stream));
        }
        dynamic::x86_encode(input).map(|stream| (X86, stream))
    };
    if let Some((kind, stream)) = binary_filter() {
        let mut choice = Choice {
            kind,
            frame: plain_compress(&stream, level)?,
            original: input.len(),
        };
        if best {
            choice.consider(PLAIN, plain_compress(input, level)?);
        }
        return Ok(choice.finish());
    }
    let text = looks_text(input);
    if text && !best {
        return Ok(text_choice(input, level)?.finish());
    }
    let mut choice = Choice {
        kind: PLAIN,
        frame: plain_compress(input, level)?,
        original: input.len(),
    };
    if text {
        choice.consider(PRESET, zstd::compress_with_dict(input, level, DICT)?);
        choice.consider(HISTORY, zstd::compress_frame(input, level, true)?);
        let tokens = words::encode(input);
        if tokens.len() < input.len() {
            choice.consider(
                TOKEN_PRESET,
                zstd::compress_with_dict(&tokens, level, TOKEN_DICT)?,
            );
            choice.consider(TOKENS, plain_compress(&tokens, level)?);
        }
    } else if dynamic::candidate(input)
        && let Some(stream) = dynamic::encode(input)
    {
        choice.consider(DYNAMIC, plain_compress(&stream, level)?);
    }
    Ok(choice.finish())
}

pub(super) fn decode(
    kind: u8,
    frame: &[u8],
    size: usize,
    original: usize,
) -> Result<Vec<u8>, String> {
    match kind {
        PLAIN => plain_decompress(frame, size),
        HISTORY => zstd::decompress_frame(frame, size, true),
        PRESET => zstd::decompress_with_dict(frame, size, DICT),
        TOKEN_PRESET => words::decode(
            &zstd::decompress_with_dict(frame, size, TOKEN_DICT)?,
            original,
        ),
        TOKENS => words::decode(&plain_decompress(frame, size)?, original),
        DYNAMIC => dynamic::decode(&plain_decompress(frame, size)?, original),
        GETTEXT => gettext::decode(plain_decompress(frame, size)?),
        X86 => dynamic::x86_decode(plain_decompress(frame, size)?),
        _ => Err("unknown compact block mode".into()),
    }
}
