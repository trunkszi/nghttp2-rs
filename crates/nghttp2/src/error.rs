//! Native error codes, their recovery classification, and the crate error type.
//!
//! `nghttp2` reports failures as negative integers. Codes strictly below
//! [`NGHTTP2_ERR_FATAL`] leave the session unusable and it must be destroyed.
//! Other negative codes are ordinary protocol, stream, or argument errors that the
//! caller can report and recover from. Three values describe a temporary state
//! rather than a failure: `NGHTTP2_ERR_WOULDBLOCK`, `NGHTTP2_ERR_DEFERRED`, and
//! `NGHTTP2_ERR_PAUSE`.
//!
//! [`NgHttp2Error`] wraps a raw native code and keeps it available for diagnostics.
//! [`Error`] additionally covers HTTP/2 stream error codes and Rust panics caught at
//! the FFI boundary. Both types preserve the original code so log output never loses
//! the value that the C library or the peer reported.

use core::{ffi::CStr, fmt};

use nghttp2_sys::{
    NGHTTP2_ERR_DEFERRED, NGHTTP2_ERR_FATAL, NGHTTP2_ERR_PAUSE, NGHTTP2_ERR_WOULDBLOCK,
    nghttp2_error, nghttp2_error_code,
};

use crate::types::ErrorCode;

/// Native error code returned by an `nghttp2` C function.
pub type RawErrorCode = nghttp2_error;

/// Native HTTP/2 error code carried by `RST_STREAM` and `GOAWAY` frames.
pub type RawStreamError = nghttp2_error_code;

/// Result type used by every fallible operation in this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Returns whether a native error code makes the session unusable.
///
/// Mirrors upstream `nghttp2_is_fatal`: only codes strictly below
/// `NGHTTP2_ERR_FATAL` are fatal; the `NGHTTP2_ERR_FATAL` value itself is not.
#[must_use]
pub const fn is_fatal(code: RawErrorCode) -> bool {
    code < NGHTTP2_ERR_FATAL
}

/// Returns whether a native error code asks the caller to retry later.
#[must_use]
pub const fn is_temporal(code: RawErrorCode) -> bool {
    matches!(
        code,
        NGHTTP2_ERR_WOULDBLOCK | NGHTTP2_ERR_DEFERRED | NGHTTP2_ERR_PAUSE
    )
}

/// Message reported when a Rust callback panics across the FFI boundary.
const CALLBACK_PANICKED_MESSAGE: &str =
    "a Rust callback panicked at the FFI boundary; the session must be discarded";

/// Returns the message `nghttp2` associates with a native error code.
///
/// `nghttp2_strerror` answers every input, including codes it does not know, with a
/// statically allocated string. The only way `to_str` can fail is a message that is
/// not valid UTF-8, which the C library does not produce; that case degrades to a
/// fixed placeholder rather than dropping the code from the diagnostic.
fn native_message(code: RawErrorCode) -> &'static str {
    // SAFETY: `nghttp2_strerror` returns a pointer to a NUL-terminated string with
    // static storage duration for every input, including unknown codes, and never
    // returns null.
    let message = unsafe { CStr::from_ptr(nghttp2_sys::nghttp2_strerror(code)) };
    message.to_str().unwrap_or("unknown error code")
}

/// A native `nghttp2` error code together with its recovery classification.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct NgHttp2Error {
    code: RawErrorCode,
}

impl NgHttp2Error {
    /// Wraps a raw code returned by an `nghttp2` C function.
    pub const fn from_raw(code: RawErrorCode) -> Self {
        Self { code }
    }

    /// Returns the raw code exactly as the C library produced it.
    #[must_use]
    pub const fn raw(self) -> RawErrorCode {
        self.code
    }

    /// Returns the human-readable message `nghttp2` associates with the code.
    #[must_use]
    pub fn message(self) -> &'static str {
        native_message(self.code)
    }

    /// Returns whether the code makes the session unusable.
    #[must_use]
    pub const fn is_fatal(self) -> bool {
        is_fatal(self.code)
    }

    /// Returns whether the code asks the caller to retry later.
    #[must_use]
    pub const fn is_temporal(self) -> bool {
        is_temporal(self.code)
    }
}

impl fmt::Display for NgHttp2Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (nghttp2 error {})", self.message(), self.code)
    }
}

impl std::error::Error for NgHttp2Error {}

