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
//! therefore cannot alias the owning session's mutable borrow. A call that
//! dispatches another callback is refused by the handler's re-entry guard.
//!
//! The handle is deliberately neither `Copy` nor `Clone`, and its constructor is
//! crate-private. A handler receives it as `&mut SessionCtx<'_>`, so it cannot move
//! the handle out or keep the borrow past the callback. `NonNull` also makes the
//! handle `!Send` and `!Sync`, independent of the callbacks' thread affinity.

use core::{marker::PhantomData, ptr::NonNull};

pub use nghttp2_sys::nghttp2_session;

/// Opaque native session.
pub type RawSession = nghttp2_session;

/// Non-owning native session handle with a caller-established validity window.
///
/// Safe code cannot construct this handle from an arbitrary pointer:
///
/// ```compile_fail
/// use nghttp2::context::{RawSession, SessionPtr};
/// let raw = core::ptr::NonNull::<RawSession>::dangling();
/// let session = SessionPtr::from_raw(raw);
/// ```
///
/// The handle cannot be copied to detach it from its borrow:
///
/// ```compile_fail
/// use nghttp2::context::SessionPtr;
/// fn copy_handle<'a>(session: &SessionPtr<'a>) -> SessionPtr<'a> {
///     *session
/// }
/// ```
pub struct SessionPtr<'a> {
    raw: NonNull<nghttp2_session>,
    _lifetime: PhantomData<&'a mut nghttp2_session>,
}

impl<'a> SessionPtr<'a> {
    /// Establishes a validity window for a native session pointer.
    ///
    /// # Safety
    ///
    /// `raw` must identify a live session throughout `'a`. The session must remain
    /// on its owning thread and may not be destroyed during that window. Native
    /// calls through other handles must respect nghttp2's re-entry rules and must
    /// not invalidate a stream while a view borrowed from this handle is in use.
    pub const unsafe fn from_raw(raw: NonNull<nghttp2_session>) -> Self {
        Self {
            raw,
            _lifetime: PhantomData,
        }
    }

    /// Returns the native pointer without transferring ownership.
    #[must_use]
    pub const fn as_ptr(&self) -> *mut nghttp2_session {
        self.raw.as_ptr()
    }
}

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
    /// handle must not escape the callback invocation when constructed there.
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
#[path = "../tests/support/context.rs"]
mod tests;
