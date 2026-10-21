//! Inbound frame and header notifications.
//!
//! `nghttp2_frame` is a tagged union: the `hd.type_` field selects which member is
//! active. Callbacks receive a pointer to it and must not retain it beyond the
//! call. The safe wrapper copies the fields it needs into owned event values that
//! can be queued and processed after the session call returns.
//!
//! [`nghttp2_frame_hd`] is the common header prefix present in every frame type.

pub use nghttp2_sys::{nghttp2_frame, nghttp2_frame_hd};

/// Native frame union.
pub type RawFrame = nghttp2_frame;

/// Native frame header prefix.
pub type RawFrameHeader = nghttp2_frame_hd;

/// Returns the frame type byte stored in a native header.
#[must_use]
pub const fn frame_type(hd: &RawFrameHeader) -> u8 {
    hd.type_
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_type_reads_the_header_byte() {
        let hd = RawFrameHeader {
            length: 0,
            stream_id: 0,
            type_: 4,
            flags: 0,
            reserved: 0,
        };
        assert_eq!(frame_type(&hd), 4);
    }
}
