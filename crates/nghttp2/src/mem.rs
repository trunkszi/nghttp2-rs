//! Custom allocators for `nghttp2` sessions and HPACK contexts.
//!
//! [`nghttp2_mem`] bundles `malloc`, `calloc`, `realloc`, and `free` function
//! pointers plus an opaque user-data pointer. Passing one at session or HPACK
//! creation redirects every allocation the library performs, which lets callers
//! route it through an arena, a tracking wrapper, or a bounded pool.
//!
//! The function pointers must preserve the C allocator contract for the lifetime of
//! the objects created with them.

pub use nghttp2_sys::{nghttp2_calloc, nghttp2_free, nghttp2_malloc, nghttp2_mem, nghttp2_realloc};

/// Native allocator table.
pub type RawAllocator = nghttp2_mem;

/// Native `malloc` replacement.
pub type RawMalloc = nghttp2_malloc;

/// Native `free` replacement.
pub type RawFree = nghttp2_free;

/// Native `calloc` replacement.
pub type RawCalloc = nghttp2_calloc;

/// Native `realloc` replacement.
pub type RawRealloc = nghttp2_realloc;
