//! Session ownership and the sans-I/O transport loop.
//!
//! The session type will own one `nghttp2_session` and the callbacks installed on
//! it. Clients and servers share the same machinery but differ in how the connection
//! preface is exchanged and which frame types are legal.
//!
//! The raw handle is opaque and must be released with `nghttp2_session_del`; the
//! wrapper will tie that call to `Drop`. This module exposes the native handle and
//! the role markers that the safe wrapper is built on.

pub use nghttp2_sys::nghttp2_session;

/// Opaque native session.
pub type RawSession = nghttp2_session;

/// Client-side session role marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Client;

/// Server-side session role marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Server;
