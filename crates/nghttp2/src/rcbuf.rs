//! Reference-counted byte buffers owned by `nghttp2`.
//!
//! A received header name or value can point into an `nghttp2_rcbuf`. The C library
//! owns one reference for the duration of the callback that delivers it and releases
//! that reference when the callback returns, so a caller that keeps the bytes must
//! take a reference of its own and release it when the buffer is no longer needed.
//!
//! [`RcBuf`] is that reference: [`RcBuf::from_raw`] increments the count,
//! [`RcBuf::as_slice`] borrows the bytes, and the [`Drop`] implementation decrements
//! the count exactly once. Buffers allocated statically by the library report `true`
//! from [`RcBuf::is_static`]; they carry no reference count, so taking and releasing
//! a reference is a no-op and the bytes live for the lifetime of the process. A
//! dynamically allocated buffer is freed when its last reference is released.
//!
//! This module also names the opaque native type and the borrowed view returned by
//! `nghttp2_rcbuf_get_buf`.

pub use nghttp2_sys::nghttp2_rcbuf;

/// Opaque native reference-counted buffer.
pub type RawRcBuf = nghttp2_rcbuf;

/// Name/value bytes borrowed from an [`nghttp2_rcbuf`].
pub type RawBufferView = nghttp2_sys::nghttp2_vec;

/// Owned reference to an [`nghttp2_rcbuf`].
///
/// The wrapper keeps one reference count alive for as long as it lives and releases
/// that reference on drop. It is neither `Send` nor `Sync` because the native object
/// is opaque and the library makes no thread-safety guarantees about it.
pub struct RcBuf {
    raw: core::ptr::NonNull<RawRcBuf>,
}

impl RcBuf {
    /// Takes a reference to an `nghttp2` buffer and wraps it.
    ///
    /// Returns `None` when `raw` is null. Otherwise the reference count is
    /// incremented, which is a no-op for statically allocated buffers, and the
    /// returned wrapper releases that reference when it is dropped.
    ///
    /// # Safety
    ///
    /// `raw` must be null or a live pointer to an [`nghttp2_rcbuf`] that `nghttp2`
    /// currently keeps allocated, such as a name or value pointer passed to an
    /// `nghttp2_on_header_callback2` invocation. The buffer must not have been freed
    /// by an earlier `nghttp2_rcbuf_decref` call.
    ///
    /// The reference taken here belongs to the returned wrapper; the caller must not
    /// pass the same acquisition to `nghttp2_rcbuf_decref`. A reference the library
    /// itself holds, including the one it keeps for the duration of a callback,
    /// remains the library's to release.
    pub unsafe fn from_raw(raw: *mut RawRcBuf) -> Option<Self> {
        let raw = core::ptr::NonNull::new(raw)?;
        // SAFETY: `NonNull::new` rejected the null pointer, and the caller
        // guarantees that the remaining pointer is a live nghttp2-managed buffer.
        unsafe { nghttp2_sys::nghttp2_rcbuf_incref(raw.as_ptr()) };
        Some(Self { raw })
    }

    /// Returns the bytes stored in the buffer.
    ///
    /// The slice borrows the buffer through `self` and remains valid for as long as
    /// the borrow lives.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `self.raw` stays allocated for the duration of the borrow and
        // nghttp2 does not mutate the bytes through any other path.
        let view = unsafe { nghttp2_sys::nghttp2_rcbuf_get_buf(self.raw.as_ptr()) };
        if view.len == 0 {
            return &[];
        }
        // SAFETY: `view.base` addresses `view.len` initialized bytes owned by the
        // buffer, and the borrow of `self` keeps that buffer alive.
        unsafe { core::slice::from_raw_parts(view.base.cast_const(), view.len) }
    }

    /// Returns whether the buffer was allocated statically by the library.
    ///
    /// Static buffers carry no reference count: the increments in
    /// [`RcBuf::from_raw`] and [`Clone`] as well as the decrement in [`Drop`] do
    /// nothing, and the bytes remain valid for the lifetime of the process.
    #[must_use]
    pub fn is_static(&self) -> bool {
        // SAFETY: `self.raw` is a live buffer for the duration of the borrow.
        unsafe { nghttp2_sys::nghttp2_rcbuf_is_static(self.raw.as_ptr().cast_const()) != 0 }
    }
}

impl Clone for RcBuf {
    fn clone(&self) -> Self {
        // SAFETY: `self` keeps a reference alive for the duration of this borrow, so
        // the buffer is live. For static buffers the call does nothing.
        unsafe { nghttp2_sys::nghttp2_rcbuf_incref(self.raw.as_ptr()) };
        Self { raw: self.raw }
    }
}

impl Drop for RcBuf {
    fn drop(&mut self) {
        // SAFETY: `from_raw` acquires one reference per wrapper and `Clone` acquires
        // one per clone, so releasing exactly one reference here matches the count
        // this value owns. For static buffers the call does nothing.
        unsafe { nghttp2_sys::nghttp2_rcbuf_decref(self.raw.as_ptr()) };
    }
}

impl AsRef<[u8]> for RcBuf {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl core::fmt::Debug for RcBuf {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("RcBuf").field(&self.as_slice()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_pointer_yields_none() {
        // SAFETY: a null pointer is one of the accepted inputs; no buffer is
        // dereferenced.
        let buffer = unsafe { RcBuf::from_raw(core::ptr::null_mut()) };
        assert!(buffer.is_none());
    }

    #[test]
    fn rc_buf_exposes_a_byte_slice() {
        // Only the trait bound is checked here. A live `RcBuf` can be created solely
        // from a buffer delivered to a session callback, and faking an nghttp2-owned
        // object would violate the `from_raw` contract; the integration suite
        // exercises real buffers instead.
        fn assert_as_ref<T: AsRef<[u8]>>() {}
        assert_as_ref::<RcBuf>();
    }
}