/// Recovery class of an [`Error`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// The session is unusable and must be destroyed.
    Fatal,
    /// The failure is reportable but leaves the session usable.
    NonFatal,
    /// The caller must retry the same operation later.
    Temporal,
    /// A Rust panic was caught at the FFI boundary.
    CallbackPanicked,
}

/// An error produced by the safe `nghttp2` wrapper.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Error {
    /// A native `nghttp2` error code.
    Native(NgHttp2Error),
    /// An HTTP/2 error code that belongs to a single stream.
    Stream(ErrorCode),
    /// A panic raised by a Rust callback and caught at the FFI boundary.
    ///
    /// The callback's state is unknown after a panic, so the session cannot be
    /// resumed and must be discarded.
    CallbackPanicked,
}

impl Error {
    /// Creates an error from a native `nghttp2` code.
    pub const fn from_native(code: RawErrorCode) -> Self {
        Self::Native(NgHttp2Error::from_raw(code))
    }

    /// Creates a stream error from an HTTP/2 error code.
    pub const fn from_stream(code: ErrorCode) -> Self {
        Self::Stream(code)
    }

    /// Returns the recovery class of this error.
    #[must_use]
    pub fn kind(self) -> ErrorKind {
        match self {
            Self::Native(error) => {
                if error.is_fatal() {
                    ErrorKind::Fatal
                } else if error.is_temporal() {
                    ErrorKind::Temporal
                } else {
                    ErrorKind::NonFatal
                }
            }
            Self::Stream(_) => ErrorKind::NonFatal,
            Self::CallbackPanicked => ErrorKind::CallbackPanicked,
        }
    }

    /// Returns whether this error leaves the session unusable.
    ///
    /// Both native fatal errors and caught callback panics require the session to
    /// be discarded; [`Error::kind`] still distinguishes the two cases.
    #[must_use]
    pub fn is_fatal(self) -> bool {
        matches!(self.kind(), ErrorKind::Fatal | ErrorKind::CallbackPanicked)
    }

    /// Returns whether the caller must retry the same operation later.
    #[must_use]
    pub fn is_temporal(self) -> bool {
        self.kind() == ErrorKind::Temporal
    }

    /// Returns the human-readable message for this error.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Native(error) => error.message(),
            Self::Stream(code) => code.message(),
            Self::CallbackPanicked => CALLBACK_PANICKED_MESSAGE,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(error) => write!(f, "{error}"),
            Self::Stream(code) => write!(f, "{} (HTTP/2 error {})", code.message(), code.raw()),
            Self::CallbackPanicked => f.write_str(CALLBACK_PANICKED_MESSAGE),
        }
    }
}

impl std::error::Error for Error {}

impl From<NgHttp2Error> for Error {
    fn from(error: NgHttp2Error) -> Self {
        Self::Native(error)
    }
}

impl From<ErrorCode> for Error {
    fn from(code: ErrorCode) -> Self {
        Self::Stream(code)
    }
}

impl From<RawErrorCode> for Error {
    fn from(code: RawErrorCode) -> Self {
        Self::from_native(code)
    }
}

#[cfg(test)]
mod tests {
    use nghttp2_sys::{
        NGHTTP2_CANCEL, NGHTTP2_ERR_CALLBACK_FAILURE, NGHTTP2_ERR_DEFERRED_DATA_EXIST,
        NGHTTP2_ERR_FLOODED, NGHTTP2_ERR_INVALID_ARGUMENT, NGHTTP2_ERR_NOMEM,
        NGHTTP2_ERR_TEMPORAL_CALLBACK_FAILURE, NGHTTP2_ERR_TOO_MANY_CONTINUATIONS,
    };

    use super::*;

    #[test]
    fn fatal_codes_stop_the_session() {
        assert!(!is_fatal(NGHTTP2_ERR_FATAL));
        assert!(is_fatal(NGHTTP2_ERR_NOMEM));
        assert!(is_fatal(NGHTTP2_ERR_CALLBACK_FAILURE));
        assert!(!is_fatal(NGHTTP2_ERR_INVALID_ARGUMENT));
        assert!(!is_fatal(0));
    }

    #[test]
    fn temporal_codes_are_retryable() {
        assert!(is_temporal(NGHTTP2_ERR_WOULDBLOCK));
        assert!(is_temporal(NGHTTP2_ERR_DEFERRED));
        assert!(is_temporal(NGHTTP2_ERR_PAUSE));
        assert!(!is_temporal(NGHTTP2_ERR_FATAL));
        assert!(!is_temporal(NGHTTP2_ERR_INVALID_ARGUMENT));
    }

