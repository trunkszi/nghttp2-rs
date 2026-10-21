//! Session configuration options.
//!
//! [`nghttp2_option`] is an opaque builder passed to session creation. Each setter
//! configures one protocol limit or compatibility switch: concurrent streams,
//! automatic window updates and pings, extension frame types, header block size,
//! HPACK table size, closed-stream retention, `SETTINGS` acknowledgement limits,
//! and rate limits for stream resets and protocol glitches.
//!
//! The option object owns no session state; the session copies the values at
//! creation time.

pub use nghttp2_sys::{nghttp2_option, nghttp2_settings_entry};

/// Opaque native option set.
pub type RawOption = nghttp2_option;

/// One native `SETTINGS` entry.
pub type RawSettingsEntry = nghttp2_settings_entry;

/// Exclusive upper bound for a stream's remote window.
pub const MAX_WINDOW_SIZE: i32 = nghttp2_sys::NGHTTP2_MAX_WINDOW_SIZE;
