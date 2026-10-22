//! RFC 9218 extensible priorities and the native descriptor.

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

/// Validated RFC 9218 urgency and incremental-delivery preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExtPri {
    urgency: u32,
    incremental: bool,
}

impl Default for ExtPri {
    fn default() -> Self {
        Self {
            urgency: URGENCY_DEFAULT,
            incremental: false,
        }
    }
}

impl ExtPri {
    /// Creates a priority with urgency in `0..=7`.
    ///
    /// # Errors
    /// Returns `NGHTTP2_ERR_INVALID_ARGUMENT` for an out-of-range urgency.
    pub const fn new(urgency: u32, incremental: bool) -> Result<Self, i32> {
        if urgency > URGENCY_LOW {
            return Err(nghttp2_sys::NGHTTP2_ERR_INVALID_ARGUMENT);
        }
        Ok(Self {
            urgency,
            incremental,
        })
    }

    /// Returns the urgency; smaller values are more urgent.
    #[must_use]
    pub const fn urgency(self) -> u32 {
        self.urgency
    }

    /// Returns whether the response should be delivered incrementally.
    #[must_use]
    pub const fn incremental(self) -> bool {
        self.incremental
    }

    /// Parses a Priority header starting from the RFC defaults.
    ///
    /// # Errors
    /// Returns the native parser error for malformed structured fields, invalid
    /// urgency values, or incorrectly typed recognized fields.
    pub fn from_header(value: impl AsRef<[u8]>) -> Result<Self, i32> {
        let mut priority = Self::default();
        priority.parse_priority(value)?;
        Ok(priority)
    }

    /// Updates fields present in a Priority header through the native parser.
    ///
    /// Omitted fields retain their previous values. Unknown dictionary keys are
    /// ignored according to the native structured-field parser. Failure leaves
    /// this priority unchanged.
    ///
    /// # Errors
    /// Returns the native parser error when the header is invalid.
    pub fn parse_priority(&mut self, value: impl AsRef<[u8]>) -> Result<(), i32> {
        let value = value.as_ref();
        let mut raw = self.as_raw();
        // SAFETY: raw is initialized and writable; value remains borrowed for
        // the entire call. The parser neither stores nor mutates its bytes.
        let code = unsafe {
            nghttp2_sys::nghttp2_extpri_parse_priority(&mut raw, value.as_ptr(), value.len())
        };
        if code != 0 {
            return Err(code);
        }
        *self = Self::new(raw.urgency, raw.inc != 0)?;
        Ok(())
    }

    /// Packs urgency in bits 0..2 and incremental delivery in bit 7.
    ///
    /// This follows the native private helper's representation; it is not an
    /// HTTP/2 frame or a serialized Priority header.
    #[must_use]
    pub const fn to_uint8(self) -> u8 {
        self.urgency as u8 | if self.incremental { 0x80 } else { 0 }
    }

    /// Decodes a byte produced by [`Self::to_uint8`].
    ///
    /// # Errors
    /// Returns `NGHTTP2_ERR_INVALID_ARGUMENT` if any reserved bit (3..6) is set.
    pub const fn from_uint8(value: u8) -> Result<Self, i32> {
        Self::new((value & 0x7f) as u32, value & 0x80 != 0)
    }

    /// Returns a native descriptor by value without exposing mutable state.
    #[must_use]
    pub const fn as_raw(self) -> RawExtPri {
        RawExtPri {
            urgency: self.urgency,
            inc: self.incremental as i32,
        }
    }
}

impl TryFrom<RawExtPri> for ExtPri {
    type Error = i32;

    fn try_from(raw: RawExtPri) -> Result<Self, Self::Error> {
        Self::new(raw.urgency, raw.inc != 0)
    }
}

