//! Reference-counted byte buffers owned by `nghttp2`.
//!
//! A received header name or value can point into an `nghttp2_rcbuf`. The C library
//! keeps the buffer alive at least for the duration of the callback that delivers
//! it, and may retain it until the header block ends; a caller that keeps the bytes
//! must take a reference of its own and release it when the buffer is no longer
//! needed.
//!
//! [`RcBuf`] is that reference: [`RcBuf::from_raw`] increments the count,
//! [`RcBuf::as_slice`] borrows the bytes, and the final Rust handle releases the
//! acquired native reference exactly once. Buffers allocated statically by the library report
//! `true` from [`RcBuf::is_static`]; they carry no reference count, so taking and releasing
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
/// A group of Rust clones shares one acquired native reference. The final Rust
/// handle releases that reference. Neither the wrapper nor its clones are `Send`
/// or `Sync`; native reference-count operations must remain serialized.
#[derive(Clone)]
pub struct RcBuf {
    inner: std::rc::Rc<Inner>,
}

struct Inner {
    raw: core::ptr::NonNull<RawRcBuf>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // SAFETY: this guard owns one acquired native reference; all Rust clones
        // are gone, and the raw-constructor contract keeps its allocator alive.
        unsafe { nghttp2_sys::nghttp2_rcbuf_decref(self.raw.as_ptr()) };
    }
}

impl RcBuf {
    /// Takes a reference to an `nghttp2` buffer and wraps it.
    ///
    /// Returns `None` when `raw` is null. Otherwise the reference count is
    /// incremented, which is a no-op for statically allocated buffers, and the
    /// final Rust clone releases that reference when it is dropped.
    ///
    /// # Safety
    ///
    /// `raw` must be null or a live pointer to an [`nghttp2_rcbuf`] that `nghttp2`
    /// currently keeps allocated, such as a name or value pointer passed to an
    /// `nghttp2_on_header_callback2` invocation. The buffer must not have been freed
    /// by an earlier `nghttp2_rcbuf_decref` call. Its bytes must be initialized,
    /// immutable, and readable for the lifetime of every resulting handle/borrow.
    /// A dynamic buffer's native count must have room for one increment.
    ///
    /// A custom allocator's free callback must not unwind; its user data and backing
    /// resources must remain valid until the last Rust clone is dropped, even after
    /// the originating session has been destroyed. Cloning only changes Rust's
    /// checked reference count; it does not acquire more native references.
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
        Some(Self {
            inner: std::rc::Rc::new(Inner { raw }),
        })
    }

    /// Returns the bytes stored in the buffer.
    ///
    /// The slice borrows the buffer through `self` and remains valid for as long as
    /// the borrow lives.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `self.inner` keeps the native reference alive during the borrow and
        // nghttp2 does not mutate the bytes through any other path.
        let view = unsafe { nghttp2_sys::nghttp2_rcbuf_get_buf(self.inner.raw.as_ptr()) };
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
    /// [`RcBuf::from_raw`] and the final native decrement do nothing, and the
    /// bytes remain valid for the lifetime of the process.
    #[must_use]
    pub fn is_static(&self) -> bool {
        // SAFETY: the shared Inner keeps the native buffer live during the borrow.
        unsafe { nghttp2_sys::nghttp2_rcbuf_is_static(self.inner.raw.as_ptr().cast_const()) != 0 }
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
        assert_eq!(ref_count(raw), 2);
        drop(buffer);
        assert_eq!(ref_count(raw), 2);
        let buffer = clone;
        assert_eq!(buffer.as_slice(), b"hello");

        drop(buffer);
        assert_eq!(ref_count(raw), 1);
        // SAFETY: this releases the reference returned by `nghttp2_rcbuf_new2`.
        unsafe { nghttp2_sys::nghttp2_rcbuf_decref(raw) };
    }

    #[test]
    fn forgotten_clones_do_not_increment_native() {
        // SAFETY: the default allocator is static; this test owns the initial reference.
        let raw = unsafe { new_buffer(b"bounded") };
        // SAFETY: no other thread accesses this live buffer, and there is count headroom.
        let buffer = unsafe { RcBuf::from_raw(raw) }.unwrap();
        let inner = std::rc::Rc::into_raw(std::rc::Rc::clone(&buffer.inner));
        for _ in 1..10_000 {
            core::mem::forget(buffer.clone());
        }
        assert_eq!(ref_count(raw), 2);
        assert_eq!(std::rc::Rc::strong_count(&buffer.inner), 10_001);
        for _ in 0..10_000 {
            // SAFETY: into_raw supplies this allocation's pointer; its retained
            // count plus the forgotten clones total 10,000 owned strong counts.
            // The surviving buffer keeps the allocation alive throughout cleanup.
            unsafe { std::rc::Rc::decrement_strong_count(inner) };
        }
        drop(buffer);
        assert_eq!(ref_count(raw), 1);
        // SAFETY: the original native reference is still owned by this test.
        unsafe { nghttp2_sys::nghttp2_rcbuf_decref(raw) };
    }

    #[test]
    fn cloning_at_native_ceiling_is_safe() {
        // SAFETY: the default allocator and buffer remain live until explicit release.
        let raw = unsafe { new_buffer(b"ceiling") };
        // SAFETY: the test-only layout mirrors the live native allocation. No
        // concurrent accesses occur; restore the real count before native release.
        unsafe { (*raw.cast::<RcBufLayout>()).ref_count = i32::MAX - 1 };
        // SAFETY: the initialized immutable buffer has one increment of headroom.
        let buffer = unsafe { RcBuf::from_raw(raw) }.unwrap();
        let clone = buffer.clone();
        assert_eq!(ref_count(raw), i32::MAX);
        drop(clone);
        assert_eq!(ref_count(raw), i32::MAX);
        // SAFETY: remove the simulated external references before dropping owners.
        unsafe { (*raw.cast::<RcBufLayout>()).ref_count = 2 };
        drop(buffer);
        assert_eq!(ref_count(raw), 1);
        // SAFETY: release the original allocation's one remaining reference.
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
