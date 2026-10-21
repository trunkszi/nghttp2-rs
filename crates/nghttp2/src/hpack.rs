//! Standalone HPACK compression and decompression.
//!
//! The deflater and inflater are independent of a session: each owns a dynamic
//! table that the caller can resize, and header blocks can be produced or consumed
//! in one call or in chunks. Sessions use the same machinery internally for their
//! own header blocks.
//!
//! These opaque types are created and destroyed through `nghttp2_hd_*_new` and
//! `nghttp2_hd_*_del`.

pub use nghttp2_sys::{nghttp2_hd_deflater, nghttp2_hd_inflater};

/// Opaque native HPACK deflater.
pub type RawHdDeflater = nghttp2_hd_deflater;

/// Opaque native HPACK inflater.
pub type RawHdInflater = nghttp2_hd_inflater;

/// Default maximum size of the HPACK dynamic table.
pub const DEFAULT_DYNAMIC_TABLE_SIZE: u32 = nghttp2_sys::NGHTTP2_DEFAULT_HEADER_TABLE_SIZE;
