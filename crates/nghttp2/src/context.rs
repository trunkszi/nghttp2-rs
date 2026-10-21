//! Non-owning session handle passed to callbacks.
//!
//! While `nghttp2` processes a frame it invokes Rust callbacks synchronously. Those
//! callbacks often need to submit another frame or query flow-control state on the
//! same `nghttp2_session`. Reconstructing the owning `Session` there would create a
//! second mutable reference to it.
//!
//! This module provides the non-owning pointer aliases that will back
//! `SessionCtx`. They carry no ownership, so callback code can use them without
//! borrowing the outer session.

pub use nghttp2_sys::nghttp2_session;

/// Opaque native session.
pub type RawSession = nghttp2_session;

/// Non-null pointer to a native session.
pub type SessionPtr = core::ptr::NonNull<nghttp2_session>;

/// Non-null pointer to a native stream owned by a session.
pub type StreamPtr = core::ptr::NonNull<nghttp2_sys::nghttp2_stream>;
