//! Safe, single-threaded HTTP/2 client and server sessions over `nghttp2`.
//!
//! The crate is organized into modules for a non-owning [`context`] view of an
//! `nghttp2_session` and a user-supplied [`callbacks`] handler. Splitting those
//! roles will let handlers submit frames and inspect stream state while the C
//! library is iterating over them, without aliasing the handler's mutable borrow.
//!
//! Modules mirror the C API surface:
//!
//! - [`session`] owns connection state and drives the sans-I/O send/receive loop.
//! - [`submit`] covers outbound frame construction.
//! - [`event`] and [`callbacks`] deliver inbound frames to application code.
//! - [`hpack`], [`option`], [`priority`], and [`mem`] expose standalone helpers.
//!
//! Raw declarations are available through [`sys`].

pub mod callbacks;
pub mod context;
pub mod error;
pub mod event;
pub mod hpack;
pub mod mem;
pub mod nv;
pub mod option;
pub mod priority;
pub mod rcbuf;
pub mod session;
pub mod stream;
pub mod submit;
pub mod types;

pub use nghttp2_sys as sys;
