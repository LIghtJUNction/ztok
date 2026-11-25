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

struct Encoder(NonNull<c_void>, Option<(i32, NonNull<c_void>)>);
struct Decoder(NonNull<c_void>, Option<NonNull<c_void>>);
impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { ZSTD_freeCCtx(self.0.as_ptr()) };
        if let Some((_, d)) = self.1 {
            unsafe { ZSTD_freeCDict(d.as_ptr()) };
        }
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { ZSTD_freeDCtx(self.0.as_ptr()) };
        if let Some(d) = self.1 {
            unsafe { ZSTD_freeDDict(d.as_ptr()) };
        }
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
    ENCODER.with_borrow_mut(|slot| {
        if slot.is_none() {
            *slot = Some(Encoder(
                NonNull::new(unsafe { ZSTD_createCCtx() })
                    .ok_or("zstd encoder allocation failed")?,
                None,
            ));
        }
        let encoder = slot.as_mut().unwrap();
        let ctx = encoder.0.as_ptr();
        if history && encoder.1.is_none_or(|(cached, _)| cached != level) {
            // Detach the old dictionary before releasing it.
            check(unsafe { ZSTD_CCtx_refCDict(ctx, std::ptr::null()) })?;
            let bytes = crate::words::HISTORY;
            let new = NonNull::new(unsafe { ZSTD_createCDict(bytes.as_ptr(), bytes.len(), level) })
                .ok_or("zstd compression dictionary allocation failed")?;
            if let Some((_, old)) = encoder.1.replace((level, new)) {
                unsafe { ZSTD_freeCDict(old.as_ptr()) };
            }
        }
        let dict = encoder
            .1
            .filter(|_| history)
            .map_or(std::ptr::null(), |(_, d)| d.as_ptr());
        check(unsafe { ZSTD_CCtx_refCDict(ctx, dict) })?;
        // ZSTD_c_compressionLevel and ZSTD_c_checksumFlag from zstd.h.
        check(unsafe { ZSTD_CCtx_setParameter(ctx, 100, level) })?;
        check(unsafe { ZSTD_CCtx_setParameter(ctx, 201, 1) })?;
        let mut out = Vec::with_capacity(unsafe { ZSTD_compressBound(input.len()) });
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
    if n > limit as u64 {
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
    if content_len(frame, expected)? != expected
        || check(unsafe { ZSTD_findFrameCompressedSize(frame.as_ptr(), frame.len()) })?
            != frame.len()
    {
        return Err("Zstandard frame length mismatch".into());
    }
    DECODER.with_borrow_mut(|slot| {
        if slot.is_none() {
            *slot = Some(Decoder(
                NonNull::new(unsafe { ZSTD_createDCtx() })
                    .ok_or("zstd decoder allocation failed")?,
                None,
            ));
        }
        let decoder = slot.as_mut().unwrap();
        let ctx = decoder.0.as_ptr();
        if history && decoder.1.is_none() {
            let bytes = crate::words::HISTORY;
            decoder.1 = Some(
                NonNull::new(unsafe { ZSTD_createDDict(bytes.as_ptr(), bytes.len()) })
                    .ok_or("zstd decompression dictionary allocation failed")?,
            );
        }
        let dict = decoder
            .1
            .filter(|_| history)
            .map_or(std::ptr::null(), |d| d.as_ptr());
        check(unsafe { ZSTD_DCtx_refDDict(ctx, dict) })?;
        let mut out = Vec::with_capacity(expected);
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
