//! One-shot, checksummed Zstandard frames. Contexts are reused within a thread.
use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::ptr::NonNull;

#[link(name = "zstd")]
unsafe extern "C" {
    fn ZSTD_createCCtx() -> *mut c_void;
    fn ZSTD_createCDict(dict: *const u8, len: usize, level: c_int) -> *mut c_void;
    fn ZSTD_freeCDict(dict: *mut c_void) -> usize;
    fn ZSTD_CCtx_refCDict(ctx: *mut c_void, dict: *const c_void) -> usize;
    fn ZSTD_createDDict(dict: *const u8, len: usize) -> *mut c_void;
    fn ZSTD_freeDDict(dict: *mut c_void) -> usize;
    fn ZSTD_DCtx_refDDict(ctx: *mut c_void, dict: *const c_void) -> usize;
    fn ZSTD_freeCCtx(ctx: *mut c_void) -> usize;
    fn ZSTD_createDCtx() -> *mut c_void;
    fn ZSTD_freeDCtx(ctx: *mut c_void) -> usize;
    fn ZSTD_CCtx_setParameter(ctx: *mut c_void, param: c_int, value: c_int) -> usize;
    fn ZSTD_compress2(
        ctx: *mut c_void,
        dst: *mut u8,
        capacity: usize,
        src: *const u8,
        len: usize,
    ) -> usize;
    fn ZSTD_decompressDCtx(
        ctx: *mut c_void,
        dst: *mut u8,
        capacity: usize,
        src: *const u8,
        len: usize,
    ) -> usize;
    fn ZSTD_compressBound(len: usize) -> usize;
    fn ZSTD_isError(code: usize) -> c_uint;
    fn ZSTD_getErrorName(code: usize) -> *const c_char;
    fn ZSTD_getFrameContentSize(src: *const u8, len: usize) -> u64;
    fn ZSTD_findFrameCompressedSize(src: *const u8, len: usize) -> usize;
}

// Only embedded dictionaries enter this cache. Identity comparisons are O(1)
// because each preset has immutable, static storage.
type Key = (*const u8, usize);

fn key(bytes: &'static [u8]) -> Key {
    (bytes.as_ptr(), bytes.len())
}

struct Encoder {
    ctx: NonNull<c_void>,
    level: Option<i32>,
    dictionaries: Vec<(Key, NonNull<c_void>)>,
    selected: Option<Key>,
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { ZSTD_freeCCtx(self.ctx.as_ptr()) };
        for (_, dictionary) in self.dictionaries.drain(..) {
            unsafe { ZSTD_freeCDict(dictionary.as_ptr()) };
        }
    }
}

impl Encoder {
    fn new() -> Result<Self, String> {
        let ctx =
            NonNull::new(unsafe { ZSTD_createCCtx() }).ok_or("zstd encoder allocation failed")?;
        let encoder = Self {
            ctx,
            level: None,
            dictionaries: Vec::new(),
            selected: None,
        };
        // ZSTD_c_checksumFlag. This is sticky across independent frames.
        check(unsafe { ZSTD_CCtx_setParameter(ctx.as_ptr(), 201, 1) })?;
        // ZSTD_c_dictIDFlag. The compact preset tag identifies its immutable
        // dictionary. Plain frames and the v1 raw history have no native ID.
        check(unsafe { ZSTD_CCtx_setParameter(ctx.as_ptr(), 202, 0) })?;
        Ok(encoder)
    }

