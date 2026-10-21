//! Reference-counted byte buffers owned by `nghttp2`.
//!
//! A received header name or value can point into an `nghttp2_rcbuf`. The C library
//! keeps the buffer alive at least for the duration of the callback that delivers
//! it, and may retain it until the header block ends; a caller that keeps the bytes
//! must take a reference of its own and release it when the buffer is no longer
//! needed.
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
    ///
    /// The native reference count is not atomic, so the caller must also ensure that
    /// no other thread increments or decrements this buffer while this call or the
    /// returned wrapper uses it.
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
    use core::ffi::{c_int, c_void};

    use super::*;

    // `libnghttp2` exports these helpers, but they live in private headers
    // (lib/nghttp2_rcbuf.h and lib/nghttp2_mem.h), so they are not part of the
    // public bindings. The tests use them to create real buffers without a session.
    unsafe extern "C" {
        fn nghttp2_mem_default() -> *mut nghttp2_sys::nghttp2_mem;
        fn nghttp2_rcbuf_new(
            rcbuf_ptr: *mut *mut RawRcBuf,
            size: usize,
            mem: *mut nghttp2_sys::nghttp2_mem,
        ) -> c_int;
        fn nghttp2_rcbuf_new2(
            rcbuf_ptr: *mut *mut RawRcBuf,
            src: *const u8,
            srclen: usize,
            mem: *mut nghttp2_sys::nghttp2_mem,
        ) -> c_int;
    }

    /// Test-only mirror of the private `struct nghttp2_rcbuf`. The layout is
    /// identical in the vendored master and v1.70.0 sources.
    #[repr(C)]
    struct RcBufLayout {
        _mem_user_data: *mut c_void,
        _free: *mut c_void,
        _base: *mut u8,
        _len: usize,
        ref_count: i32,
    }

    const _: () = assert!(core::mem::size_of::<RcBufLayout>() == 40);
    const _: () = assert!(core::mem::offset_of!(RcBufLayout, ref_count) == 32);

    fn ref_count(raw: *mut RawRcBuf) -> i32 {
        // SAFETY: the caller passes a live buffer and `RcBufLayout` mirrors the C
        // layout used by the linked library.
        unsafe { (*raw.cast::<RcBufLayout>()).ref_count }
    }

    unsafe fn new_buffer(src: &[u8]) -> *mut RawRcBuf {
        let mut raw = core::ptr::null_mut();
        // SAFETY: `nghttp2_mem_default` returns a static allocator; `new2` copies
        // `src` into a buffer whose reference count starts at one.
        let result = unsafe {
            nghttp2_rcbuf_new2(&raw mut raw, src.as_ptr(), src.len(), nghttp2_mem_default())
        };
        assert_eq!(result, 0, "nghttp2_rcbuf_new2 failed");
        assert!(!raw.is_null());
        raw
    }

    #[test]
    fn null_pointer_yields_none() {
        // SAFETY: a null pointer is one of the accepted inputs; no buffer is
        // dereferenced.
        let buffer = unsafe { RcBuf::from_raw(core::ptr::null_mut()) };
        assert!(buffer.is_none());
    }

    #[test]
    fn live_buffer_exposes_bytes_and_balances_references() {
        // SAFETY: `new_buffer` returns a live buffer with one reference, which this
        // test releases explicitly at the end.
        let raw = unsafe { new_buffer(b"hello") };
        assert_eq!(ref_count(raw), 1);

        // SAFETY: `raw` is live and owned by this test.
        let buffer = unsafe { RcBuf::from_raw(raw) }.expect("non-null buffer");
        assert_eq!(ref_count(raw), 2);
        assert_eq!(buffer.as_slice(), b"hello");
        assert_eq!(buffer.as_ref(), b"hello");
        assert!(!buffer.is_static());

        let clone = buffer.clone();
        assert_eq!(ref_count(raw), 3);
        drop(clone);
        assert_eq!(ref_count(raw), 2);
        assert_eq!(buffer.as_slice(), b"hello");

        drop(buffer);
        assert_eq!(ref_count(raw), 1);
        // SAFETY: this releases the reference returned by `nghttp2_rcbuf_new2`.
        unsafe { nghttp2_sys::nghttp2_rcbuf_decref(raw) };
    }

    #[test]
    fn zero_length_buffer_is_empty_and_debuggable() {
        let mut raw = core::ptr::null_mut();
        // SAFETY: `nghttp2_mem_default` returns a static allocator; `new` allocates
        // a buffer whose reference count starts at one.
        let result = unsafe { nghttp2_rcbuf_new(&raw mut raw, 0, nghttp2_mem_default()) };
        assert_eq!(result, 0, "nghttp2_rcbuf_new failed");
        assert!(!raw.is_null());

        // SAFETY: `raw` is live and owned by this test.
        let buffer = unsafe { RcBuf::from_raw(raw) }.expect("non-null buffer");
        assert!(buffer.as_slice().is_empty());
        assert_eq!(format!("{buffer:?}"), "RcBuf([])");
        assert_eq!(ref_count(raw), 2);

        drop(buffer);
        assert_eq!(ref_count(raw), 1);
        // SAFETY: this releases the reference returned by `nghttp2_rcbuf_new`.
        unsafe { nghttp2_sys::nghttp2_rcbuf_decref(raw) };
    }

    #[test]
    fn static_buffer_is_refcount_neutral() {
        static NAME: &[u8] = b":method";
        let mut raw = RcBufLayout {
            _mem_user_data: core::ptr::null_mut(),
            _free: core::ptr::null_mut(),
            _base: NAME.as_ptr().cast_mut(),
            _len: NAME.len(),
            ref_count: -1,
        };
        // SAFETY: the stack object mirrors the private layout, and `ref == -1` marks
        // a static buffer; both incref and decref return immediately for that value,
        // so nghttp2 never frees the stack storage.
        let buffer =
            unsafe { RcBuf::from_raw((&raw mut raw).cast::<RawRcBuf>()) }.expect("non-null buffer");
        assert!(buffer.is_static());
        assert_eq!(buffer.as_slice(), NAME);

        let clone = buffer.clone();
        assert_eq!(raw.ref_count, -1);
        drop(clone);
        assert_eq!(raw.ref_count, -1);
        drop(buffer);
        assert_eq!(raw.ref_count, -1);
    }

    #[test]
    fn rc_buf_exposes_a_byte_slice() {
        fn assert_as_ref<T: AsRef<[u8]>>() {}
        assert_as_ref::<RcBuf>();
    }
}
