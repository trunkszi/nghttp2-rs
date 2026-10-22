//! Safe HTTP/2 protocol enumerations and scalar aliases.
//!
//! The C library models frame types, flags, settings identifiers, stream states,
//! header categories, and error codes as integer typedefs. The newtypes here keep
//! the native values losslessly while giving callers named constants and checked
//! conversions. Unknown values remain representable, which matters for extension
//! frame types, user-defined settings, and error codes from future peers.

use core::{
    ffi::{CStr, c_uchar},
    fmt,
    ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not},
};

/// HTTP/2 stream identifier.
pub type StreamId = i32;

/// 32-bit value carried by a `SETTINGS` entry.
pub type SettingsValue = u32;

/// Native frame header shared by every frame type.
pub type FrameHeader = nghttp2_sys::nghttp2_frame_hd;

/// Masks a non-negative stream identifier for on-wire frame fields.
///
/// Stream identifiers are positive; the `-1` value used by submission functions to
/// request a new stream is a sentinel and has no wire encoding. The most
/// significant bit is reserved and always clear on the wire.
#[must_use]
pub const fn stream_id_bits(stream_id: u32) -> u32 {
    stream_id & 0x7fff_ffff
}

/// HTTP/2 error code carried by `RST_STREAM` and `GOAWAY` frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ErrorCode(u32);

impl ErrorCode {
    /// No error.
    pub const NO_ERROR: Self = Self(nghttp2_sys::NGHTTP2_NO_ERROR);
    /// Peer detected a protocol violation.
    pub const PROTOCOL_ERROR: Self = Self(nghttp2_sys::NGHTTP2_PROTOCOL_ERROR);
    /// The endpoint encountered an unexpected internal error.
    pub const INTERNAL_ERROR: Self = Self(nghttp2_sys::NGHTTP2_INTERNAL_ERROR);
    /// The endpoint detected that its peer violated the flow-control protocol.
    pub const FLOW_CONTROL_ERROR: Self = Self(nghttp2_sys::NGHTTP2_FLOW_CONTROL_ERROR);
    /// The endpoint did not receive a `SETTINGS` frame in time.
    pub const SETTINGS_TIMEOUT: Self = Self(nghttp2_sys::NGHTTP2_SETTINGS_TIMEOUT);
    /// The endpoint received a frame after a stream was closed.
    pub const STREAM_CLOSED: Self = Self(nghttp2_sys::NGHTTP2_STREAM_CLOSED);
    /// The endpoint received a frame with an invalid size.
    pub const FRAME_SIZE_ERROR: Self = Self(nghttp2_sys::NGHTTP2_FRAME_SIZE_ERROR);
    /// The endpoint refused the stream before performing additional processing.
    pub const REFUSED_STREAM: Self = Self(nghttp2_sys::NGHTTP2_REFUSED_STREAM);
    /// The stream is no longer needed and was cancelled.
    pub const CANCEL: Self = Self(nghttp2_sys::NGHTTP2_CANCEL);
    /// The endpoint could not maintain its header compression state.
    pub const COMPRESSION_ERROR: Self = Self(nghttp2_sys::NGHTTP2_COMPRESSION_ERROR);
    /// The connection established in response to `CONNECT` was reset or aborted.
    pub const CONNECT_ERROR: Self = Self(nghttp2_sys::NGHTTP2_CONNECT_ERROR);
    /// The endpoint detected that its peer is exhibiting abusive behaviour.
    pub const ENHANCE_YOUR_CALM: Self = Self(nghttp2_sys::NGHTTP2_ENHANCE_YOUR_CALM);
    /// The underlying transport has properties that do not meet security minimums.
    pub const INADEQUATE_SECURITY: Self = Self(nghttp2_sys::NGHTTP2_INADEQUATE_SECURITY);
    /// The endpoint requires HTTP/1.1 instead of HTTP/2.
    pub const HTTP_1_1_REQUIRED: Self = Self(nghttp2_sys::NGHTTP2_HTTP_1_1_REQUIRED);

