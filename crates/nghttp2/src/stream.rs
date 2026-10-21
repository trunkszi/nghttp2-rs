//! Stream introspection.
//!
//! `nghttp2_stream` is an opaque handle returned by `nghttp2_session_find_stream`.
//! It identifies a stream without transferring ownership. Except for the root
//! stream (ID 0), a returned handle is valid only until the next call to
//! `nghttp2_session_send`, `nghttp2_session_mem_send2`, `nghttp2_session_recv`, or
//! `nghttp2_session_mem_recv2`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_stream_has_no_id() {
        // SAFETY: the function explicitly accepts a null handle.
        assert_eq!(unsafe { stream_id(core::ptr::null_mut()) }, 0);
    }
}
