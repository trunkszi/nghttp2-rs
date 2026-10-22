//! Typed session events and the pull-based collector.
//!
//! `nghttp2_frame` is a tagged union: `hd.type_` selects which member is active, and
//! the pointer handed to a callback is valid only for the duration of that call.
//! [`EventCollector`] implements [`SessionHandler`] by copying the fields the
//! application needs into owned [`SessionEvent`] values, so a session can be driven
//! through `mem_recv` / `mem_send` and its notifications drained afterwards with
//! [`EventCollector::next_event`].
//!
//! Events are queued in callback order, which is the order the frames were received
//! or sent. A frame that maps to a dedicated event (for example `GOAWAY`) produces
//! only that event; every other received frame produces [`SessionEvent::FrameRecv`].
//!
//! [`nghttp2_frame_hd`] is the common header prefix present in every frame type.

use std::collections::VecDeque;

pub use nghttp2_sys::{nghttp2_frame, nghttp2_frame_hd};

use crate::{
    callbacks::{CallbackError, CallbackResult, Frame, SessionHandler},
    context::SessionCtx,
    error::NgHttp2Error,
    types::{ErrorCode, FrameFlags, FrameType, HeadersCategory},
};

/// Native frame union.
pub type RawFrame = nghttp2_frame;

/// Native frame header prefix.
pub type RawFrameHeader = nghttp2_frame_hd;

/// Returns the frame type byte stored in a native header.
#[must_use]
pub const fn frame_type(hd: &RawFrameHeader) -> u8 {
    hd.type_
}

/// One notification delivered by a session callback.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum SessionEvent {
    /// The library started receiving a header block.
    BeginHeaders {
        /// Stream the header block belongs to.
        stream_id: i32,
        /// Kind of header block being received.
        category: HeadersCategory,
    },
    /// One header name/value pair from the current block.
    Header {
        /// Stream the header block belongs to.
        stream_id: i32,
        /// Header name bytes, copied from the library's buffer.
        name: Vec<u8>,
        /// Header value bytes, copied from the library's buffer.
        value: Vec<u8>,
        /// `nghttp2` header flags for this pair.
        flags: u8,
    },
    /// A chunk of `DATA` payload.
    DataChunk {
        /// Stream the data belongs to.
        stream_id: i32,
        /// `DATA` frame flags.
        flags: u8,
        /// Payload bytes, copied from the library's buffer.
        data: Vec<u8>,
    },
    /// A frame was received that has no dedicated event.
    FrameRecv {
        /// Frame type.
        frame_type: FrameType,
        /// Frame flags.
        flags: u8,
        /// Stream the frame belongs to; zero for connection-level frames.
        /// For a received PUSH_PROMISE this is the associated stream; header
        /// events for the same frame carry the promised stream instead.
        stream_id: i32,
    },
    /// A frame left the session.
    FrameSend {
        /// Frame type.
        frame_type: FrameType,
        /// Frame flags.
        flags: u8,
        /// Stream the frame belongs to; zero for connection-level frames.
        stream_id: i32,
    },
    /// A queued frame was abandoned before transmission.
    FrameNotSend {
        /// Stream the frame belongs to.
        stream_id: i32,
        /// Native frame type byte.
        frame_type: u8,
        /// Native reason the frame could not be sent.
        lib_error_code: i32,
    },
    /// A stream was closed.
    StreamClose {
        /// Closed stream.
        stream_id: i32,
        /// Reason reported by the library or the peer.
        error_code: ErrorCode,
    },
    /// A `GOAWAY` frame was received.
    GoAway {
        /// Highest stream identifier the peer processed.
        last_stream_id: i32,
        /// Connection error code.
        error_code: ErrorCode,
        /// Debug payload, copied from the frame.
        opaque_data: Vec<u8>,
    },
    /// A `PING` frame was received.
    Ping {
        /// Whether this is an acknowledgement of an earlier `PING`.
        ack: bool,
        /// Payload bytes carried by the frame.
        opaque_data: [u8; 8],
    },
    /// A `SETTINGS` frame with the `ACK` flag was received.
    SettingsAck,
    /// A `WINDOW_UPDATE` frame was received.
    WindowUpdate {
        /// Stream the update applies to; zero for the connection window.
        stream_id: i32,
        /// Window size increment.
        increment: i32,
    },
    /// The library reported a session error.
    Error {
        /// Native error code.
        code: NgHttp2Error,
        /// Diagnostic message bytes, copied from the library.
        message: Vec<u8>,
    },
}

/// Queue of owned callback notifications, bounded by event count and payload bytes.
///
/// Admission failure preserves every queued event and returns a callback failure.
/// Payload budgets include header names/values, DATA, GOAWAY debug bytes and error
/// messages; fixed-size event storage is bounded separately by the event limit.
#[derive(Debug)]
pub struct EventCollector {
    events: VecDeque<SessionEvent>,
    max_events: usize,
    max_owned_bytes: usize,
    owned_bytes: usize,
}