    /// Wraps a raw HTTP/2 error code, including values this build does not know.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw HTTP/2 error code.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Returns the name `nghttp2` associates with the code.
    ///
    /// Unknown codes yield the library's `"unknown"` fallback.
    #[must_use]
    pub fn message(self) -> &'static str {
        // SAFETY: nghttp2_http2_strerror returns a pointer to a static,
        // NUL-terminated string for every input, including unknown codes.
        let message = unsafe { CStr::from_ptr(nghttp2_sys::nghttp2_http2_strerror(self.0)) };
        message.to_str().unwrap_or("unknown error code")
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NO_ERROR => f.write_str("NO_ERROR"),
            Self::PROTOCOL_ERROR => f.write_str("PROTOCOL_ERROR"),
            Self::INTERNAL_ERROR => f.write_str("INTERNAL_ERROR"),
            Self::FLOW_CONTROL_ERROR => f.write_str("FLOW_CONTROL_ERROR"),
            Self::SETTINGS_TIMEOUT => f.write_str("SETTINGS_TIMEOUT"),
            Self::STREAM_CLOSED => f.write_str("STREAM_CLOSED"),
            Self::FRAME_SIZE_ERROR => f.write_str("FRAME_SIZE_ERROR"),
            Self::REFUSED_STREAM => f.write_str("REFUSED_STREAM"),
            Self::CANCEL => f.write_str("CANCEL"),
            Self::COMPRESSION_ERROR => f.write_str("COMPRESSION_ERROR"),
            Self::CONNECT_ERROR => f.write_str("CONNECT_ERROR"),
            Self::ENHANCE_YOUR_CALM => f.write_str("ENHANCE_YOUR_CALM"),
            Self::INADEQUATE_SECURITY => f.write_str("INADEQUATE_SECURITY"),
            Self::HTTP_1_1_REQUIRED => f.write_str("HTTP_1_1_REQUIRED"),
            Self(raw) => write!(f, "UNKNOWN(0x{raw:08x})"),
        }
    }
}

impl From<u32> for ErrorCode {
    fn from(raw: u32) -> Self {
        Self(raw)
    }
}

impl From<ErrorCode> for u32 {
    fn from(code: ErrorCode) -> Self {
        code.0
    }
}

/// HTTP/2 `SETTINGS` identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SettingsId(u32);

impl SettingsId {
    /// `SETTINGS_HEADER_TABLE_SIZE`.
    pub const HEADER_TABLE_SIZE: Self = Self(nghttp2_sys::NGHTTP2_SETTINGS_HEADER_TABLE_SIZE);
    /// `SETTINGS_ENABLE_PUSH`.
    pub const ENABLE_PUSH: Self = Self(nghttp2_sys::NGHTTP2_SETTINGS_ENABLE_PUSH);
    /// `SETTINGS_MAX_CONCURRENT_STREAMS`.
    pub const MAX_CONCURRENT_STREAMS: Self =
        Self(nghttp2_sys::NGHTTP2_SETTINGS_MAX_CONCURRENT_STREAMS);
    /// `SETTINGS_INITIAL_WINDOW_SIZE`.
    pub const INITIAL_WINDOW_SIZE: Self = Self(nghttp2_sys::NGHTTP2_SETTINGS_INITIAL_WINDOW_SIZE);
    /// `SETTINGS_MAX_FRAME_SIZE`.
    pub const MAX_FRAME_SIZE: Self = Self(nghttp2_sys::NGHTTP2_SETTINGS_MAX_FRAME_SIZE);
    /// `SETTINGS_MAX_HEADER_LIST_SIZE`.
    pub const MAX_HEADER_LIST_SIZE: Self = Self(nghttp2_sys::NGHTTP2_SETTINGS_MAX_HEADER_LIST_SIZE);
    /// `SETTINGS_ENABLE_CONNECT_PROTOCOL` (RFC 8441).
    pub const ENABLE_CONNECT_PROTOCOL: Self =
        Self(nghttp2_sys::NGHTTP2_SETTINGS_ENABLE_CONNECT_PROTOCOL);
    /// `SETTINGS_NO_RFC7540_PRIORITIES` (RFC 9218).
    pub const NO_RFC7540_PRIORITIES: Self =
        Self(nghttp2_sys::NGHTTP2_SETTINGS_NO_RFC7540_PRIORITIES);

