//! Low-level FFI declarations for the statically linked upstream `nghttp2`.
//!
//! The declarations cover the public surface of `nghttp2.h` and `nghttp2ver.h`.
//! They are assembled at build time from checked-in fragments, so this crate never
//! needs `libclang`; the C library itself is compiled from source by `build.rs`.
//!
//! The `pre-release` feature (enabled by default) tracks the upstream `master`
//! branch. The `stable` feature resolves the newest `v*` release tag instead. Set
//! `NGHTTP2_DIR` to build against an existing source checkout.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::manual_c_str_literals)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

/// Whether the resolved native library provides the outbound queue limit setter.
pub const HAS_MAX_OUTBOUND_QUEUE_SIZE: bool = cfg!(nghttp2_has_max_outbound_queue_size);

/// Returns the native setter when the resolved headers and library support it.
#[must_use]
pub const fn max_outbound_queue_size_setter()
-> Option<unsafe extern "C" fn(*mut nghttp2_option, usize)> {
    #[cfg(nghttp2_has_max_outbound_queue_size)]
    {
        Some(nghttp2_option_set_max_outbound_queue_size)
    }
    #[cfg(not(nghttp2_has_max_outbound_queue_size))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use core::{
        ffi::{CStr, c_int},
        mem::{offset_of, size_of},
    };

    use super::*;

    #[test]
    fn outbound_queue_capability_matches_the_native_setter() {
        let setter = max_outbound_queue_size_setter();
        assert_eq!(HAS_MAX_OUTBOUND_QUEUE_SIZE, setter.is_some());
        if let Some(setter) = setter {
            let mut option = core::ptr::null_mut();
            // SAFETY: option is a writable output pointer.
            assert_eq!(unsafe { nghttp2_option_new(&mut option) }, 0);
            assert!(!option.is_null());
            // SAFETY: option is live and exclusively owned until its deletion.
            unsafe {
                setter(option, 64);
                nghttp2_option_del(option);
            }
        }
    }

    #[test]
    fn public_struct_layout_matches_the_c_abi() {
        assert_eq!(size_of::<nghttp2_info>(), 24);
        assert_eq!(offset_of!(nghttp2_info, age), 0);
        assert_eq!(offset_of!(nghttp2_info, version_num), 4);
        assert_eq!(offset_of!(nghttp2_info, version_str), 8);
        assert_eq!(offset_of!(nghttp2_info, proto_str), 16);

        assert_eq!(size_of::<nghttp2_nv>(), 40);
        assert_eq!(offset_of!(nghttp2_nv, name), 0);
        assert_eq!(offset_of!(nghttp2_nv, value), 8);
        assert_eq!(offset_of!(nghttp2_nv, namelen), 16);
        assert_eq!(offset_of!(nghttp2_nv, valuelen), 24);
        assert_eq!(offset_of!(nghttp2_nv, flags), 32);

        assert_eq!(size_of::<nghttp2_frame_hd>(), 16);
        assert_eq!(offset_of!(nghttp2_frame_hd, length), 0);
        assert_eq!(offset_of!(nghttp2_frame_hd, stream_id), 8);
        assert_eq!(offset_of!(nghttp2_frame_hd, type_), 12);
        assert_eq!(offset_of!(nghttp2_frame_hd, flags), 13);
        assert_eq!(offset_of!(nghttp2_frame_hd, reserved), 14);

        assert_eq!(size_of::<nghttp2_data>(), 24);
        assert_eq!(offset_of!(nghttp2_data, hd), 0);
        assert_eq!(offset_of!(nghttp2_data, padlen), 16);

        assert_eq!(size_of::<nghttp2_headers>(), 64);
        assert_eq!(offset_of!(nghttp2_headers, hd), 0);
        assert_eq!(offset_of!(nghttp2_headers, padlen), 16);
        assert_eq!(offset_of!(nghttp2_headers, pri_spec), 24);
        assert_eq!(offset_of!(nghttp2_headers, nva), 40);
        assert_eq!(offset_of!(nghttp2_headers, nvlen), 48);
        assert_eq!(offset_of!(nghttp2_headers, cat), 56);

        assert_eq!(size_of::<nghttp2_priority_spec>(), 12);
        assert_eq!(offset_of!(nghttp2_priority_spec, stream_id), 0);
        assert_eq!(offset_of!(nghttp2_priority_spec, weight), 4);
        assert_eq!(offset_of!(nghttp2_priority_spec, exclusive), 8);

        assert_eq!(size_of::<nghttp2_rst_stream>(), 24);
        assert_eq!(offset_of!(nghttp2_rst_stream, hd), 0);
        assert_eq!(offset_of!(nghttp2_rst_stream, error_code), 16);

        assert_eq!(size_of::<nghttp2_settings_entry>(), 8);
        assert_eq!(offset_of!(nghttp2_settings_entry, settings_id), 0);
        assert_eq!(offset_of!(nghttp2_settings_entry, value), 4);

        assert_eq!(size_of::<nghttp2_settings>(), 32);
        assert_eq!(offset_of!(nghttp2_settings, hd), 0);
        assert_eq!(offset_of!(nghttp2_settings, niv), 16);
        assert_eq!(offset_of!(nghttp2_settings, iv), 24);

        assert_eq!(size_of::<nghttp2_push_promise>(), 48);
        assert_eq!(offset_of!(nghttp2_push_promise, hd), 0);
        assert_eq!(offset_of!(nghttp2_push_promise, padlen), 16);
        assert_eq!(offset_of!(nghttp2_push_promise, nva), 24);
        assert_eq!(offset_of!(nghttp2_push_promise, nvlen), 32);
        assert_eq!(offset_of!(nghttp2_push_promise, promised_stream_id), 40);
        assert_eq!(offset_of!(nghttp2_push_promise, reserved), 44);

        assert_eq!(size_of::<nghttp2_ping>(), 24);
        assert_eq!(offset_of!(nghttp2_ping, hd), 0);
        assert_eq!(offset_of!(nghttp2_ping, opaque_data), 16);

        assert_eq!(size_of::<nghttp2_goaway>(), 48);
        assert_eq!(offset_of!(nghttp2_goaway, hd), 0);
        assert_eq!(offset_of!(nghttp2_goaway, last_stream_id), 16);
        assert_eq!(offset_of!(nghttp2_goaway, error_code), 20);
        assert_eq!(offset_of!(nghttp2_goaway, opaque_data), 24);
        assert_eq!(offset_of!(nghttp2_goaway, opaque_data_len), 32);
        assert_eq!(offset_of!(nghttp2_goaway, reserved), 40);

        assert_eq!(size_of::<nghttp2_window_update>(), 24);
        assert_eq!(offset_of!(nghttp2_window_update, hd), 0);
        assert_eq!(offset_of!(nghttp2_window_update, window_size_increment), 16);
        assert_eq!(offset_of!(nghttp2_window_update, reserved), 20);

        assert_eq!(size_of::<nghttp2_extension>(), 24);
        assert_eq!(offset_of!(nghttp2_extension, hd), 0);
        assert_eq!(offset_of!(nghttp2_extension, payload), 16);

        assert_eq!(size_of::<nghttp2_frame>(), 64);
        assert_eq!(offset_of!(nghttp2_frame, hd), 0);
        assert_eq!(offset_of!(nghttp2_frame, headers), 0);
        assert_eq!(offset_of!(nghttp2_frame, goaway), 0);
        assert_eq!(offset_of!(nghttp2_frame, ext), 0);

        assert_eq!(size_of::<nghttp2_data_provider2>(), 16);
        assert_eq!(offset_of!(nghttp2_data_provider2, source), 0);
        assert_eq!(offset_of!(nghttp2_data_provider2, read_callback), 8);

        assert_eq!(size_of::<nghttp2_extpri>(), 8);
        assert_eq!(offset_of!(nghttp2_extpri, urgency), 0);
        assert_eq!(offset_of!(nghttp2_extpri, inc), 4);

        assert_eq!(size_of::<nghttp2_mem>(), 40);
        assert_eq!(offset_of!(nghttp2_mem, mem_user_data), 0);
        assert_eq!(offset_of!(nghttp2_mem, malloc), 8);
        assert_eq!(offset_of!(nghttp2_mem, free), 16);
        assert_eq!(offset_of!(nghttp2_mem, calloc), 24);
        assert_eq!(offset_of!(nghttp2_mem, realloc), 32);

        assert_eq!(size_of::<nghttp2_vec>(), 16);
        assert_eq!(offset_of!(nghttp2_vec, base), 0);
        assert_eq!(offset_of!(nghttp2_vec, len), 8);

        assert_eq!(size_of::<nghttp2_data_provider>(), 16);
        assert_eq!(offset_of!(nghttp2_data_provider, source), 0);
        assert_eq!(offset_of!(nghttp2_data_provider, read_callback), 8);

        assert_eq!(size_of::<nghttp2_data_source>(), 8);
        assert_eq!(offset_of!(nghttp2_data_source, fd), 0);
        assert_eq!(offset_of!(nghttp2_data_source, ptr), 0);

        assert_eq!(size_of::<nghttp2_priority>(), 32);
        assert_eq!(offset_of!(nghttp2_priority, hd), 0);
        assert_eq!(offset_of!(nghttp2_priority, pri_spec), 16);

        assert_eq!(size_of::<nghttp2_ext_altsvc>(), 32);
        assert_eq!(offset_of!(nghttp2_ext_altsvc, origin), 0);
        assert_eq!(offset_of!(nghttp2_ext_altsvc, origin_len), 8);
        assert_eq!(offset_of!(nghttp2_ext_altsvc, field_value), 16);
        assert_eq!(offset_of!(nghttp2_ext_altsvc, field_value_len), 24);

        assert_eq!(size_of::<nghttp2_origin_entry>(), 16);
        assert_eq!(offset_of!(nghttp2_origin_entry, origin), 0);
        assert_eq!(offset_of!(nghttp2_origin_entry, origin_len), 8);

        assert_eq!(size_of::<nghttp2_ext_origin>(), 16);
        assert_eq!(offset_of!(nghttp2_ext_origin, nov), 0);
        assert_eq!(offset_of!(nghttp2_ext_origin, ov), 8);

        assert_eq!(size_of::<nghttp2_ext_priority_update>(), 24);
        assert_eq!(offset_of!(nghttp2_ext_priority_update, stream_id), 0);
        assert_eq!(offset_of!(nghttp2_ext_priority_update, field_value), 8);
        assert_eq!(offset_of!(nghttp2_ext_priority_update, field_value_len), 16);
    }

    #[test]
    fn linked_library_reports_the_built_version() {
        // SAFETY: nghttp2_version returns a pointer to static library metadata that
        // stays valid for the lifetime of the process.
        let info = unsafe { nghttp2_version(0) };
        assert!(!info.is_null());
        // SAFETY: the pointer was checked above and refers to the static metadata.
        let info = unsafe { &*info };
        assert!(info.age >= 1);
        assert_eq!(info.version_num, NGHTTP2_VERSION_NUM as c_int);
        // SAFETY: version_str and proto_str are NUL-terminated static strings.
        let (version, proto) = unsafe {
            (
                CStr::from_ptr(info.version_str),
                CStr::from_ptr(info.proto_str),
            )
        };
        assert_eq!(
            version.to_bytes(),
            &NGHTTP2_VERSION[..NGHTTP2_VERSION.len() - 1]
        );
        assert_eq!(proto.to_bytes(), b"h2");
    }

    #[test]
    fn strerror_describes_native_errors() {
        // SAFETY: nghttp2_strerror returns a static NUL-terminated string for every
        // error code, including unknown values.
        let invalid = unsafe { CStr::from_ptr(nghttp2_strerror(NGHTTP2_ERR_INVALID_ARGUMENT)) };
        assert_eq!(invalid.to_bytes(), b"Invalid argument");
        // SAFETY: the same contract applies to the success code.
        let success = unsafe { CStr::from_ptr(nghttp2_strerror(0)) };
        assert_eq!(success.to_bytes(), b"Success");
    }
}