impl From<ExtPri> for RawExtPri {
    fn from(priority: ExtPri) -> Self {
        priority.as_raw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_validation_and_native_conversion() {
        for urgency in 0..=7 {
            for incremental in [false, true] {
                let priority = ExtPri::new(urgency, incremental).unwrap();
                let raw = priority.as_raw();
                assert_eq!(raw.urgency, urgency);
                assert_eq!(raw.inc, i32::from(incremental));
                assert_eq!(ExtPri::try_from(raw), Ok(priority));
            }
        }
        assert!(ExtPri::new(8, false).is_err());
        assert!(ExtPri::new(u32::MAX, false).is_err());
        assert!(ExtPri::try_from(RawExtPri { urgency: 8, inc: 0 }).is_err());
        assert!(
            ExtPri::try_from(RawExtPri {
                urgency: 2,
                inc: -1
            })
            .unwrap()
            .incremental()
        );
    }

    #[test]
    fn packing_exhaustively_checks_reserved_bits() {
        for byte in 0..=u8::MAX {
            let decoded = ExtPri::from_uint8(byte);
            if byte & 0x78 != 0 {
                assert_eq!(decoded, Err(nghttp2_sys::NGHTTP2_ERR_INVALID_ARGUMENT));
            } else {
                let priority = decoded.unwrap();
                assert_eq!(priority.urgency(), (byte & 7) as u32);
                assert_eq!(priority.incremental(), byte & 0x80 != 0);
                assert_eq!(priority.to_uint8(), byte);
            }
        }
        assert_eq!(ExtPri::new(7, true).unwrap().to_uint8(), 0x87);
    }

    #[test]
    fn native_parser_preserves_omitted_fields_and_rfc_defaults() {
        assert_eq!(ExtPri::default(), ExtPri::new(3, false).unwrap());
        assert_eq!(ExtPri::from_header("").unwrap(), ExtPri::default());
        assert_eq!(
            ExtPri::from_header("u=3, i").unwrap(),
            ExtPri::new(3, true).unwrap()
        );
        let mut priority = ExtPri::new(7, true).unwrap();
        priority.parse_priority("u=0").unwrap();
        assert_eq!(priority, ExtPri::new(0, true).unwrap());
        priority.parse_priority(b"i=?0, extension=(1 2);x").unwrap();
        assert_eq!(priority, ExtPri::new(0, false).unwrap());
        priority.parse_priority("unknown=5").unwrap();
        assert_eq!(priority, ExtPri::new(0, false).unwrap());
    }

    #[test]
    fn unit_scenario_native_parser_handles_binary_input_and_duplicate_keys() {
        // Structured dictionaries use the last occurrence of a duplicate key.
        assert_eq!(
            ExtPri::from_header("u=7, u=0, i=?0, i").unwrap(),
            ExtPri::new(0, true).unwrap()
        );
        for byte in [0, b'\r', b'\n', 0x80, 0xff] {
            let bytes = [b'u', b'=', b'1', b',', b' ', byte];
            let mut priority = ExtPri::new(6, true).unwrap();
            assert_eq!(
                priority.parse_priority(bytes),
                Err(nghttp2_sys::NGHTTP2_ERR_INVALID_ARGUMENT),
                "accepted bytes {bytes:?}"
            );
            assert_eq!(priority, ExtPri::new(6, true).unwrap());
        }
        assert_eq!(
            ExtPri::from_header("u=0;ignored=7, i=?1;ignored").unwrap(),
            ExtPri::new(0, true).unwrap()
        );
    }

    #[test]
    fn malformed_headers_never_partially_update_priority() {
        for header in [
            "u=8",
            "u=-1",
            "u=1.5",
            "u=\"3\"",
            "i=1",
            "i=?2",
            "u=2, i=1",
            "u=2,",
            "u=1\0",
            "u=2\r\ni",
            "u=999999999999999999999999",
        ] {
            let mut priority = ExtPri::new(7, true).unwrap();
            assert!(
                priority.parse_priority(header).is_err(),
                "accepted {header:?}"
            );
            assert_eq!(
                priority,
                ExtPri::new(7, true).unwrap(),
                "mutated for {header:?}"
            );
        }
    }
}