    /// Wraps a raw identifier, including user-defined and unknown values.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw identifier.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SettingsId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::HEADER_TABLE_SIZE => f.write_str("HEADER_TABLE_SIZE"),
            Self::ENABLE_PUSH => f.write_str("ENABLE_PUSH"),
            Self::MAX_CONCURRENT_STREAMS => f.write_str("MAX_CONCURRENT_STREAMS"),
            Self::INITIAL_WINDOW_SIZE => f.write_str("INITIAL_WINDOW_SIZE"),
            Self::MAX_FRAME_SIZE => f.write_str("MAX_FRAME_SIZE"),
            Self::MAX_HEADER_LIST_SIZE => f.write_str("MAX_HEADER_LIST_SIZE"),
            Self::ENABLE_CONNECT_PROTOCOL => f.write_str("ENABLE_CONNECT_PROTOCOL"),
            Self::NO_RFC7540_PRIORITIES => f.write_str("NO_RFC7540_PRIORITIES"),
            Self(raw) => write!(f, "UNKNOWN(0x{raw:04x})"),
        }
    }
}

impl From<u32> for SettingsId {
    fn from(raw: u32) -> Self {
        Self(raw)
    }
}

impl From<SettingsId> for u32 {
    fn from(id: SettingsId) -> Self {
        id.0
    }
}

/// HTTP/2 frame type (`nghttp2_frame_hd.type_`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameType(u8);

impl FrameType {
    /// `DATA`.
    pub const DATA: Self = Self(nghttp2_sys::NGHTTP2_DATA as u8);
    /// `HEADERS`.
    pub const HEADERS: Self = Self(nghttp2_sys::NGHTTP2_HEADERS as u8);
    /// `PRIORITY`.
    pub const PRIORITY: Self = Self(nghttp2_sys::NGHTTP2_PRIORITY as u8);
    /// `RST_STREAM`.
    pub const RST_STREAM: Self = Self(nghttp2_sys::NGHTTP2_RST_STREAM as u8);
    /// `SETTINGS`.
    pub const SETTINGS: Self = Self(nghttp2_sys::NGHTTP2_SETTINGS as u8);
    /// `PUSH_PROMISE`.
    pub const PUSH_PROMISE: Self = Self(nghttp2_sys::NGHTTP2_PUSH_PROMISE as u8);
    /// `PING`.
    pub const PING: Self = Self(nghttp2_sys::NGHTTP2_PING as u8);
    /// `GOAWAY`.
    pub const GOAWAY: Self = Self(nghttp2_sys::NGHTTP2_GOAWAY as u8);
    /// `WINDOW_UPDATE`.
    pub const WINDOW_UPDATE: Self = Self(nghttp2_sys::NGHTTP2_WINDOW_UPDATE as u8);
    /// `CONTINUATION`.
    pub const CONTINUATION: Self = Self(nghttp2_sys::NGHTTP2_CONTINUATION as u8);
    /// `ALTSVC` (RFC 7838).
    pub const ALTSVC: Self = Self(nghttp2_sys::NGHTTP2_ALTSVC as u8);
    /// `ORIGIN` (RFC 8336).
    pub const ORIGIN: Self = Self(nghttp2_sys::NGHTTP2_ORIGIN as u8);
    /// `PRIORITY_UPDATE` (RFC 9218).
    pub const PRIORITY_UPDATE: Self = Self(nghttp2_sys::NGHTTP2_PRIORITY_UPDATE as u8);

    /// Wraps a raw frame type, including extension values.
    #[must_use]
    pub const fn from_raw(raw: u8) -> Self {
        Self(raw)
    }

    /// Returns the raw frame type.
    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }
}

impl fmt::Display for FrameType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::DATA => f.write_str("DATA"),
            Self::HEADERS => f.write_str("HEADERS"),
            Self::PRIORITY => f.write_str("PRIORITY"),
            Self::RST_STREAM => f.write_str("RST_STREAM"),
            Self::SETTINGS => f.write_str("SETTINGS"),
            Self::PUSH_PROMISE => f.write_str("PUSH_PROMISE"),
            Self::PING => f.write_str("PING"),
            Self::GOAWAY => f.write_str("GOAWAY"),
            Self::WINDOW_UPDATE => f.write_str("WINDOW_UPDATE"),
            Self::CONTINUATION => f.write_str("CONTINUATION"),
            Self::ALTSVC => f.write_str("ALTSVC"),
            Self::ORIGIN => f.write_str("ORIGIN"),
            Self::PRIORITY_UPDATE => f.write_str("PRIORITY_UPDATE"),
            Self(raw) => write!(f, "UNKNOWN(0x{raw:02x})"),
        }
    }
}

