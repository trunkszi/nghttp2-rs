//! RFC 9218 extensible priorities.
//!
//! `nghttp2_extpri` carries an urgency level in `0..=7` (0 is most urgent) and an
//! incremental-delivery flag. The public C API parses `priority` header values with
//! `nghttp2_extpri_parse_priority`; the RFC 9218 byte encoding must be produced by
//! the safe layer because the corresponding helper is private to the C
//! implementation.

pub use nghttp2_sys::{
    NGHTTP2_EXTPRI_DEFAULT_URGENCY, NGHTTP2_EXTPRI_URGENCY_HIGH, NGHTTP2_EXTPRI_URGENCY_LOW,
    nghttp2_extpri,
};

/// Native extensible-priority descriptor.
pub type RawExtPri = nghttp2_extpri;

/// Most urgent priority level.
pub const URGENCY_HIGH: u32 = NGHTTP2_EXTPRI_URGENCY_HIGH;

/// Least urgent priority level.
pub const URGENCY_LOW: u32 = NGHTTP2_EXTPRI_URGENCY_LOW;

/// Urgency assigned when a stream does not specify one.
pub const URGENCY_DEFAULT: u32 = NGHTTP2_EXTPRI_DEFAULT_URGENCY;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_range_matches_rfc9218() {
        assert_eq!(URGENCY_HIGH, 0);
        assert_eq!(URGENCY_LOW, 7);
        assert_eq!(URGENCY_DEFAULT, 3);
    }
}