    fn configure(&mut self, level: i32, preset: Option<&'static [u8]>) -> Result<(), String> {
        let ctx = self.ctx.as_ptr();
        if self.level != Some(level) {
            if self.selected.is_some() {
                check(unsafe { ZSTD_CCtx_refCDict(ctx, std::ptr::null()) })?;
                self.selected = None;
            }
            for (_, dict) in self.dictionaries.drain(..) {
                unsafe { ZSTD_freeCDict(dict.as_ptr()) };
            }
            // ZSTD_c_compressionLevel. A CDict is created with this same level.
            check(unsafe { ZSTD_CCtx_setParameter(ctx, 100, level) })?;
            self.level = Some(level);
        }
        let wanted = preset.map(key);
        if self.selected == wanted {
            return Ok(());
        }
        let dict = if let Some(bytes) = preset {
            let id = key(bytes);
            match self.dictionaries.iter().find(|(k, _)| *k == id) {
                Some((_, handle)) => handle.as_ptr(),
                None => {
                    let handle = NonNull::new(unsafe {
                        ZSTD_createCDict(bytes.as_ptr(), bytes.len(), level)
                    })
                    .ok_or("zstd compression dictionary allocation failed")?;
                    self.dictionaries.push((id, handle));
                    handle.as_ptr()
                }
            }
        } else {
            std::ptr::null_mut()
        };
        check(unsafe { ZSTD_CCtx_refCDict(ctx, dict) })?;
        self.selected = wanted;
        Ok(())
    }
}

struct Decoder {
    ctx: NonNull<c_void>,
    dictionaries: Vec<(Key, NonNull<c_void>)>,
    selected: Option<Key>,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { ZSTD_freeDCtx(self.ctx.as_ptr()) };
        for (_, dict) in self.dictionaries.drain(..) {
            unsafe { ZSTD_freeDDict(dict.as_ptr()) };
        }
    }
}

impl Decoder {
    fn configure(&mut self, preset: Option<&'static [u8]>) -> Result<(), String> {
        let wanted = preset.map(key);
        if self.selected == wanted {
            return Ok(());
        }
        let dict = if let Some(bytes) = preset {
            let id = key(bytes);
            match self.dictionaries.iter().find(|(k, _)| *k == id) {
                Some((_, handle)) => handle.as_ptr(),
                None => {
                    let handle =
                        NonNull::new(unsafe { ZSTD_createDDict(bytes.as_ptr(), bytes.len()) })
                            .ok_or("zstd decompression dictionary allocation failed")?;
                    self.dictionaries.push((id, handle));
                    handle.as_ptr()
                }
            }
        } else {
            std::ptr::null_mut()
        };
        check(unsafe { ZSTD_DCtx_refDDict(self.ctx.as_ptr(), dict) })?;
        self.selected = wanted;
        Ok(())
    }
}
thread_local! {
    static ENCODER: RefCell<Option<Encoder>> = const { RefCell::new(None) };
    static DECODER: RefCell<Option<Decoder>> = const { RefCell::new(None) };
}
fn check(n: usize) -> Result<usize, String> {
    if unsafe { ZSTD_isError(n) } == 0 {
        return Ok(n);
    }
    Err(unsafe { CStr::from_ptr(ZSTD_getErrorName(n)) }
        .to_string_lossy()
        .into_owned())
}

/// Compress one independent frame using content size and the Zstandard checksum.
pub fn plain_compress(input: &[u8], level: i32) -> Result<Vec<u8>, String> {
    compress_frame(input, level, false)
}

pub(crate) fn compress_frame(input: &[u8], level: i32, history: bool) -> Result<Vec<u8>, String> {
    encode_frame(input, level, history.then_some(crate::words::HISTORY))
}

pub(crate) fn compress_with_dict(
    input: &[u8],
    level: i32,
    dict: &'static [u8],
) -> Result<Vec<u8>, String> {
    encode_frame(input, level, Some(dict))
}

fn encode_frame(
    input: &[u8],
    level: i32,
    preset: Option<&'static [u8]>,
) -> Result<Vec<u8>, String> {
    ENCODER.with_borrow_mut(|slot| {
        if slot.is_none() {
            *slot = Some(Encoder::new()?);
        }
        let encoder = slot.as_mut().unwrap();
        encoder.configure(level, preset)?;
        let ctx = encoder.ctx.as_ptr();
        let mut out = Vec::new();
        out.try_reserve_exact(unsafe { ZSTD_compressBound(input.len()) })
            .map_err(|_| "zstd compression buffer too large")?;
        let n = check(unsafe {
            ZSTD_compress2(
                ctx,
                out.as_mut_ptr(),
                out.capacity(),
                input.as_ptr(),
                input.len(),
            )
        })?;
        // libzstd initialized precisely the returned prefix of the buffer.
        unsafe {
            out.set_len(n);
        }
        Ok(out)
    })
}