impl From<u8> for FrameType {
    fn from(raw: u8) -> Self {
        Self(raw)
    }
}

impl From<FrameType> for u8 {
    fn from(frame_type: FrameType) -> Self {
        frame_type.0
    }
}

/// A set of HTTP/2 frame flag bits.
///
/// Flag meanings are frame-type dependent: bit `0x1` is `END_STREAM` on `DATA` and
/// `HEADERS` but `ACK` on `SETTINGS` and `PING`. Both names are provided because
/// the C library exposes both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameFlags(u8);

impl FrameFlags {
    /// No flags set.
    pub const NONE: Self = Self(nghttp2_sys::NGHTTP2_FLAG_NONE as u8);
    /// `END_STREAM` on `DATA` and `HEADERS`.
    pub const END_STREAM: Self = Self(nghttp2_sys::NGHTTP2_FLAG_END_STREAM as u8);
    /// `ACK` on `SETTINGS` and `PING`; shares bit `0x1` with [`Self::END_STREAM`].
    pub const ACK: Self = Self(nghttp2_sys::NGHTTP2_FLAG_ACK as u8);
    /// `END_HEADERS` on `HEADERS`, `PUSH_PROMISE`, and `CONTINUATION`.
    pub const END_HEADERS: Self = Self(nghttp2_sys::NGHTTP2_FLAG_END_HEADERS as u8);
    /// `PADDED` on `DATA` and `HEADERS`.
    pub const PADDED: Self = Self(nghttp2_sys::NGHTTP2_FLAG_PADDED as u8);
    /// `PRIORITY` on `HEADERS`.
    pub const PRIORITY: Self = Self(nghttp2_sys::NGHTTP2_FLAG_PRIORITY as u8);

    /// Wraps raw flag bits without discarding unknown bits.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Returns the raw flag bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns whether every bit in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns whether no bits are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for FrameFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for FrameFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for FrameFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for FrameFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl Not for FrameFlags {
    type Output = Self;

    fn not(self) -> Self {
        Self(!self.0)
    }
}

impl From<u8> for FrameFlags {
    fn from(bits: u8) -> Self {
        Self(bits)
    }
}

impl From<FrameFlags> for u8 {
    fn from(flags: FrameFlags) -> Self {
        flags.0
    }
}

/// HTTP/2 stream protocol state reported by `nghttp2_stream_get_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamProtoState(u32);

impl StreamProtoState {
    /// `idle`.
    pub const IDLE: Self = Self(nghttp2_sys::NGHTTP2_STREAM_STATE_IDLE);
    /// `open`.
    pub const OPEN: Self = Self(nghttp2_sys::NGHTTP2_STREAM_STATE_OPEN);
    /// `reserved (local)`.
    pub const RESERVED_LOCAL: Self = Self(nghttp2_sys::NGHTTP2_STREAM_STATE_RESERVED_LOCAL);
    /// `reserved (remote)`.
    pub const RESERVED_REMOTE: Self = Self(nghttp2_sys::NGHTTP2_STREAM_STATE_RESERVED_REMOTE);
    /// `half-closed (local)`.
    pub const HALF_CLOSED_LOCAL: Self = Self(nghttp2_sys::NGHTTP2_STREAM_STATE_HALF_CLOSED_LOCAL);
    /// `half-closed (remote)`.
    pub const HALF_CLOSED_REMOTE: Self = Self(nghttp2_sys::NGHTTP2_STREAM_STATE_HALF_CLOSED_REMOTE);
    /// `closed`.
    pub const CLOSED: Self = Self(nghttp2_sys::NGHTTP2_STREAM_STATE_CLOSED);

    /// Wraps a raw state value, including values from a newer library.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw state value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Returns whether the stream is in the `closed` state.
    #[must_use]
    pub const fn is_closed(self) -> bool {
        self.0 == Self::CLOSED.0
    }
}