    #[test]
    fn native_messages_describe_known_codes() {
        assert_eq!(
            NgHttp2Error::from_raw(NGHTTP2_ERR_INVALID_ARGUMENT).message(),
            "Invalid argument"
        );
        assert_eq!(NgHttp2Error::from_raw(0).message(), "Success");
        assert_eq!(
            NgHttp2Error::from_raw(NGHTTP2_ERR_NOMEM).message(),
            "Out of memory"
        );
    }

    /// Codes below the fatal boundary are classified as fatal, so the unknown-code
    /// cases use -600: not defined by the library, above -900, and not one of the
    /// three temporal values.
    const UNKNOWN_NATIVE_CODE: RawErrorCode = -600;

    #[test]
    fn native_message_for_unknown_codes_is_the_library_default() {
        let message = NgHttp2Error::from_raw(UNKNOWN_NATIVE_CODE).message();
        assert_eq!(message, "Unknown error code");
    }

    #[test]
    fn native_error_preserves_the_raw_code() {
        let error = NgHttp2Error::from_raw(NGHTTP2_ERR_INVALID_ARGUMENT);
        assert_eq!(error.raw(), -501);
        assert_eq!(
            NgHttp2Error::from_raw(UNKNOWN_NATIVE_CODE).raw(),
            UNKNOWN_NATIVE_CODE
        );
    }

    #[test]
    fn native_error_display_reports_message_and_code() {
        let error = NgHttp2Error::from_raw(NGHTTP2_ERR_INVALID_ARGUMENT);
        assert_eq!(error.to_string(), "Invalid argument (nghttp2 error -501)");
    }

    #[test]
    fn fatal_boundary_is_strict() {
        assert!(!NgHttp2Error::from_raw(NGHTTP2_ERR_FATAL).is_fatal());
        assert!(NgHttp2Error::from_raw(NGHTTP2_ERR_NOMEM).is_fatal());
        assert!(NgHttp2Error::from_raw(NGHTTP2_ERR_CALLBACK_FAILURE).is_fatal());
        assert!(NgHttp2Error::from_raw(NGHTTP2_ERR_FLOODED).is_fatal());
        // Beyond the codes used by today's tests: a range check must still be fatal.
        assert!(NgHttp2Error::from_raw(NGHTTP2_ERR_TOO_MANY_CONTINUATIONS).is_fatal());
    }

    #[test]
    fn temporal_classification_is_exact() {
        assert!(NgHttp2Error::from_raw(NGHTTP2_ERR_WOULDBLOCK).is_temporal());
        assert!(NgHttp2Error::from_raw(NGHTTP2_ERR_DEFERRED).is_temporal());
        assert!(NgHttp2Error::from_raw(NGHTTP2_ERR_PAUSE).is_temporal());
        assert!(!NgHttp2Error::from_raw(NGHTTP2_ERR_INVALID_ARGUMENT).is_temporal());
        assert!(!NgHttp2Error::from_raw(NGHTTP2_ERR_TEMPORAL_CALLBACK_FAILURE).is_temporal());
        // A code whose name contains DEFERRED is not one of the retryable states.
        assert!(!NgHttp2Error::from_raw(NGHTTP2_ERR_DEFERRED_DATA_EXIST).is_temporal());
    }

    #[test]
    fn unknown_native_codes_are_neither_fatal_nor_temporal() {
        let error = Error::from_native(UNKNOWN_NATIVE_CODE);
        assert_eq!(error.kind(), ErrorKind::NonFatal);
        assert!(!error.is_fatal());
        assert!(!error.is_temporal());
    }

    #[test]
    fn native_error_kind_tracks_the_native_classification() {
        assert_eq!(
            Error::from_native(NGHTTP2_ERR_NOMEM).kind(),
            ErrorKind::Fatal
        );
        assert_eq!(
            Error::from_native(NGHTTP2_ERR_WOULDBLOCK).kind(),
            ErrorKind::Temporal
        );
        assert_eq!(
            Error::from_native(NGHTTP2_ERR_INVALID_ARGUMENT).kind(),
            ErrorKind::NonFatal
        );
        // The boundary value itself is not fatal, so it falls back to `NonFatal`.
        assert_eq!(
            Error::from_native(NGHTTP2_ERR_FATAL).kind(),
            ErrorKind::NonFatal
        );
        assert!(Error::from_native(NGHTTP2_ERR_CALLBACK_FAILURE).is_fatal());
        assert!(Error::from_native(NGHTTP2_ERR_DEFERRED).is_temporal());
        assert!(!Error::from_native(NGHTTP2_ERR_INVALID_ARGUMENT).is_fatal());
        assert!(!Error::from_native(NGHTTP2_ERR_INVALID_ARGUMENT).is_temporal());
    }