impl Default for EventCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl EventCollector {
    /// Creates a collector limited to 4096 events and 16 MiB of owned payloads.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_limits(4096, 16 * 1024 * 1024)
    }

    /// Creates a collector with independent event-count and payload-byte limits.
    /// A zero event limit rejects all notifications; zero bytes allows events
    /// without variable-length payloads.
    #[must_use]
    pub const fn with_limits(max_events: usize, max_owned_bytes: usize) -> Self {
        Self {
            events: VecDeque::new(),
            max_events,
            max_owned_bytes,
            owned_bytes: 0,
        }
    }

    /// Removes the oldest event and releases its admission budget.
    #[must_use]
    pub fn next_event(&mut self) -> Option<SessionEvent> {
        let event = self.events.pop_front()?;
        self.owned_bytes -= event.owned_bytes();
        Some(event)
    }

    /// Returns whether no events are pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Returns the number of pending events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Returns the total variable-length payload bytes held by queued events.
    #[must_use]
    pub fn owned_bytes(&self) -> usize {
        self.owned_bytes
    }

    /// Drops every pending event and releases its budget, retaining queue capacity.
    pub fn clear(&mut self) {
        self.events.clear();
        self.owned_bytes = 0;
    }

    fn admit(&mut self, bytes: usize) -> CallbackResult {
        let total = self
            .owned_bytes
            .checked_add(bytes)
            .ok_or(CallbackError::Failure)?;
        if self.events.len() >= self.max_events || total > self.max_owned_bytes {
            return Err(CallbackError::Failure);
        }
        self.events
            .try_reserve(1)
            .map_err(|_| CallbackError::Failure)
    }

    fn commit(&mut self, event: SessionEvent) {
        self.owned_bytes += event.owned_bytes();
        self.events.push_back(event);
    }

    fn push(&mut self, event: SessionEvent) -> CallbackResult {
        self.admit(event.owned_bytes())?;
        self.commit(event);
        Ok(())
    }
}

impl SessionEvent {
    fn owned_bytes(&self) -> usize {
        match self {
            Self::Header { name, value, .. } => name.len() + value.len(),
            Self::DataChunk { data, .. } => data.len(),
            Self::GoAway { opaque_data, .. } => opaque_data.len(),
            Self::Error { message, .. } => message.len(),
            _ => 0,
        }
    }
}

fn copy_payload(bytes: &[u8]) -> Result<Vec<u8>, CallbackError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|_| CallbackError::Failure)?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

/// Resolves the stream a header block belongs to.
///
/// `on_begin_headers` and `on_header` run for `HEADERS` and for `PUSH_PROMISE`
/// frames. A push promise announces the request headers of the promised stream, so
/// its events carry the promised stream identifier rather than the associated
/// stream the promise arrived on.
fn header_stream_id(frame: &Frame) -> i32 {
    // SAFETY: every member of `nghttp2_frame` starts with `nghttp2_frame_hd` at
    // offset zero, so the tag is readable whichever member is active.
    let hd = unsafe { frame.hd };
    if hd.type_ == FrameType::PUSH_PROMISE.raw() {
        // SAFETY: `hd.type_` is `NGHTTP2_PUSH_PROMISE`, which selects the
        // `push_promise` member of the union.
        unsafe { frame.push_promise.promised_stream_id }
    } else {
        hd.stream_id
    }
}

/// Copies the debug payload of a `GOAWAY` frame.
fn copy_goaway_opaque_data(goaway: &nghttp2_sys::nghttp2_goaway) -> Result<Vec<u8>, CallbackError> {
    if goaway.opaque_data.is_null() || goaway.opaque_data_len == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: for a received `GOAWAY` frame, `opaque_data` points at
    // `opaque_data_len` payload bytes that remain valid for the duration of the
    // callback; the bytes are copied before the callback returns.
    copy_payload(unsafe { core::slice::from_raw_parts(goaway.opaque_data, goaway.opaque_data_len) })
}