impl fmt::Display for StreamProtoState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::IDLE => f.write_str("idle"),
            Self::OPEN => f.write_str("open"),
            Self::RESERVED_LOCAL => f.write_str("reserved (local)"),
            Self::RESERVED_REMOTE => f.write_str("reserved (remote)"),
            Self::HALF_CLOSED_LOCAL => f.write_str("half-closed (local)"),
            Self::HALF_CLOSED_REMOTE => f.write_str("half-closed (remote)"),
            Self::CLOSED => f.write_str("closed"),
            Self(raw) => write!(f, "UNKNOWN(0x{raw:08x})"),
        }
    }
}

impl From<u32> for StreamProtoState {
    fn from(raw: u32) -> Self {
        Self(raw)
    }
}

impl From<StreamProtoState> for u32 {
    fn from(state: StreamProtoState) -> Self {
        state.0
    }
}

/// Category of a `HEADERS` frame (`nghttp2_headers.cat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HeadersCategory(u32);

impl HeadersCategory {
    /// Request headers.
    pub const REQUEST: Self = Self(nghttp2_sys::NGHTTP2_HCAT_REQUEST);
    /// Response headers.
    pub const RESPONSE: Self = Self(nghttp2_sys::NGHTTP2_HCAT_RESPONSE);
    /// Push-response headers.
    pub const PUSH_RESPONSE: Self = Self(nghttp2_sys::NGHTTP2_HCAT_PUSH_RESPONSE);
    /// Trailers or non-final (1xx) response headers.
    pub const HEADERS: Self = Self(nghttp2_sys::NGHTTP2_HCAT_HEADERS);

    /// Wraps a raw category value.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw category value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Display for HeadersCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::REQUEST => f.write_str("request"),
            Self::RESPONSE => f.write_str("response"),
            Self::PUSH_RESPONSE => f.write_str("push response"),
            Self::HEADERS => f.write_str("HEADERS"),
            Self(raw) => write!(f, "UNKNOWN(0x{raw:08x})"),
        }
    }
}

impl From<u32> for HeadersCategory {
    fn from(raw: u32) -> Self {
        Self(raw)
    }
}

impl From<HeadersCategory> for u32 {
    fn from(category: HeadersCategory) -> Self {
        category.0
    }
}

/// Metadata reported by `nghttp2_version`.
///
/// The string references point into static library data and remain valid for the
/// lifetime of the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VersionInfo {
    /// Structure layout age; currently `1`.
    pub age: i32,
    /// Encoded version number (`major << 16 | minor << 8 | patch`).
    pub version_num: i32,
    /// Human-readable version, for example `"1.70.0"`.
    pub version_str: &'static CStr,
    /// ALPN protocol identifier implemented by the library, for example `"h2"`.
    pub proto_str: &'static CStr,
}

/// Returns the metadata of the linked `nghttp2` library.
///
/// # Panics
///
/// Panics if `nghttp2_version(0)` returns a null pointer. The C library always
/// returns a pointer for a zero minimum version, so this indicates a broken
/// installation.
#[must_use]
pub fn version_info() -> VersionInfo {
    // SAFETY: nghttp2_version returns a pointer to static metadata that remains
    // valid for the process lifetime; least_version 0 always succeeds.
    let info = unsafe { nghttp2_sys::nghttp2_version(0) };
    assert!(!info.is_null(), "nghttp2_version(0) returned null");
    // SAFETY: the pointer is non-null and points at static metadata.
    let info = unsafe { &*info };
    // SAFETY: both fields point at static, NUL-terminated strings.
    let (version_str, proto_str) = unsafe {
        (
            CStr::from_ptr(info.version_str),
            CStr::from_ptr(info.proto_str),
        )
    };
    VersionInfo {
        age: info.age,
        version_num: info.version_num,
        version_str,
        proto_str,
    }
}

/// Protocol selected by [`select_next_protocol`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AlpnProtocol {
    /// HTTP/2 over TLS (`h2`).
    Http2,
    /// HTTP/1.1 (`http/1.1`).
    Http11,
}

impl AlpnProtocol {
    /// Returns the protocol identifier bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &'static [u8] {
        match *self {
            Self::Http2 => b"h2",
            Self::Http11 => b"http/1.1",
        }
    }
}

/// Largest list accepted by [`select_next_protocol`].
///
/// The C parser advances through the list with `unsigned int` arithmetic; at or
/// above this length its `i + keylen` check can wrap and read past the slice.
const MAX_ALPN_LIST_LEN: usize = u32::MAX as usize - 256;