    #[test]
    fn native_error_display_includes_the_raw_code() {
        let error = Error::from_native(NGHTTP2_ERR_INVALID_ARGUMENT);
        assert_eq!(error.to_string(), "Invalid argument (nghttp2 error -501)");
    }

    #[test]
    fn stream_errors_are_per_stream_failures() {
        let error = Error::from_stream(ErrorCode::from_raw(NGHTTP2_CANCEL));
        assert_eq!(error.kind(), ErrorKind::NonFatal);
        assert!(!error.is_fatal());
        assert!(!error.is_temporal());
        assert_eq!(
            error.message(),
            ErrorCode::from_raw(NGHTTP2_CANCEL).message()
        );

        let Error::Stream(code) = error else {
            panic!("expected a stream error, got {error:?}");
        };
        assert_eq!(code.raw(), NGHTTP2_CANCEL);
        // HTTP/2 CANCEL is wire value 8, so the raw code survives the round trip.
        assert_eq!(code.raw(), 8);
    }

    #[test]
    fn stream_display_includes_the_raw_http2_code() {
        let error = Error::from_stream(ErrorCode::from_raw(NGHTTP2_CANCEL));
        let display = error.to_string();
        assert!(
            display.contains("(HTTP/2 error 8)"),
            "display must carry the raw HTTP/2 code: {display}"
        );
        assert!(display.contains(error.message()));
    }

    #[test]
    fn callback_panics_have_their_own_kind() {
        let error = Error::CallbackPanicked;
        assert_eq!(error.kind(), ErrorKind::CallbackPanicked);
        // The session is unusable after a panic even though the kind is not `Fatal`.
        assert!(error.is_fatal());
        assert!(!error.is_temporal());
        assert!(error.message().contains("panicked"));
        assert!(error.message().contains("discarded"));
        assert_eq!(error.to_string(), error.message());
    }

    #[test]
    fn result_alias_carries_the_crate_error() {
        fn fail() -> Result<()> {
            Err(Error::CallbackPanicked)
        }

        fn succeed() -> Result<u8> {
            Ok(7)
        }

        assert_eq!(fail(), Err(Error::CallbackPanicked));
        assert_eq!(succeed(), Ok(7));
    }

    #[test]
    fn from_conversions_build_the_matching_variant() {
        let native = Error::from(NgHttp2Error::from_raw(NGHTTP2_ERR_INVALID_ARGUMENT));
        let Error::Native(inner) = native else {
            panic!("expected a native error, got {native:?}");
        };
        assert_eq!(inner.raw(), NGHTTP2_ERR_INVALID_ARGUMENT);

        let stream = Error::from(ErrorCode::from_raw(NGHTTP2_CANCEL));
        let Error::Stream(inner) = stream else {
            panic!("expected a stream error, got {stream:?}");
        };
        assert_eq!(inner.raw(), NGHTTP2_CANCEL);

        let raw = Error::from(NGHTTP2_ERR_NOMEM);
        let Error::Native(inner) = raw else {
            panic!("expected a native error, got {raw:?}");
        };
        assert_eq!(inner.raw(), NGHTTP2_ERR_NOMEM);
    }

    #[test]
    fn error_trait_is_implemented_for_the_whole_family() {
        fn assert_std_error<T: std::error::Error + Send + Sync + 'static>(_value: &T) {}

        assert_std_error(&NgHttp2Error::from_raw(NGHTTP2_ERR_INVALID_ARGUMENT));
        assert_std_error(&Error::from_native(NGHTTP2_ERR_INVALID_ARGUMENT));
        assert_std_error(&Error::from_stream(ErrorCode::from_raw(NGHTTP2_CANCEL)));
        assert_std_error(&Error::CallbackPanicked);
    }

    #[test]
    fn unknown_fatal_codes_are_classified_by_threshold() {
        // -1000 is below the fatal boundary even though nghttp2 does not define it.
        let error = Error::from_native(-1000);
        assert_eq!(error.kind(), ErrorKind::Fatal);
        assert!(error.is_fatal());
        assert!(!error.is_temporal());
        assert_eq!(error.message(), "Unknown error code");
    }

    #[test]
    fn pause_is_temporal_at_the_error_level() {
        assert!(Error::from_native(NGHTTP2_ERR_PAUSE).is_temporal());
    }
}
