//! Native HTTP/2 enumerations and scalar aliases.
//!
//! `nghttp2` models frame types, flags, settings identifiers, stream states, and
//! header categories as C integer typedefs. This module re-exports those types
//! unchanged so safe wrappers can convert between native values and Rust enums
//! without guessing widths.

pub use nghttp2_sys::{
    nghttp2_data_flag, nghttp2_error_code, nghttp2_flag, nghttp2_frame_type,
    nghttp2_headers_category, nghttp2_nv_flag, nghttp2_settings_id, nghttp2_stream_proto_state,
};

/// HTTP/2 stream identifier.
pub type StreamId = i32;

/// 32-bit value carried by a `SETTINGS` entry.
pub type SettingsValue = u32;

/// Native frame header shared by every frame type.
pub type FrameHeader = nghttp2_sys::nghttp2_frame_hd;

/// Converts a stream identifier into its on-wire unsigned representation.
///
/// The most significant bit is reserved and always clear on the wire.
#[must_use]
pub const fn stream_id_bits(stream_id: StreamId) -> u32 {
    (stream_id as u32) & 0x7fff_ffff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_id_bits_clear_the_reserved_bit() {
        assert_eq!(stream_id_bits(0), 0);
        assert_eq!(stream_id_bits(1), 1);
        assert_eq!(stream_id_bits(i32::MAX), 0x7fff_ffff);
        assert_eq!(stream_id_bits(-1), 0x7fff_ffff);
    }
}