pub(crate) fn content_len(frame: &[u8], limit: usize) -> Result<usize, String> {
    if frame.get(..4) != Some(&[0x28, 0xb5, 0x2f, 0xfd]) || frame.get(4).is_none_or(|v| v & 4 == 0)
    {
        return Err("expected a checksummed Zstandard frame".into());
    }
    let n = unsafe { ZSTD_getFrameContentSize(frame.as_ptr(), frame.len()) };
    if n >= u64::MAX - 1 || n > limit as u64 {
        return Err("invalid or excessive Zstandard content size".into());
    }
    Ok(n as usize)
}

/// Decompress one checksummed frame with an exact expected output length.
pub fn plain_decompress(frame: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    decompress_frame(frame, expected, false)
}

pub(crate) fn decompress_frame(
    frame: &[u8],
    expected: usize,
    history: bool,
) -> Result<Vec<u8>, String> {
    decode_frame(frame, expected, history.then_some(crate::words::HISTORY))
}

pub(crate) fn decompress_with_dict(
    frame: &[u8],
    expected: usize,
    dict: &'static [u8],
) -> Result<Vec<u8>, String> {
    decode_frame(frame, expected, Some(dict))
}

fn decode_frame(
    frame: &[u8],
    expected: usize,
    preset: Option<&'static [u8]>,
) -> Result<Vec<u8>, String> {
    if content_len(frame, expected)? != expected
        || check(unsafe { ZSTD_findFrameCompressedSize(frame.as_ptr(), frame.len()) })?
            != frame.len()
    {
        return Err("Zstandard frame length mismatch".into());
    }
    DECODER.with_borrow_mut(|slot| {
        if slot.is_none() {
            let ctx = NonNull::new(unsafe { ZSTD_createDCtx() })
                .ok_or("zstd decoder allocation failed")?;
            *slot = Some(Decoder {
                ctx,
                dictionaries: Vec::new(),
                selected: None,
            });
        }
        let decoder = slot.as_mut().unwrap();
        decoder.configure(preset)?;
        let ctx = decoder.ctx.as_ptr();
        let mut out = Vec::new();
        out.try_reserve_exact(expected)
            .map_err(|_| "zstd decompression buffer too large")?;
        let n = check(unsafe {
            ZSTD_decompressDCtx(ctx, out.as_mut_ptr(), expected, frame.as_ptr(), frame.len())
        })?;
        if n != expected {
            return Err("Zstandard output length mismatch".into());
        }
        unsafe {
            out.set_len(n);
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_preset_supplies_the_dictionary_id_out_of_band() {
        static DICT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/preset-v2.zdict"));
        assert_eq!(&DICT[..4], &[0x37, 0xa4, 0x30, 0xec]);
        assert_ne!(&DICT[4..8], &[0; 4]);
        let input = &DICT[DICT.len() - 512..];
        let frame = compress_with_dict(input, 3, DICT).unwrap();
        assert_eq!(frame[4] & 3, 0);
        assert_eq!(
            decompress_with_dict(&frame, input.len(), DICT).unwrap(),
            input
        );
        assert!(plain_decompress(&frame, input.len()).is_err());
    }

    #[test]
    fn sticky_context_switches_between_independent_presets_and_plain() {
        static A: &[u8] = b"An uncommon sample document contains this particular header.\nA different line follows the header.";
        static B: &[u8] = b"Independent vocabulary for another category.\nThe opening clause remains an exact match.";
        for level in [1, 3, 3, 9, 3] {
            for dictionary in [A, B, A] {
                let frame = compress_with_dict(dictionary, level, dictionary).unwrap();
                let packed = plain_compress(dictionary, level).unwrap();
                assert_eq!(
                    plain_decompress(&packed, dictionary.len()).unwrap(),
                    dictionary
                );
                assert_eq!(
                    decompress_with_dict(&frame, dictionary.len(), dictionary).unwrap(),
                    dictionary
                );
            }
        }
    }
}
