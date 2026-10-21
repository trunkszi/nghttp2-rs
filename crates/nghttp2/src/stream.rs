//! Stream introspection.
//!
//! `nghttp2_stream` is an opaque handle returned by `nghttp2_session_find_stream`.
//! It exposes the stream identifier, protocol state, and flow-control windows
//! without transferring ownership; the handle becomes invalid when the session is
//! destroyed or the stream is removed.
//!
//! Queries through a stream handle are read-only and do not affect the session's
//! scheduling decisions.

pub use nghttp2_sys::nghttp2_stream;

/// Opaque native stream.
pub type RawStream = nghttp2_stream;

/// Native stream protocol state.
pub type RawStreamState = nghttp2_sys::nghttp2_stream_proto_state;

/// Returns the numeric stream identifier, or `0` for a null handle.
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