fn checked_alpn_len(len: usize) -> Option<u32> {
    (len <= MAX_ALPN_LIST_LEN).then_some(len as u32)
}

/// Selects the protocol to advertise for a peer's ALPN list.
///
/// `protocols` is the length-prefixed protocol list from the TLS ClientHello. HTTP/2
/// is preferred over HTTP/1.1 regardless of the peer's order; `None` means the lists
/// do not overlap or are too large to pass to the C parser.
#[must_use]
pub fn select_next_protocol(protocols: &[u8]) -> Option<AlpnProtocol> {
    let inlen = checked_alpn_len(protocols.len())?;
    let mut out: *mut c_uchar = core::ptr::null_mut();
    let mut outlen: c_uchar = 0;
    // SAFETY: `checked_alpn_len` rejects lengths where the C loop's unsigned
    // `i + keylen` arithmetic could wrap; below that limit the parser bounds-checks
    // every read. The out-pointers are valid locals, written only on success, and
    // the selected bytes point into `protocols`, which outlives the call.
    let selected = unsafe {
        nghttp2_sys::nghttp2_select_next_protocol(
            &raw mut out,
            &raw mut outlen,
            protocols.as_ptr(),
            inlen,
        )
    };
    match selected {
        1 => Some(AlpnProtocol::Http2),
        0 => Some(AlpnProtocol::Http11),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_id_bits_clear_the_reserved_bit() {
        assert_eq!(stream_id_bits(0), 0);
        assert_eq!(stream_id_bits(1), 1);
        assert_eq!(stream_id_bits(0x7fff_ffff), 0x7fff_ffff);
        assert_eq!(stream_id_bits(u32::MAX), 0x7fff_ffff);
    }

    #[test]
    fn error_codes_match_rfc9113() {
        assert_eq!(ErrorCode::NO_ERROR.raw(), 0);
        assert_eq!(ErrorCode::PROTOCOL_ERROR.raw(), 1);
        assert_eq!(ErrorCode::INTERNAL_ERROR.raw(), 2);
        assert_eq!(ErrorCode::FLOW_CONTROL_ERROR.raw(), 3);
        assert_eq!(ErrorCode::SETTINGS_TIMEOUT.raw(), 4);
        assert_eq!(ErrorCode::STREAM_CLOSED.raw(), 5);
        assert_eq!(ErrorCode::FRAME_SIZE_ERROR.raw(), 6);
        assert_eq!(ErrorCode::REFUSED_STREAM.raw(), 7);
        assert_eq!(ErrorCode::CANCEL.raw(), 8);
        assert_eq!(ErrorCode::COMPRESSION_ERROR.raw(), 9);
        assert_eq!(ErrorCode::CONNECT_ERROR.raw(), 10);
        assert_eq!(ErrorCode::ENHANCE_YOUR_CALM.raw(), 11);
        assert_eq!(ErrorCode::INADEQUATE_SECURITY.raw(), 12);
        assert_eq!(ErrorCode::HTTP_1_1_REQUIRED.raw(), 13);
    }

    #[test]
    fn error_code_messages_and_unknown_values() {
        assert_eq!(ErrorCode::CANCEL.message(), "CANCEL");
        assert_eq!(ErrorCode::CANCEL.to_string(), "CANCEL");
        let unknown = ErrorCode::from_raw(999);
        assert_eq!(unknown.message(), "unknown");
        assert_eq!(unknown.raw(), 999);
        assert_eq!(unknown.to_string(), "UNKNOWN(0x000003e7)");
        assert_eq!(ErrorCode::from(8), ErrorCode::CANCEL);
        assert_eq!(u32::from(ErrorCode::CANCEL), 8);
    }

    #[test]
    fn settings_ids_match_rfc9113_and_rfc8441() {
        assert_eq!(SettingsId::HEADER_TABLE_SIZE.raw(), 1);
        assert_eq!(SettingsId::ENABLE_PUSH.raw(), 2);
        assert_eq!(SettingsId::MAX_CONCURRENT_STREAMS.raw(), 3);
        assert_eq!(SettingsId::INITIAL_WINDOW_SIZE.raw(), 4);
        assert_eq!(SettingsId::MAX_FRAME_SIZE.raw(), 5);
        assert_eq!(SettingsId::MAX_HEADER_LIST_SIZE.raw(), 6);
        assert_eq!(SettingsId::ENABLE_CONNECT_PROTOCOL.raw(), 8);
        assert_eq!(SettingsId::NO_RFC7540_PRIORITIES.raw(), 9);
        assert_eq!(SettingsId::MAX_FRAME_SIZE.to_string(), "MAX_FRAME_SIZE");
        assert_eq!(SettingsId::from_raw(0x1234).to_string(), "UNKNOWN(0x1234)");
    }

    #[test]
    fn frame_types_match_the_wire_values() {
        assert_eq!(FrameType::DATA.raw(), 0);
        assert_eq!(FrameType::HEADERS.raw(), 1);
        assert_eq!(FrameType::PRIORITY.raw(), 2);
        assert_eq!(FrameType::RST_STREAM.raw(), 3);
        assert_eq!(FrameType::SETTINGS.raw(), 4);
        assert_eq!(FrameType::PUSH_PROMISE.raw(), 5);
        assert_eq!(FrameType::PING.raw(), 6);
        assert_eq!(FrameType::GOAWAY.raw(), 7);
        assert_eq!(FrameType::WINDOW_UPDATE.raw(), 8);
        assert_eq!(FrameType::CONTINUATION.raw(), 9);
        assert_eq!(FrameType::ALTSVC.raw(), 10);
        assert_eq!(FrameType::ORIGIN.raw(), 12);
        assert_eq!(FrameType::PRIORITY_UPDATE.raw(), 16);
        assert_eq!(FrameType::GOAWAY.to_string(), "GOAWAY");
        assert_eq!(FrameType::from_raw(0x7f).to_string(), "UNKNOWN(0x7f)");
    }

    #[test]
    fn frame_flags_form_a_bit_set() {
        assert!(FrameFlags::NONE.is_empty());
        assert_eq!(FrameFlags::END_STREAM.bits(), 0x1);
        assert_eq!(FrameFlags::ACK.bits(), 0x1);
        assert_eq!(FrameFlags::END_STREAM, FrameFlags::ACK);
        assert_eq!(FrameFlags::END_HEADERS.bits(), 0x4);
        assert_eq!(FrameFlags::PADDED.bits(), 0x8);
        assert_eq!(FrameFlags::PRIORITY.bits(), 0x20);

        let flags = FrameFlags::END_STREAM | FrameFlags::END_HEADERS;
        assert!(flags.contains(FrameFlags::END_STREAM));
        assert!(flags.contains(FrameFlags::END_HEADERS));
        assert!(!flags.contains(FrameFlags::PADDED));
        assert!(flags.contains(FrameFlags::NONE));
        // Partial overlap is not containment.
        assert!(!FrameFlags::from_bits(0x04).contains(FrameFlags::from_bits(0x05)));
        assert!(FrameFlags::from_bits(0x05).contains(FrameFlags::from_bits(0x05)));
        assert_eq!(flags.bits(), 0x5);
        assert_eq!((!FrameFlags::NONE).bits(), 0xff);
        assert_eq!(
            FrameFlags::from_bits(0xff) & FrameFlags::END_HEADERS,
            FrameFlags::END_HEADERS
        );
        assert_eq!(u8::from(flags), 0x5);
        assert_eq!(FrameFlags::from(0x5), flags);

        let mut assigned = FrameFlags::NONE;
        assigned |= FrameFlags::END_STREAM;
        assigned |= FrameFlags::END_HEADERS;
        assert_eq!(assigned.bits(), 0x5);
        assigned &= FrameFlags::END_HEADERS;
        assert_eq!(assigned.bits(), 0x4);
    }

    #[test]
    fn unknown_raw_values_round_trip_through_the_newtypes() {
        assert_eq!(ErrorCode::from_raw(u32::MAX).raw(), u32::MAX);
        assert_eq!(SettingsId::from_raw(0xdead).raw(), 0xdead);
        assert_eq!(FrameType::from_raw(0xfe).raw(), 0xfe);
        assert_eq!(StreamProtoState::from_raw(0xffff).raw(), 0xffff);
        assert_eq!(HeadersCategory::from_raw(0xffff).raw(), 0xffff);
    }

    #[test]
    fn alpn_skips_zero_length_entries() {
        // The zero-length entry is malformed but the C walk skips it safely.
        assert_eq!(
            select_next_protocol(&[0, 2, b'h', b'2']),
            Some(AlpnProtocol::Http2)
        );
    }

    #[test]
    fn error_code_unknown_display_keeps_the_raw_value() {
        assert_eq!(ErrorCode::from_raw(14).to_string(), "UNKNOWN(0x0000000e)");
        assert_eq!(ErrorCode::from_raw(255).to_string(), "UNKNOWN(0x000000ff)");
        assert_eq!(
            ErrorCode::from_raw(u32::MAX).to_string(),
            "UNKNOWN(0xffffffff)"
        );
    }

    #[test]
    fn stream_states_match_nghttp2() {
        assert_eq!(StreamProtoState::IDLE.raw(), 1);
        assert_eq!(StreamProtoState::OPEN.raw(), 2);
        assert_eq!(StreamProtoState::RESERVED_LOCAL.raw(), 3);
        assert_eq!(StreamProtoState::RESERVED_REMOTE.raw(), 4);
        assert_eq!(StreamProtoState::HALF_CLOSED_LOCAL.raw(), 5);
        assert_eq!(StreamProtoState::HALF_CLOSED_REMOTE.raw(), 6);
        assert_eq!(StreamProtoState::CLOSED.raw(), 7);
        assert!(StreamProtoState::CLOSED.is_closed());
        assert!(!StreamProtoState::OPEN.is_closed());
        // Unknown values must not be treated as closed by a `>=` implementation.
        assert!(!StreamProtoState::from_raw(0).is_closed());
        assert!(!StreamProtoState::from_raw(8).is_closed());
        assert_eq!(StreamProtoState::OPEN.to_string(), "open");
        assert_eq!(
            StreamProtoState::from_raw(0).to_string(),
            "UNKNOWN(0x00000000)"
        );
    }

    #[test]
    fn headers_categories_match_nghttp2() {
        assert_eq!(HeadersCategory::REQUEST.raw(), 0);
        assert_eq!(HeadersCategory::RESPONSE.raw(), 1);
        assert_eq!(HeadersCategory::PUSH_RESPONSE.raw(), 2);
        assert_eq!(HeadersCategory::HEADERS.raw(), 3);
        assert_eq!(HeadersCategory::HEADERS.to_string(), "HEADERS");
        assert_eq!(
            HeadersCategory::from_raw(9).to_string(),
            "UNKNOWN(0x00000009)"
        );
    }

    #[test]
    fn version_info_matches_the_linked_library() {
        let info = version_info();
        assert!(info.age >= 1);
        assert_eq!(
            info.version_str.to_bytes(),
            &nghttp2_sys::NGHTTP2_VERSION[..nghttp2_sys::NGHTTP2_VERSION.len() - 1]
        );
        assert_eq!(info.version_num as u32, nghttp2_sys::NGHTTP2_VERSION_NUM);
        assert_eq!(info.proto_str.to_bytes(), b"h2");
    }

    #[test]
    fn alpn_selects_http2_over_http11() {
        let http11 = b"\x08http/1.1";
        let h2 = b"\x02h2";
        assert_eq!(select_next_protocol(h2), Some(AlpnProtocol::Http2));
        assert_eq!(select_next_protocol(http11), Some(AlpnProtocol::Http11));

        let mut both = http11.to_vec();
        both.extend_from_slice(h2);
        assert_eq!(select_next_protocol(&both), Some(AlpnProtocol::Http2));
        assert_eq!(AlpnProtocol::Http2.as_bytes(), b"h2");
        assert_eq!(AlpnProtocol::Http11.as_bytes(), b"http/1.1");
    }

    #[test]
    fn alpn_reports_no_overlap_and_ignores_malformed_lists() {
        assert_eq!(checked_alpn_len(MAX_ALPN_LIST_LEN), Some(u32::MAX - 256));
        assert_eq!(checked_alpn_len(MAX_ALPN_LIST_LEN + 1), None);
        assert_eq!(select_next_protocol(&[]), None);
        // A length prefix that cannot fit the remaining bytes.
        assert_eq!(select_next_protocol(&[250, b'h']), None);
        // An unknown protocol followed by h2.
        let mut list = vec![5, b'x', b'x', b'x', b'x', b'x'];
        list.extend_from_slice(b"\x02h2");
        assert_eq!(select_next_protocol(&list), Some(AlpnProtocol::Http2));
    }
}
