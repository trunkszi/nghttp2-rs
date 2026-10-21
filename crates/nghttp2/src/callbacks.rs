//! Session callbacks.
//!
//! `nghttp2_session_callbacks` is the table of function pointers installed on a
//! session before it processes frames. It covers I/O transport, frame and header
//! notifications, data supply, padding selection, extension frames, and error
//! reporting.
//!
//! Every callback that re-enters Rust must be isolated from unwinding across the C
//! stack; the safe wrapper installs trampolines that catch panics and report
//! `NGHTTP2_ERR_CALLBACK_FAILURE` to the library.

pub use nghttp2_sys::nghttp2_session_callbacks;

/// Opaque native callback table.
pub type RawSessionCallbacks = nghttp2_session_callbacks;

/// Native callback failure code returned after a caught Rust panic.
pub const CALLBACK_FAILURE: i32 = nghttp2_sys::NGHTTP2_ERR_CALLBACK_FAILURE;

/// Native transport send callback.
pub type RawSendCallback = nghttp2_sys::nghttp2_send_callback2;

/// Native frame-received callback.
pub type RawFrameRecvCallback = nghttp2_sys::nghttp2_on_frame_recv_callback;
