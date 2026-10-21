//! Native error codes and their recovery classification.
//!
//! `nghttp2` reports failures as negative integers. Codes at or below
//! [`NGHTTP2_ERR_FATAL`] leave the session unusable and it must be destroyed.
//! Other negative codes are ordinary protocol, stream, or argument errors that the
//! caller can report and recover from. Three values describe a temporary state
//! rather than a failure: `NGHTTP2_ERR_WOULDBLOCK`, `NGHTTP2_ERR_DEFERRED`, and
//! `NGHTTP2_ERR_PAUSE`.
//!
//! The typed error wrapper will preserve the native code so diagnostics never
//! lose information.

use nghttp2_sys::{
    NGHTTP2_ERR_DEFERRED, NGHTTP2_ERR_FATAL, NGHTTP2_ERR_PAUSE, NGHTTP2_ERR_WOULDBLOCK,
    nghttp2_error, nghttp2_error_code,
};

/// Native error code returned by an `nghttp2` C function.
pub type RawErrorCode = nghttp2_error;

/// Native HTTP/2 error code carried by `RST_STREAM` and `GOAWAY` frames.
pub type RawStreamError = nghttp2_error_code;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_codes_stop_the_session() {
        assert!(!is_fatal(NGHTTP2_ERR_FATAL));
        assert!(is_fatal(nghttp2_sys::NGHTTP2_ERR_NOMEM));
        assert!(is_fatal(nghttp2_sys::NGHTTP2_ERR_CALLBACK_FAILURE));
        assert!(!is_fatal(nghttp2_sys::NGHTTP2_ERR_INVALID_ARGUMENT));
        assert!(!is_fatal(0));
    }

    #[test]
    fn temporal_codes_are_retryable() {
        assert!(is_temporal(NGHTTP2_ERR_WOULDBLOCK));
        assert!(is_temporal(NGHTTP2_ERR_DEFERRED));
        assert!(is_temporal(NGHTTP2_ERR_PAUSE));
        assert!(!is_temporal(nghttp2_sys::NGHTTP2_ERR_FATAL));
        assert!(!is_temporal(nghttp2_sys::NGHTTP2_ERR_INVALID_ARGUMENT));
    }
}
