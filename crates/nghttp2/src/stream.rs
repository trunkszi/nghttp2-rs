//! Stream introspection.
//!
//! `nghttp2_stream` is an opaque handle returned by `nghttp2_session_find_stream`.
//! It identifies a stream without transferring ownership. Except for the root
//! stream (ID 0), a returned handle is valid only until the next call to
//! `nghttp2_session_send`, `nghttp2_session_mem_send2`, `nghttp2_session_recv`, or
//! `nghttp2_session_mem_recv2`. Those calls may close and free streams, so a
//! handle retained past them can dangle.
//!
//! Queries through a stream handle are read-only and do not affect the session's
//! scheduling decisions.

use core::marker::PhantomData;

pub use nghttp2_sys::nghttp2_stream;

use crate::{
    context::{SessionCtx, SessionPtr, StreamPtr},
    types::StreamProtoState,
};

/// Opaque native stream.
pub type RawStream = nghttp2_stream;

/// Native stream protocol state.
pub type RawStreamState = nghttp2_sys::nghttp2_stream_proto_state;

/// Returns the numeric stream identifier, or `0` for a null handle.
///
/// A null handle and the root stream both report `0`; callers that need to tell
/// them apart must test the pointer before calling this function.
///
/// # Safety
///
/// `stream` must be null or a live handle owned by a session.
#[must_use]
pub unsafe fn stream_id(stream: *mut RawStream) -> i32 {
    if stream.is_null() {
        return 0;
    }
    // SAFETY: the caller guarantees a live stream handle.
    unsafe { nghttp2_sys::nghttp2_stream_get_stream_id(stream) }
}

/// Non-owning view of a stream owned by a session.
///
/// The handle is valid only until the next send/receive call on the session
/// (`nghttp2_session_send`, `nghttp2_session_mem_send2`, `nghttp2_session_recv`,
/// or `nghttp2_session_mem_recv2`); the root stream is the exception and lives
/// for the duration of the process. The lifetime parameter ties the view to the
/// borrow that produced it, but that borrow is of the pointer value, not of the
/// session owner; neither the session lifetime nor the send/receive invalidation
/// window can be enforced by this type, so callers must respect both.
///
/// Two views compare equal exactly when they refer to the same native stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamRef<'a> {
    stream: StreamPtr,
    _session: PhantomData<&'a ()>,
}

impl<'a> StreamRef<'a> {
    /// Wraps a native stream pointer owned by a live session.
    ///
    /// The caller must guarantee that `stream` belongs to a session that is
    /// alive and that the view is not used across the send/receive calls listed
    /// on [`StreamRef`], except for the root stream.
    ///
    /// # Safety
    ///
    /// `stream` must be a live handle owned by a reachable session.
    pub(crate) unsafe fn from_ptr(stream: StreamPtr) -> Self {
        Self {
            stream,
            _session: PhantomData,
        }
    }

    /// Wraps an optional native stream pointer.
    ///
    /// `nghttp2` reports an absent dependency link or a missing stream as a null
    /// pointer; that maps to `None` rather than to an invalid handle.
    fn from_optional_ptr(stream: *mut RawStream) -> Option<Self> {
        let stream = StreamPtr::new(stream)?;
        // SAFETY: the public lookup that reaches this point requires a live session.
        Some(unsafe { Self::from_ptr(stream) })
    }

    /// Returns the numeric stream identifier.
    ///
    /// The root stream reports `0`; stream identifiers assigned to peer streams
    /// are positive.
    #[must_use]
    pub fn id(&self) -> i32 {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // only reads the identifier field.
        unsafe { nghttp2_sys::nghttp2_stream_get_stream_id(self.stream.as_ptr()) }
    }

    /// Returns the HTTP/2 protocol state of the stream.
    ///
    /// The state is derived from the stream's shutdown and close flags rather
    /// than set directly by the peer: a request sent with `END_STREAM` reports
    /// [`StreamProtoState::HALF_CLOSED_LOCAL`], while the root stream always
    /// reports [`StreamProtoState::IDLE`].
    #[must_use]
    pub fn state(&self) -> StreamProtoState {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // only reads the stream's state and shutdown fields.
        let raw = unsafe { nghttp2_sys::nghttp2_stream_get_state(self.stream.as_ptr()) };
        StreamProtoState::from_raw(raw)
    }

    /// Returns the RFC 7540 dependency weight of the stream.
    ///
    /// Upstream removed the RFC 7540 priority tree in favour of RFC 9218, so the
    /// native getter is a compatibility stub that reports
    /// `NGHTTP2_DEFAULT_WEIGHT` for every stream.
    #[must_use]
    pub fn weight(&self) -> i32 {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // does not dereference any caller-owned data.
        unsafe { nghttp2_sys::nghttp2_stream_get_weight(self.stream.as_ptr()) }
    }

    /// Returns the sum of the weights of the streams depending on this one.
    ///
    /// The native getter is a compatibility stub for the removed RFC 7540
    /// dependency tree and always reports `0`, which is never negative.
    #[must_use]
    pub fn sum_dependency_weight(&self) -> i32 {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // does not dereference any caller-owned data.
        unsafe { nghttp2_sys::nghttp2_stream_get_sum_dependency_weight(self.stream.as_ptr()) }
    }

