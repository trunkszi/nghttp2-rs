//! Header name/value descriptors.
//!
//! An [`nghttp2_nv`] is a borrowed view. The name and value pointers must remain
//! valid until `nghttp2` has finished encoding the frame that references them. The
//! `NO_COPY` flags let callers keep ownership of the buffers; with those flags
//! clear the library copies the bytes into its own HPACK workspace.
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
/// remains valid for the lifetime of the process and needs no cleanup.
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
