//! Non-owning session handle passed to callbacks.
//!
//! `nghttp2` invokes the installed Rust callbacks synchronously on the session's
//! thread while it processes a frame. A callback often needs to submit another
//! frame or query flow-control state on the same `nghttp2_session`. Reconstructing
//! the owning `Session` there would create a second mutable reference to it.
//!
//! [`SessionCtx`] answers that need without owning anything. It stores the raw
//! pointer and a `PhantomData<&'a mut nghttp2_session>`, so it is disjoint from the
//! `&mut Session` that is driving the library: the phantom borrow is over the
//! native `nghttp2_session`, which no Rust reference points to, not over the
//! wrapper value that holds the pointer. A re-entrant call made inside a callback
//! therefore cannot alias the owning session's mutable borrow.
//!
//! The handle is deliberately neither `Copy` nor `Clone`. One value exists per
//! callback invocation, and duplicating it would make it easy to keep a pointer
//! past its validity window. That window is the callback invocation itself: the
//! trampoline chooses `'a` so that it ends when the callback returns, so safe code
//! cannot store the handle or use it from another thread. `SessionCtx` is `!Send`
//! and `!Sync` because callbacks run synchronously on the session's thread.

use core::{marker::PhantomData, ptr::NonNull};

pub use nghttp2_sys::nghttp2_session;

/// Opaque native session.
pub type RawSession = nghttp2_session;

/// Non-null pointer to a native session.
pub type SessionPtr = core::ptr::NonNull<nghttp2_session>;

/// Non-null pointer to a native stream owned by a session.
pub type StreamPtr = core::ptr::NonNull<nghttp2_sys::nghttp2_stream>;

/// Non-owning handle to the session whose callback is currently running.
///
/// The handle borrows the native session for `'a` at the type level and holds no
/// ownership of it. It is created only by the crate's callback trampolines and
/// session layer; user code receives one as an argument and can use it to call
/// back into the library.
pub struct SessionCtx<'a> {
    raw: NonNull<nghttp2_session>,
    _lifetime: PhantomData<&'a mut nghttp2_session>,
}

impl<'a> SessionCtx<'a> {
    /// Wraps the raw session pointer delivered to a callback.
    ///
    /// Returns `None` when `raw` is null.
    ///
    /// # Safety
    ///
    /// `raw` must be null or a live session pointer that outlives `'a`. nghttp2
    /// invokes callbacks synchronously on the session's thread, so the returned
    /// handle must not escape the callback invocation.
    pub(crate) unsafe fn from_raw(raw: *mut nghttp2_session) -> Option<Self> {
        Some(Self {
            raw: NonNull::new(raw)?,
            _lifetime: PhantomData,
        })
    }

    /// Returns the native session pointer.
    ///
    /// The pointer is valid only for the callback invocation that produced this
    /// handle.
    #[must_use]
    pub fn as_ptr(&self) -> *mut nghttp2_session {
        self.raw.as_ptr()
    }
}

impl core::fmt::Debug for SessionCtx<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Print the address only: the native session is opaque and must not be
        // reachable through formatting.
        f.debug_tuple("SessionCtx")
            .field(&self.raw.as_ptr())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_pointer_yields_no_context() {
        let raw: *mut nghttp2_session = core::ptr::null_mut();
        // SAFETY: `from_raw` explicitly accepts a null pointer and reports it as
        // `None`.
        let ctx = unsafe { SessionCtx::from_raw(raw) };
        assert!(ctx.is_none());
    }

    #[test]
    fn non_null_pointer_round_trips_without_dereferencing() {
        let raw = NonNull::<nghttp2_session>::dangling().as_ptr();
        // SAFETY: the handle stores the pointer and only hands it back; neither
        // `as_ptr` nor the `Debug` impl dereferences it, so this dangling address
        // never reaches a session.
        let ctx = unsafe { SessionCtx::from_raw(raw) }.expect("non-null pointer");
        assert_eq!(ctx.as_ptr(), raw);
        assert_eq!(format!("{ctx:?}"), format!("SessionCtx({raw:p})"));
    }
}