    /// Returns the parent stream in the RFC 7540 dependency tree.
    ///
    /// The native getter returns null unconditionally because upstream removed
    /// that tree, so this is always `None` for the versions this crate tracks.
    #[must_use]
    pub fn parent(&self) -> Option<StreamRef<'a>> {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // only reads the parent link and returns null when there is none.
        let parent = unsafe { nghttp2_sys::nghttp2_stream_get_parent(self.stream.as_ptr()) };
        Self::from_optional_ptr(parent)
    }

    /// Returns the first child in the RFC 7540 dependency tree.
    ///
    /// The native getter returns null unconditionally because upstream removed
    /// that tree, so this is always `None` for the versions this crate tracks.
    #[must_use]
    pub fn first_child(&self) -> Option<StreamRef<'a>> {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // only reads the child link and returns null when there is none.
        let child = unsafe { nghttp2_sys::nghttp2_stream_get_first_child(self.stream.as_ptr()) };
        Self::from_optional_ptr(child)
    }

    /// Returns the next sibling in the RFC 7540 dependency tree.
    ///
    /// The native getter returns null unconditionally because upstream removed
    /// that tree, so this is always `None` for the versions this crate tracks.
    #[must_use]
    pub fn next_sibling(&self) -> Option<StreamRef<'a>> {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // only reads the sibling link and returns null when there is none.
        let sibling = unsafe { nghttp2_sys::nghttp2_stream_get_next_sibling(self.stream.as_ptr()) };
        Self::from_optional_ptr(sibling)
    }

    /// Returns the previous sibling in the RFC 7540 dependency tree.
    ///
    /// The native getter returns null unconditionally because upstream removed
    /// that tree, so this is always `None` for the versions this crate tracks.
    #[must_use]
    pub fn previous_sibling(&self) -> Option<StreamRef<'a>> {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // only reads the sibling link and returns null when there is none.
        let sibling =
            unsafe { nghttp2_sys::nghttp2_stream_get_previous_sibling(self.stream.as_ptr()) };
        Self::from_optional_ptr(sibling)
    }
}

/// Stream lookup operations borrowing a live session handle.
///
/// Returned views borrow the handle; a callback cannot retain one after its
/// context borrow ends or while that context is borrowed mutably.
///
/// ```compile_fail
/// use nghttp2::{context::SessionCtx, stream::{SessionStreams, StreamRef}};
/// fn escape<'a>(ctx: &SessionCtx<'a>) -> StreamRef<'a> {
///     ctx.find_stream(1).unwrap()
/// }
/// ```
///
/// ```compile_fail
/// use nghttp2::{context::SessionCtx, stream::SessionStreams};
/// fn mutate(_: &mut SessionCtx<'_>) {}
/// fn inspect(ctx: &mut SessionCtx<'_>) {
///     let stream = ctx.find_stream(1).unwrap();
///     mutate(ctx);
///     let _id = stream.id();
/// }
/// ```
pub trait SessionStreams {
    /// Returns the stream with the given identifier, or `None` when the session
    /// does not have one.
    ///
    /// The lookup is read-only. A request stream is registered only when its
    /// `HEADERS` frame is serialized, so it is not visible before the send call
    /// that emits it and is gone once the stream has closed.
    ///
    /// `stream_id == 0` resolves to the root stream:
    /// `nghttp2_session_find_stream` special-cases ID 0 instead of returning
    /// null, matching [`Self::root_stream`].
    fn find_stream(&self, stream_id: i32) -> Option<StreamRef<'_>>;

    /// Returns the session's root stream.
    ///
    /// The root stream is a static object outside the peer's stream numbering.
    /// It reports ID `0`, state [`StreamProtoState::IDLE`], and no dependency
    /// links, and it lives for the duration of the process, so the invalidation
    /// window of ordinary [`StreamRef`] handles does not apply to it.
    ///
    /// Returns `None` only if the native getter reports null; the versions this
    /// crate tracks always return the root object.
    fn root_stream(&self) -> Option<StreamRef<'_>>;
}

impl SessionStreams for SessionPtr<'_> {
    fn find_stream(&self, stream_id: i32) -> Option<StreamRef<'_>> {
        // SAFETY: the pointer comes from a live session; the lookup only reads
        // the session's stream map and the returned handle is not retained.
        let stream = unsafe { nghttp2_sys::nghttp2_session_find_stream(self.as_ptr(), stream_id) };
        StreamRef::from_optional_ptr(stream)
    }

    fn root_stream(&self) -> Option<StreamRef<'_>> {
        // SAFETY: the pointer comes from a live session; the root stream is a
        // static object, so the returned handle outlives the session itself.
        let stream = unsafe { nghttp2_sys::nghttp2_session_get_root_stream(self.as_ptr()) };
        StreamRef::from_optional_ptr(stream)
    }
}

impl SessionStreams for SessionCtx<'_> {
    fn find_stream(&self, stream_id: i32) -> Option<StreamRef<'_>> {
        // SAFETY: the pointer comes from a live session; the lookup only reads
        // the session's stream map and the returned handle is not retained.
        let stream = unsafe { nghttp2_sys::nghttp2_session_find_stream(self.as_ptr(), stream_id) };
        StreamRef::from_optional_ptr(stream)
    }

    fn root_stream(&self) -> Option<StreamRef<'_>> {
        // SAFETY: the pointer comes from a live session; the root stream is a
        // static object, so the returned handle outlives the session itself.
        let stream = unsafe { nghttp2_sys::nghttp2_session_get_root_stream(self.as_ptr()) };
        StreamRef::from_optional_ptr(stream)
    }
}

#[cfg(test)]
#[path = "../tests/support/stream.rs"]
mod tests;
