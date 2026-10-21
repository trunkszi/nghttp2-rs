//! RFC 9218 extensible priorities.
//!
//! `nghttp2_extpri` carries an urgency level in `0..=7` (0 is most urgent) and an
//! incremental-delivery flag. The C library parses `priority` header values with
//! `nghttp2_extpri_parse_priority` and packs the pair into a byte with
//! `nghttp2_extpri_to_uint8`.

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
