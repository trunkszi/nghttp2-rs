//! Header name/value descriptors.
//!
//! An [`nghttp2_nv`] is a borrowed view. The name and value pointers must remain
//! valid until `nghttp2` has finished encoding the frame that references them. The
//! `NO_COPY` flags let callers keep ownership of the buffers; with those flags
//! clear the library copies the bytes into a single allocation owned by the frame.
//!
//! The `nv!` macro in `nghttp2-macros` produces static byte strings suitable for
//! the zero-copy path.

pub use nghttp2_sys::{
    NGHTTP2_NV_FLAG_NO_COPY_NAME, NGHTTP2_NV_FLAG_NO_COPY_VALUE, NGHTTP2_NV_FLAG_NO_INDEX,
    NGHTTP2_NV_FLAG_NONE, nghttp2_nv, nghttp2_nv_flag,
};

/// Native header name/value pair.
pub type RawHeaderField = nghttp2_nv;

/// Native header flags.
pub type RawHeaderFlags = nghttp2_nv_flag;

/// Flags that keep the name and value buffers under caller ownership.
pub const NO_COPY_FLAGS: RawHeaderFlags =
    NGHTTP2_NV_FLAG_NO_COPY_NAME | NGHTTP2_NV_FLAG_NO_COPY_VALUE;

/// Builds the raw `:method: POST` pseudo-header without copying either string.
///
/// The name and value point at bytes embedded in the binary, so the descriptor
/// remains valid for the lifetime of the process and needs no cleanup. The fields
/// are `*mut` only because `nghttp2_nv` declares them that way; the buffers must
/// never be written through, and the library never frees them under `NO_COPY`.
#[must_use]
pub fn method_post() -> RawHeaderField {
    let (name, value) = nghttp2_macros::nv!(":method", "POST");
    RawHeaderField {
        name: name.as_ptr().cast_mut(),
        value: value.as_ptr().cast_mut(),
        namelen: name.len(),
        valuelen: value.len(),
        flags: NO_COPY_FLAGS as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_post_is_static_and_zero_copy() {
        let field = method_post();
        assert_eq!(field.flags, NO_COPY_FLAGS as u8);
        assert_eq!(field.namelen, 7);
        assert_eq!(field.valuelen, 4);
        // SAFETY: method_post points at static byte strings of the stated lengths.
        let (name, value) = unsafe {
            (
                core::slice::from_raw_parts(field.name, field.namelen),
                core::slice::from_raw_parts(field.value, field.valuelen),
            )
        };
        assert_eq!(name, b":method");
        assert_eq!(value, b"POST");
    }

    #[test]
    fn nv_macro_embeds_static_bytes() {
        let (name, value) = nghttp2_macros::nv!(":path", "/");
        assert_eq!(name, b":path");
        assert_eq!(value, b"/");
    }
}
