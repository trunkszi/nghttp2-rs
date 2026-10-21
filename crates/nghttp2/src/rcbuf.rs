//! Reference-counted byte buffers owned by `nghttp2`.
//!
//! A received header name or value can point into an `nghttp2_rcbuf`. The C library
//! owns one reference for the duration of the callback and releases it afterwards,
//! so a caller that keeps the bytes must increment the count first and decrement it
//! when the buffer is no longer needed.
//!
//! This module names the opaque native type and the borrowed view returned by
//! `nghttp2_rcbuf_get_buf`; the RAII wrapper will build on them.

pub use nghttp2_sys::nghttp2_rcbuf;

/// Opaque native reference-counted buffer.
pub type RawRcBuf = nghttp2_rcbuf;

/// Name/value bytes borrowed from an [`nghttp2_rcbuf`].
pub type RawBufferView = nghttp2_sys::nghttp2_vec;

/// Returns whether a buffer was allocated statically by the library.
///
/// Static buffers never need a reference-count update for the lifetime of the
/// process.
///
/// # Safety
///
/// `rcbuf` must be a live, non-null pointer obtained from `nghttp2`.
#[must_use]
pub unsafe fn is_static(rcbuf: *const RawRcBuf) -> bool {
    // SAFETY: the caller guarantees a live nghttp2-managed buffer.
    unsafe { nghttp2_sys::nghttp2_rcbuf_is_static(rcbuf) != 0 }
}
