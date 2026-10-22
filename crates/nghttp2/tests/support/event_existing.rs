use nghttp2_sys::{
    NGHTTP2_CANCEL, NGHTTP2_DATA, NGHTTP2_ERR_INVALID_FRAME, NGHTTP2_GOAWAY, NGHTTP2_HCAT_RESPONSE,
    NGHTTP2_HEADERS, NGHTTP2_NO_ERROR, NGHTTP2_PING, NGHTTP2_PUSH_PROMISE, NGHTTP2_SETTINGS,
    NGHTTP2_WINDOW_UPDATE,
};

use super::*;

fn view(raw: &nghttp2_frame) -> &Frame {
    // SAFETY: fixtures initialize the member selected by their native frame tag,
    // and any pointed-to payload remains live throughout the immediate call.
    unsafe { Frame::from_raw_ref(raw) }
}

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
    collector
        .on_begin_headers(&mut ctx, view(&headers))
        .unwrap();
    collector
        .on_header(&mut ctx, view(&headers), b":status", b"200", 0)
        .unwrap();
    collector
        .on_data_chunk_recv(&mut ctx, FrameFlags::END_STREAM.bits(), 1, b"hello")
        .unwrap();

    let mut opaque = *b"seen";
    let goaway = goaway_frame(7, NGHTTP2_CANCEL, opaque.as_mut_ptr(), opaque.len());
    collector.on_frame_recv(&mut ctx, view(&goaway)).unwrap();

    let ping = ping_frame(false, *b"pingpong");
    collector.on_frame_recv(&mut ctx, view(&ping)).unwrap();

    let settings = settings_frame(FrameFlags::ACK.bits());
    collector.on_frame_recv(&mut ctx, view(&settings)).unwrap();

    let window_update = window_update_frame(3, 4096);
    collector
        .on_frame_recv(&mut ctx, view(&window_update))
        .unwrap();

    let data = data_frame(5, 0);
    collector.on_frame_recv(&mut ctx, view(&data)).unwrap();

    collector
        .on_frame_send(&mut ctx, view(&window_update))
        .unwrap();
    collector
        .on_stream_close(&mut ctx, 9, ErrorCode::CANCEL)
        .unwrap();
    collector
        .error_callback2(
            &mut ctx,
            NgHttp2Error::from_raw(NGHTTP2_ERR_INVALID_FRAME),
            b"bad frame",
        )
        .unwrap();

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
    collector.on_frame_recv(&mut ctx, view(&goaway)).unwrap();
    collector
        .on_frame_recv(&mut ctx, view(&settings_frame(0)))
        .unwrap();

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

    collector
        .on_frame_recv(&mut ctx, view(&ping_frame(true, *b"abcdefgh")))
        .unwrap();

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
    collector.on_begin_headers(&mut ctx, view(&frame)).unwrap();
    collector
        .on_header(&mut ctx, view(&frame), b":method", b"GET", 0)
        .unwrap();

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
