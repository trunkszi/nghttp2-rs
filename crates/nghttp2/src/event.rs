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
    callbacks::SessionHandler,
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

/// Queue of [`SessionEvent`] values filled by a session's callbacks.
///
/// `EventCollector` never blocks and never allocates on behalf of the caller beyond
/// owning the event payloads; it copies exactly the bytes each callback reports.
#[derive(Debug, Default)]
pub struct EventCollector {
    events: VecDeque<SessionEvent>,
}

impl EventCollector {
    /// Creates an empty collector.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            events: VecDeque::new(),
        }
    }

    /// Removes and returns the oldest pending event.
    ///
    /// Returns `None` when no events are pending.
    #[must_use]
    pub fn next_event(&mut self) -> Option<SessionEvent> {
        self.events.pop_front()
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

    /// Drops every pending event, keeping the allocated capacity.
    pub fn clear(&mut self) {
        self.events.clear();
    }
}

/// Resolves the stream a header block belongs to.
///
/// `on_begin_headers` and `on_header` run for `HEADERS` and for `PUSH_PROMISE`
/// frames. A push promise announces the request headers of the promised stream, so
/// its events carry the promised stream identifier rather than the associated
/// stream the promise arrived on.
fn header_stream_id(frame: &nghttp2_frame) -> i32 {
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
fn copy_goaway_opaque_data(goaway: &nghttp2_sys::nghttp2_goaway) -> Vec<u8> {
    if goaway.opaque_data.is_null() || goaway.opaque_data_len == 0 {
        return Vec::new();
    }
    // SAFETY: for a received `GOAWAY` frame, `opaque_data` points at
    // `opaque_data_len` payload bytes that remain valid for the duration of the
    // callback; the bytes are copied before the callback returns.
    unsafe { core::slice::from_raw_parts(goaway.opaque_data, goaway.opaque_data_len) }.to_vec()
}

impl SessionHandler for EventCollector {
    fn on_frame_recv(&mut self, _ctx: &mut SessionCtx<'_>, frame: &nghttp2_frame) {
        // SAFETY: every member of `nghttp2_frame` starts with `nghttp2_frame_hd` at
        // offset zero, so the tag and flags are readable whichever member is active.
        let hd = unsafe { frame.hd };
        let frame_type = FrameType::from_raw(hd.type_);
        let event = match frame_type {
            FrameType::GOAWAY => {
                // SAFETY: `hd.type_` is `NGHTTP2_GOAWAY`, which selects the `goaway`
                // member of the union.
                let goaway = unsafe { frame.goaway };
                SessionEvent::GoAway {
                    last_stream_id: goaway.last_stream_id,
                    error_code: ErrorCode::from_raw(goaway.error_code),
                    opaque_data: copy_goaway_opaque_data(&goaway),
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
        self.events.push_back(event);
    }

    fn on_frame_send(&mut self, _ctx: &mut SessionCtx<'_>, frame: &nghttp2_frame) {
        // SAFETY: every member of `nghttp2_frame` starts with `nghttp2_frame_hd` at
        // offset zero, so the tag and flags are readable whichever member is active.
        let hd = unsafe { frame.hd };
        self.events.push_back(SessionEvent::FrameSend {
            frame_type: FrameType::from_raw(hd.type_),
            flags: hd.flags,
            stream_id: hd.stream_id,
        });
    }

    fn on_begin_headers(&mut self, _ctx: &mut SessionCtx<'_>, frame: &nghttp2_frame) {
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
        self.events.push_back(SessionEvent::BeginHeaders {
            stream_id,
            category,
        });
    }

    fn on_header(
        &mut self,
        _ctx: &mut SessionCtx<'_>,
        frame: &nghttp2_frame,
        name: &[u8],
        value: &[u8],
        flags: u8,
    ) {
        self.events.push_back(SessionEvent::Header {
            stream_id: header_stream_id(frame),
            name: name.to_vec(),
            value: value.to_vec(),
            flags,
        });
    }

    fn on_data_chunk_recv(
        &mut self,
        _ctx: &mut SessionCtx<'_>,
        flags: u8,
        stream_id: i32,
        data: &[u8],
    ) {
        self.events.push_back(SessionEvent::DataChunk {
            stream_id,
            flags,
            data: data.to_vec(),
        });
    }

    fn on_stream_close(
        &mut self,
        _ctx: &mut SessionCtx<'_>,
        stream_id: i32,
        error_code: ErrorCode,
    ) {
        self.events.push_back(SessionEvent::StreamClose {
            stream_id,
            error_code,
        });
    }

    fn error_callback2(
        &mut self,
        _ctx: &mut SessionCtx<'_>,
        lib_error: NgHttp2Error,
        message: &[u8],
    ) {
        self.events.push_back(SessionEvent::Error {
            code: lib_error,
            message: message.to_vec(),
        });
    }
}

#[cfg(test)]
mod tests {
    use nghttp2_sys::{
        NGHTTP2_CANCEL, NGHTTP2_DATA, NGHTTP2_ERR_INVALID_FRAME, NGHTTP2_GOAWAY,
        NGHTTP2_HCAT_RESPONSE, NGHTTP2_HEADERS, NGHTTP2_NO_ERROR, NGHTTP2_PING,
        NGHTTP2_PUSH_PROMISE, NGHTTP2_SETTINGS, NGHTTP2_WINDOW_UPDATE,
    };

    use super::*;

    /// Builds a header whose `length` and `reserved` fields are irrelevant here.
    fn frame_hd(type_: u8, flags: u8, stream_id: i32) -> nghttp2_frame_hd {
        nghttp2_frame_hd {
            length: 0,
            stream_id,
            type_,
            flags,
            reserved: 0,
        }
    }

    fn headers_frame(stream_id: i32, category: u32, flags: u8) -> nghttp2_frame {
        nghttp2_frame {
            headers: nghttp2_sys::nghttp2_headers {
                hd: frame_hd(NGHTTP2_HEADERS as u8, flags, stream_id),
                padlen: 0,
                pri_spec: nghttp2_sys::nghttp2_priority_spec {
                    stream_id: 0,
                    weight: 16,
                    exclusive: 0,
                },
                nva: core::ptr::null_mut(),
                nvlen: 0,
                cat: category,
            },
        }
    }

    fn push_promise_frame(stream_id: i32, promised_stream_id: i32) -> nghttp2_frame {
        nghttp2_frame {
            push_promise: nghttp2_sys::nghttp2_push_promise {
                hd: frame_hd(NGHTTP2_PUSH_PROMISE as u8, 0, stream_id),
                padlen: 0,
                nva: core::ptr::null_mut(),
                nvlen: 0,
                promised_stream_id,
                reserved: 0,
            },
        }
    }

    fn data_frame(stream_id: i32, flags: u8) -> nghttp2_frame {
        nghttp2_frame {
            data: nghttp2_sys::nghttp2_data {
                hd: frame_hd(NGHTTP2_DATA as u8, flags, stream_id),
                padlen: 0,
            },
        }
    }

    fn goaway_frame(
        last_stream_id: i32,
        error_code: u32,
        opaque_data: *mut u8,
        opaque_data_len: usize,
    ) -> nghttp2_frame {
        nghttp2_frame {
            goaway: nghttp2_sys::nghttp2_goaway {
                hd: frame_hd(NGHTTP2_GOAWAY as u8, 0, 0),
                last_stream_id,
                error_code,
                opaque_data,
                opaque_data_len,
                reserved: 0,
            },
        }
    }

    fn ping_frame(ack: bool, opaque_data: [u8; 8]) -> nghttp2_frame {
        let flags = if ack { FrameFlags::ACK.bits() } else { 0 };
        nghttp2_frame {
            ping: nghttp2_sys::nghttp2_ping {
                hd: frame_hd(NGHTTP2_PING as u8, flags, 0),
                opaque_data,
            },
        }
    }

    fn settings_frame(flags: u8) -> nghttp2_frame {
        nghttp2_frame {
            settings: nghttp2_sys::nghttp2_settings {
                hd: frame_hd(NGHTTP2_SETTINGS as u8, flags, 0),
                niv: 0,
                iv: core::ptr::null_mut(),
            },
        }
    }

    fn window_update_frame(stream_id: i32, increment: i32) -> nghttp2_frame {
        nghttp2_frame {
            window_update: nghttp2_sys::nghttp2_window_update {
                hd: frame_hd(NGHTTP2_WINDOW_UPDATE as u8, 0, stream_id),
                window_size_increment: increment,
                reserved: 0,
            },
        }
    }

    /// Builds a context over a dangling pointer that is never dereferenced.
    fn test_ctx() -> SessionCtx<'static> {
        let raw = core::ptr::NonNull::<nghttp2_sys::nghttp2_session>::dangling().as_ptr();
        // SAFETY: the pointer is dangling and never dereferenced: `SessionCtx` only
        // stores it, and every collector method ignores the handle.
        unsafe { SessionCtx::from_raw(raw) }.expect("dangling session pointer is non-null")
    }

    #[test]
    fn frame_type_reads_the_header_byte() {
        let hd = RawFrameHeader {
            length: 0,
            stream_id: 0,
            type_: 4,
            flags: 0,
            reserved: 0,
        };
        assert_eq!(frame_type(&hd), 4);
    }

    #[test]
    fn collector_records_callbacks_in_order() {
        let mut collector = EventCollector::new();
        let mut ctx = test_ctx();

        let headers = headers_frame(1, NGHTTP2_HCAT_RESPONSE, 0);
        collector.on_begin_headers(&mut ctx, &headers);
        collector.on_header(&mut ctx, &headers, b":status", b"200", 0);
        collector.on_data_chunk_recv(&mut ctx, FrameFlags::END_STREAM.bits(), 1, b"hello");

        let mut opaque = *b"seen";
        let goaway = goaway_frame(7, NGHTTP2_CANCEL, opaque.as_mut_ptr(), opaque.len());
        collector.on_frame_recv(&mut ctx, &goaway);

        let ping = ping_frame(false, *b"pingpong");
        collector.on_frame_recv(&mut ctx, &ping);

        let settings = settings_frame(FrameFlags::ACK.bits());
        collector.on_frame_recv(&mut ctx, &settings);

        let window_update = window_update_frame(3, 4096);
        collector.on_frame_recv(&mut ctx, &window_update);

        let data = data_frame(5, 0);
        collector.on_frame_recv(&mut ctx, &data);

        collector.on_frame_send(&mut ctx, &window_update);
        collector.on_stream_close(&mut ctx, 9, ErrorCode::CANCEL);
        collector.error_callback2(
            &mut ctx,
            NgHttp2Error::from_raw(NGHTTP2_ERR_INVALID_FRAME),
            b"bad frame",
        );

        let expected = [
            SessionEvent::BeginHeaders {
                stream_id: 1,
                category: HeadersCategory::RESPONSE,
            },
            SessionEvent::Header {
                stream_id: 1,
                name: b":status".to_vec(),
                value: b"200".to_vec(),
                flags: 0,
            },
            SessionEvent::DataChunk {
                stream_id: 1,
                flags: FrameFlags::END_STREAM.bits(),
                data: b"hello".to_vec(),
            },
            SessionEvent::GoAway {
                last_stream_id: 7,
                error_code: ErrorCode::CANCEL,
                opaque_data: b"seen".to_vec(),
            },
            SessionEvent::Ping {
                ack: false,
                opaque_data: *b"pingpong",
            },
            SessionEvent::SettingsAck,
            SessionEvent::WindowUpdate {
                stream_id: 3,
                increment: 4096,
            },
            SessionEvent::FrameRecv {
                frame_type: FrameType::DATA,
                flags: 0,
                stream_id: 5,
            },
            SessionEvent::FrameSend {
                frame_type: FrameType::WINDOW_UPDATE,
                flags: 0,
                stream_id: 3,
            },
            SessionEvent::StreamClose {
                stream_id: 9,
                error_code: ErrorCode::CANCEL,
            },
            SessionEvent::Error {
                code: NgHttp2Error::from_raw(NGHTTP2_ERR_INVALID_FRAME),
                message: b"bad frame".to_vec(),
            },
        ];

        assert_eq!(collector.len(), expected.len());
        for (index, event) in expected.iter().enumerate() {
            assert_eq!(
                collector.next_event().as_ref(),
                Some(event),
                "event {index} out of order"
            );
        }
        assert_eq!(collector.next_event(), None);
        assert!(collector.is_empty());
    }

    #[test]
    fn goaway_without_opaque_data_and_settings_without_ack() {
        let mut collector = EventCollector::new();
        let mut ctx = test_ctx();

        let goaway = goaway_frame(11, NGHTTP2_NO_ERROR, core::ptr::null_mut(), 0);
        collector.on_frame_recv(&mut ctx, &goaway);
        collector.on_frame_recv(&mut ctx, &settings_frame(0));

        assert_eq!(
            collector.next_event(),
            Some(SessionEvent::GoAway {
                last_stream_id: 11,
                error_code: ErrorCode::NO_ERROR,
                opaque_data: Vec::new(),
            })
        );
        // A SETTINGS frame without ACK is not an acknowledgement, so it stays a
        // plain frame notification.
        assert_eq!(
            collector.next_event(),
            Some(SessionEvent::FrameRecv {
                frame_type: FrameType::SETTINGS,
                flags: 0,
                stream_id: 0,
            })
        );
        assert_eq!(collector.next_event(), None);
    }

    #[test]
    fn ping_ack_flag_is_reported() {
        let mut collector = EventCollector::new();
        let mut ctx = test_ctx();

        collector.on_frame_recv(&mut ctx, &ping_frame(true, *b"abcdefgh"));

        assert_eq!(
            collector.next_event(),
            Some(SessionEvent::Ping {
                ack: true,
                opaque_data: *b"abcdefgh",
            })
        );
    }

    #[test]
    fn push_promise_begins_headers_for_the_promised_stream() {
        let mut collector = EventCollector::new();
        let mut ctx = test_ctx();

        let frame = push_promise_frame(1, 2);
        collector.on_begin_headers(&mut ctx, &frame);
        collector.on_header(&mut ctx, &frame, b":method", b"GET", 0);

        assert_eq!(
            collector.next_event(),
            Some(SessionEvent::BeginHeaders {
                stream_id: 2,
                category: HeadersCategory::REQUEST,
            })
        );
        assert_eq!(
            collector.next_event(),
            Some(SessionEvent::Header {
                stream_id: 2,
                name: b":method".to_vec(),
                value: b"GET".to_vec(),
                flags: 0,
            })
        );
        assert_eq!(collector.next_event(), None);
    }

    #[test]
    fn pending_events_can_be_counted_and_cleared() {
        let mut collector = EventCollector::default();
        assert!(collector.is_empty());
        assert_eq!(collector.len(), 0);
        assert_eq!(collector.next_event(), None);

        collector.events.push_back(SessionEvent::SettingsAck);
        collector.events.push_back(SessionEvent::SettingsAck);
        assert!(!collector.is_empty());
        assert_eq!(collector.len(), 2);

        assert_eq!(collector.next_event(), Some(SessionEvent::SettingsAck));
        assert_eq!(collector.len(), 1);

        collector.clear();
        assert!(collector.is_empty());
        assert_eq!(collector.len(), 0);
        assert_eq!(collector.next_event(), None);
    }
}
