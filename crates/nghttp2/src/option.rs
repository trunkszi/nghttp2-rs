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

use std::ptr::NonNull;

use crate::{error::RawErrorCode, sys};

/// Owned options copied by native session constructors.
///
/// Setters require exclusive access. The object may be dropped immediately after
/// session creation because the session does not retain its pointer.
#[derive(Debug)]
pub struct SessionOptions {
    raw: NonNull<nghttp2_option>,
}

macro_rules! option_setters {
    ($( $(#[$doc:meta])* $method:ident($arg:ident: $ty:ty) => $native:ident($value:expr); )*) => {
        $(
            $(#[$doc])*
            pub fn $method(&mut self, $arg: $ty) -> &mut Self {
                // SAFETY: this uniquely borrowed object owns a live native option.
                unsafe { sys::$native(self.raw.as_ptr(), $value) };
                self
            }
        )*
    };
}

impl SessionOptions {
    /// Allocates native session options with upstream defaults.
    ///
    /// # Errors
    /// Returns the native allocation failure code if allocation fails.
    pub fn new() -> Result<Self, RawErrorCode> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: raw is writable and the native function initializes it on success.
        let code = unsafe { sys::nghttp2_option_new(&mut raw) };
        if code != 0 {
            return Err(code);
        }
        NonNull::new(raw)
            .map(|raw| Self { raw })
            .ok_or(sys::NGHTTP2_ERR_NOMEM)
    }

    /// Borrows the native options for a session constructor.
    ///
    /// The pointer is valid until this owner is dropped. It must not be freed or
    /// mutated through the pointer while Rust references to the owner are live.
    #[must_use]
    pub fn as_ptr(&self) -> *const nghttp2_option {
        self.raw.as_ptr()
    }

    option_setters! {
        /// Disables automatic receive-window updates when enabled.
        set_no_auto_window_update(enabled: bool) => nghttp2_option_set_no_auto_window_update(i32::from(enabled));
        /// Sets the assumed peer concurrent-stream limit before SETTINGS arrives.
        set_peer_max_concurrent_streams(value: u32) => nghttp2_option_set_peer_max_concurrent_streams(value);
        /// Disables validation of the client connection preface when enabled.
        set_no_recv_client_magic(enabled: bool) => nghttp2_option_set_no_recv_client_magic(i32::from(enabled));
        /// Disables HTTP messaging validation when enabled.
        set_no_http_messaging(enabled: bool) => nghttp2_option_set_no_http_messaging(i32::from(enabled));
        /// Sets the maximum number of reserved remote streams.
        set_max_reserved_remote_streams(value: u32) => nghttp2_option_set_max_reserved_remote_streams(value);
        /// Registers an extension frame type for application handling.
        set_user_recv_extension_type(frame_type: u8) => nghttp2_option_set_user_recv_extension_type(frame_type);
        /// Enables built-in handling for an extension frame type.
        set_builtin_recv_extension_type(frame_type: u8) => nghttp2_option_set_builtin_recv_extension_type(frame_type);
        /// Disables automatic PING acknowledgements when enabled.
        set_no_auto_ping_ack(enabled: bool) => nghttp2_option_set_no_auto_ping_ack(i32::from(enabled));
        /// Sets the maximum outbound uncompressed header-block length.
        set_max_send_header_block_length(value: usize) => nghttp2_option_set_max_send_header_block_length(value);
        /// Caps the HPACK deflater's dynamic table size.
        set_max_deflate_dynamic_table_size(value: usize) => nghttp2_option_set_max_deflate_dynamic_table_size(value);
        /// Disables closed-stream retention when enabled.
        set_no_closed_streams(enabled: bool) => nghttp2_option_set_no_closed_streams(i32::from(enabled));
        /// Limits queued acknowledgement frames.
        set_max_outbound_ack(value: usize) => nghttp2_option_set_max_outbound_ack(value);
        /// Limits entries accepted in one SETTINGS frame.
        set_max_settings(value: usize) => nghttp2_option_set_max_settings(value);
        /// Enables the server's RFC 7540 priority compatibility fallback.
        set_server_fallback_rfc7540_priorities(enabled: bool) => nghttp2_option_set_server_fallback_rfc7540_priorities(i32::from(enabled));
        /// Disables RFC 9113 header-value edge-whitespace validation.
        set_no_rfc9113_leading_and_trailing_ws_validation(enabled: bool) => nghttp2_option_set_no_rfc9113_leading_and_trailing_ws_validation(i32::from(enabled));
        /// Limits the number of CONTINUATION frames per header block.
        set_max_continuations(value: usize) => nghttp2_option_set_max_continuations(value);
        /// Limits the outbound queue's frame count.
        set_max_outbound_queue_size(value: usize) => nghttp2_option_set_max_outbound_queue_size(value);
    }

    /// Sets the stream-reset token bucket's burst and replenishment rate.
    pub fn set_stream_reset_rate_limit(&mut self, burst: u64, rate: u64) -> &mut Self {
        // SAFETY: this uniquely borrowed object owns a live native option.
        unsafe { sys::nghttp2_option_set_stream_reset_rate_limit(self.raw.as_ptr(), burst, rate) };
        self
    }

    /// Sets the protocol-glitch token bucket's burst and replenishment rate.
    pub fn set_glitch_rate_limit(&mut self, burst: u64, rate: u64) -> &mut Self {
        // SAFETY: this uniquely borrowed object owns a live native option.
        unsafe { sys::nghttp2_option_set_glitch_rate_limit(self.raw.as_ptr(), burst, rate) };
        self
    }
}

impl Drop for SessionOptions {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner and deletion occurs exactly once.
        unsafe { sys::nghttp2_option_del(self.raw.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_lifecycle_all_setters_and_session_copy() {
        let mut options = SessionOptions::new().unwrap();
        options
            .set_no_auto_window_update(true)
            .set_peer_max_concurrent_streams(17)
            .set_no_recv_client_magic(true)
            .set_no_http_messaging(true)
            .set_max_reserved_remote_streams(23)
            .set_user_recv_extension_type(201)
            .set_builtin_recv_extension_type(sys::NGHTTP2_ALTSVC as u8)
            .set_no_auto_ping_ack(true)
            .set_max_send_header_block_length(8192)
            .set_max_deflate_dynamic_table_size(2048)
            .set_no_closed_streams(true)
            .set_max_outbound_ack(31)
            .set_max_settings(12)
            .set_server_fallback_rfc7540_priorities(true)
            .set_no_rfc9113_leading_and_trailing_ws_validation(true)
            .set_max_continuations(4)
            .set_max_outbound_queue_size(100)
            .set_stream_reset_rate_limit(100, 10)
            .set_glitch_rate_limit(100, 10);
        let mut callbacks = std::ptr::null_mut();
        let mut session = std::ptr::null_mut();
        // SAFETY: both out-pointers are writable; callbacks remain alive through
        // construction, and the resulting session is freed once after use.
        unsafe {
            assert_eq!(sys::nghttp2_session_callbacks_new(&mut callbacks), 0);
            assert_eq!(
                sys::nghttp2_session_client_new2(
                    &mut session,
                    callbacks,
                    std::ptr::null_mut(),
                    options.as_ptr()
                ),
                0
            );
            sys::nghttp2_session_callbacks_del(callbacks);
            drop(options);
            assert_eq!(
                sys::nghttp2_session_get_remote_settings(
                    session,
                    sys::NGHTTP2_SETTINGS_MAX_CONCURRENT_STREAMS
                ),
                17
            );
            sys::nghttp2_session_del(session);
        }
    }

    #[test]
    fn integration_lifecycle_options_are_copied_for_each_session() {
        let mut options = SessionOptions::new().unwrap();
        let mut first = std::ptr::null_mut();
        let mut second = std::ptr::null_mut();
        let mut callbacks = std::ptr::null_mut();
        // SAFETY: all constructor out-pointers are valid; callback/options owners
        // survive construction and each successfully created session is deleted.
        unsafe {
            assert_eq!(sys::nghttp2_session_callbacks_new(&mut callbacks), 0);
            options.set_peer_max_concurrent_streams(0);
            assert_eq!(
                sys::nghttp2_session_client_new2(
                    &mut first,
                    callbacks,
                    std::ptr::null_mut(),
                    options.as_ptr()
                ),
                0
            );
            options.set_peer_max_concurrent_streams(u32::MAX);
            assert_eq!(
                sys::nghttp2_session_client_new2(
                    &mut second,
                    callbacks,
                    std::ptr::null_mut(),
                    options.as_ptr()
                ),
                0
            );
            sys::nghttp2_session_callbacks_del(callbacks);
            drop(options);
            assert_eq!(
                sys::nghttp2_session_get_remote_settings(
                    first,
                    sys::NGHTTP2_SETTINGS_MAX_CONCURRENT_STREAMS
                ),
                0
            );
            assert_eq!(
                sys::nghttp2_session_get_remote_settings(
                    second,
                    sys::NGHTTP2_SETTINGS_MAX_CONCURRENT_STREAMS
                ),
                u32::MAX
            );
            sys::nghttp2_session_del(first);
            sys::nghttp2_session_del(second);
        }
    }

    #[test]
    fn unit_scenario_boolean_options_can_be_disabled_again() {
        let mut options = SessionOptions::new().unwrap();
        options
            .set_no_auto_window_update(true)
            .set_no_auto_window_update(false)
            .set_no_recv_client_magic(true)
            .set_no_recv_client_magic(false)
            .set_no_http_messaging(true)
            .set_no_http_messaging(false)
            .set_no_auto_ping_ack(true)
            .set_no_auto_ping_ack(false)
            .set_no_closed_streams(true)
            .set_no_closed_streams(false)
            .set_server_fallback_rfc7540_priorities(true)
            .set_server_fallback_rfc7540_priorities(false)
            .set_no_rfc9113_leading_and_trailing_ws_validation(true)
            .set_no_rfc9113_leading_and_trailing_ws_validation(false);
        assert!(!options.as_ptr().is_null());
    }

    #[test]
    fn integration_scenario_ping_ack_obeys_option_after_owner_drop() {
        for disabled in [false, true] {
            let mut options = SessionOptions::new().unwrap();
            options
                .set_no_recv_client_magic(true)
                .set_no_auto_ping_ack(!disabled)
                .set_no_auto_ping_ack(disabled);
            let mut callbacks = std::ptr::null_mut();
            let mut session = std::ptr::null_mut();
            let settings = [0u8, 0, 0, 4, 0, 0, 0, 0, 0];
            let ping = [0u8, 0, 8, 6, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];
            // SAFETY: constructors receive writable out-pointers and live options.
            // Input arrays remain alive during receive; returned output is copied
            // before the next native call invalidates it. Session is freed once.
            unsafe {
                assert_eq!(sys::nghttp2_session_callbacks_new(&mut callbacks), 0);
                assert_eq!(
                    sys::nghttp2_session_server_new2(
                        &mut session,
                        callbacks,
                        std::ptr::null_mut(),
                        options.as_ptr()
                    ),
                    0
                );
                sys::nghttp2_session_callbacks_del(callbacks);
                drop(options);
                assert_eq!(
                    sys::nghttp2_session_mem_recv2(session, settings.as_ptr(), settings.len()),
                    9
                );
                let mut output = std::ptr::null();
                // Drain the SETTINGS acknowledgement before injecting PING.
                assert_eq!(sys::nghttp2_session_mem_send2(session, &mut output), 9);
                assert_eq!(sys::nghttp2_session_mem_send2(session, &mut output), 0);
                assert_eq!(
                    sys::nghttp2_session_mem_recv2(session, ping.as_ptr(), ping.len()),
                    17
                );
                let count = sys::nghttp2_session_mem_send2(session, &mut output);
                if disabled {
                    assert_eq!(count, 0);
                } else {
                    assert_eq!(count, 17);
                    let ack = std::slice::from_raw_parts(output, count as usize);
                    assert_eq!(ack[3], 6);
                    assert_eq!(ack[4], 1);
                    assert_eq!(&ack[9..], &ping[9..]);
                }
                sys::nghttp2_session_del(session);
            }
        }
    }
}
