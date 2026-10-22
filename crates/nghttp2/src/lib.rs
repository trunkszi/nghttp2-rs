//! Safe, single-threaded HTTP/2 client and server sessions over `nghttp2`.
//!
//! [`Session`] owns the native connection and its [`SessionHandler`]. Transport
//! code drives the connection through the sans-I/O send/receive loop. Callbacks
//! receive a non-owning [`SessionCtx`] to submit frames and inspect stream state.
//! [`EventCollector`] provides queued [`SessionEvent`] values for applications
//! that prefer polling over a custom handler.
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

pub use callbacks::SessionHandler;
pub use context::SessionCtx;
pub use error::{Error, ErrorKind, Result};
pub use event::{EventCollector, SessionEvent};
pub use hpack::{HpDeflater, HpInflater, HpackHeader};
pub use mem::CustomAllocator;
pub use nghttp2_sys as sys;
pub use nv::{HeaderField, NvFlags, OwnedHeader, nv};
pub use option::SessionOptions;
pub use priority::ExtPri;
pub use rcbuf::RcBuf;
pub use session::{Client, ClientSession, Server, ServerSession, Session};
pub use stream::{SessionStreams, StreamRef};
pub use submit::{DataPoll, DataProvider, DataSource, SessionSubmit, SettingsEntry};
pub use types::{ErrorCode, FrameFlags, FrameType, SettingsId};