impl SessionHandler for EventCollector {
    fn on_frame_recv(&mut self, _ctx: &mut SessionCtx<'_>, frame: &Frame) -> CallbackResult {
        // SAFETY: every member of `nghttp2_frame` starts with `nghttp2_frame_hd` at
        // offset zero, so the tag and flags are readable whichever member is active.
        let hd = unsafe { frame.hd };
        let frame_type = FrameType::from_raw(hd.type_);
        let payload_bytes = if frame_type == FrameType::GOAWAY {
            // SAFETY: the validated frame tag selects the initialized GOAWAY member.
            unsafe { frame.goaway.opaque_data_len }
        } else {
            0
        };
        self.admit(payload_bytes)?;
        let event = match frame_type {
            FrameType::GOAWAY => {
                // SAFETY: `hd.type_` is `NGHTTP2_GOAWAY`, which selects the `goaway`
                // member of the union.
                let goaway = unsafe { frame.goaway };
                SessionEvent::GoAway {
                    last_stream_id: goaway.last_stream_id,
                    error_code: ErrorCode::from_raw(goaway.error_code),
                    opaque_data: copy_goaway_opaque_data(&goaway)?,
                }
            }
            FrameType::PING => {
                // SAFETY: `hd.type_` is `NGHTTP2_PING`, which selects the `ping`
                // member of the union.
                let ping = unsafe { frame.ping };
                SessionEvent::Ping {
                    ack: hd.flags & FrameFlags::ACK.bits() != 0,
                    opaque_data: ping.opaque_data,
                }
            }
            FrameType::SETTINGS if hd.flags & FrameFlags::ACK.bits() != 0 => {
                SessionEvent::SettingsAck
            }
            FrameType::WINDOW_UPDATE => {
                // SAFETY: `hd.type_` is `NGHTTP2_WINDOW_UPDATE`, which selects the
                // `window_update` member of the union.
                let window_update = unsafe { frame.window_update };
                SessionEvent::WindowUpdate {
                    stream_id: hd.stream_id,
                    increment: window_update.window_size_increment,
                }
            }
            _ => SessionEvent::FrameRecv {
                frame_type,
                flags: hd.flags,
                stream_id: hd.stream_id,
            },
        };
        self.commit(event);
        Ok(())
    }

    fn on_frame_send(&mut self, _ctx: &mut SessionCtx<'_>, frame: &Frame) -> CallbackResult {
        // SAFETY: every member of `nghttp2_frame` starts with `nghttp2_frame_hd` at
        // offset zero, so the tag and flags are readable whichever member is active.
        let hd = unsafe { frame.hd };
        self.push(SessionEvent::FrameSend {
            frame_type: FrameType::from_raw(hd.type_),
            flags: hd.flags,
            stream_id: hd.stream_id,
        })
    }

    fn on_frame_not_send(
        &mut self,
        _ctx: &mut SessionCtx<'_>,
        frame: &Frame,
        error: NgHttp2Error,
    ) -> CallbackResult {
        // SAFETY: every validated frame has an initialized common header.
        let hd = unsafe { frame.hd };
        self.push(SessionEvent::FrameNotSend {
            stream_id: hd.stream_id,
            frame_type: hd.type_,
            lib_error_code: error.raw(),
        })
    }

    fn on_begin_headers(&mut self, _ctx: &mut SessionCtx<'_>, frame: &Frame) -> CallbackResult {
        // SAFETY: every member of `nghttp2_frame` starts with `nghttp2_frame_hd` at
        // offset zero, so the tag is readable whichever member is active.
        let hd = unsafe { frame.hd };
        let (stream_id, category) = if hd.type_ == FrameType::PUSH_PROMISE.raw() {
            // SAFETY: `hd.type_` is `NGHTTP2_PUSH_PROMISE`, which selects the
            // `push_promise` member of the union. The member has no `cat` field: a
            // push promise carries the request headers of the promised stream.
            let promised_stream_id = unsafe { frame.push_promise.promised_stream_id };
            (promised_stream_id, HeadersCategory::REQUEST)
        } else {
            // SAFETY: the callback runs for `HEADERS` and `PUSH_PROMISE` frames; the
            // branch above rules out `PUSH_PROMISE`, so the active member is
            // `headers`, whose `cat` field the library has filled in.
            let category = unsafe { frame.headers.cat };
            (hd.stream_id, HeadersCategory::from_raw(category))
        };
        self.push(SessionEvent::BeginHeaders {
            stream_id,
            category,
        })
    }

    fn on_header(
        &mut self,
        _ctx: &mut SessionCtx<'_>,
        frame: &Frame,
        name: &[u8],
        value: &[u8],
        flags: u8,
    ) -> CallbackResult {
        let bytes = name
            .len()
            .checked_add(value.len())
            .ok_or(CallbackError::Failure)?;
        self.admit(bytes)?;
        self.commit(SessionEvent::Header {
            stream_id: header_stream_id(frame),
            name: copy_payload(name)?,
            value: copy_payload(value)?,
            flags,
        });
        Ok(())
    }

    fn on_data_chunk_recv(
        &mut self,
        _ctx: &mut SessionCtx<'_>,
        flags: u8,
        stream_id: i32,
        data: &[u8],
    ) -> CallbackResult {
        self.admit(data.len())?;
        self.commit(SessionEvent::DataChunk {
            stream_id,
            flags,
            data: copy_payload(data)?,
        });
        Ok(())
    }

    fn on_stream_close(
        &mut self,
        _ctx: &mut SessionCtx<'_>,
        stream_id: i32,
        error_code: ErrorCode,
    ) -> CallbackResult {
        self.push(SessionEvent::StreamClose {
            stream_id,
            error_code,
        })
    }

    fn error_callback2(
        &mut self,
        _ctx: &mut SessionCtx<'_>,
        lib_error: NgHttp2Error,
        message: &[u8],
    ) -> CallbackResult {
        self.admit(message.len())?;
        self.commit(SessionEvent::Error {
            code: lib_error,
            message: copy_payload(message)?,
        });
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/support/event_existing.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/support/event_limits.rs"]
mod limits_tests;
