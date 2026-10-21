//! Outbound frame construction.
//!
//! Submission functions validate arguments, copy the frame descriptor where
//! required, and queue the frame for the next send pass. DATA frames either copy
//! from a buffer or call a `DataProvider` repeatedly; the provider can defer with
//! `NGHTTP2_ERR_DEFERRED` and resume from another callback.
//!
//! This module exposes the native provider and data-source types that the safe
//! submission API will wrap.

pub use nghttp2_sys::{nghttp2_data_provider2, nghttp2_data_source};

/// Native streaming DATA provider.
pub type RawDataProvider = nghttp2_data_provider2;

/// Native source handed to a DATA provider callback.
pub type RawDataSource = nghttp2_data_source;

/// Provider callback result that postpones transmission.
pub const DEFERRED: i32 = nghttp2_sys::NGHTTP2_ERR_DEFERRED;
