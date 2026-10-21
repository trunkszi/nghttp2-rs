use super::*;

fn ctx() -> SessionCtx<'static> {
    // SAFETY: the context is used only by collector callbacks, which never inspect
    // its pointer. No native operation is invoked through this test token.
    unsafe { SessionCtx::from_raw(core::ptr::NonNull::dangling().as_ptr()) }.unwrap()
}

fn settings() -> nghttp2_frame {
    nghttp2_frame {
        settings: nghttp2_sys::nghttp2_settings {
            hd: nghttp2_frame_hd {
                length: 0,
                stream_id: 0,
                type_: 4,
                flags: 0,
                reserved: 0,
            },
            niv: 0,
            iv: core::ptr::null_mut(),
        },
    }
}

fn view(raw: &nghttp2_frame) -> &Frame {
    // SAFETY: fixtures initialize the union member identified by their tag, and
    // all payload storage remains borrowed during the immediate callback.
    unsafe { Frame::from_raw_ref(raw) }
}

#[test]
fn unit_scenario_limits_preserve_prefix_and_release_budgets() {
    let mut collector = EventCollector::with_limits(2, 4);
    let mut ctx = ctx();
    collector
        .on_data_chunk_recv(&mut ctx, 0, 1, b"abcd")
        .unwrap();
    assert_eq!(
        collector.on_data_chunk_recv(&mut ctx, 0, 1, b"x"),
        Err(CallbackError::Failure)
    );
    collector
        .on_frame_recv(&mut ctx, view(&settings()))
        .unwrap();
    assert_eq!(
        collector.on_stream_close(&mut ctx, 1, ErrorCode::NO_ERROR),
        Err(CallbackError::Failure)
    );
    assert_eq!((collector.len(), collector.owned_bytes()), (2, 4));
    assert_eq!(
        collector.next_event(),
        Some(SessionEvent::DataChunk {
            stream_id: 1,
            flags: 0,
            data: b"abcd".to_vec()
        })
    );
    assert_eq!(collector.owned_bytes(), 0);
    collector
        .error_callback2(&mut ctx, NgHttp2Error::from_raw(-1), b"done")
        .unwrap();
    collector.clear();
    assert_eq!((collector.len(), collector.owned_bytes()), (0, 0));
    collector
        .on_data_chunk_recv(&mut ctx, 0, 1, b"abcd")
        .unwrap();
}

#[test]
fn unit_scenario_header_admission_is_atomic_and_owned() {
    let mut collector = EventCollector::with_limits(8, 5);
    let mut ctx = ctx();
    let frame = settings(); // on_header reads only the initialized common header.
    collector
        .on_header(&mut ctx, view(&frame), b"ab", b"cde", 0)
        .unwrap();
    assert_eq!(
        collector.on_header(&mut ctx, view(&frame), b"a", b"b", 0),
        Err(CallbackError::Failure)
    );
    assert_eq!((collector.len(), collector.owned_bytes()), (1, 5));
    assert!(
        matches!(collector.next_event(), Some(SessionEvent::Header { name, value, .. }) if name == b"ab" && value == b"cde")
    );
    let mut bytes = b"fresh".to_vec();
    collector
        .on_data_chunk_recv(&mut ctx, 0, 1, &bytes)
        .unwrap();
    bytes.fill(0);
    assert!(
        matches!(collector.next_event(), Some(SessionEvent::DataChunk { data, .. }) if data == b"fresh")
    );
}

#[test]
fn unit_scenario_zero_limits_and_checked_byte_overflow_refuse_events() {
    let mut ctx = ctx();
    let mut collector = EventCollector::with_limits(0, usize::MAX);
    assert_eq!(
        collector.on_frame_recv(&mut ctx, view(&settings())),
        Err(CallbackError::Failure)
    );
    assert!(collector.is_empty());
    let mut collector = EventCollector::with_limits(1, 0);
    assert_eq!(
        collector.on_data_chunk_recv(&mut ctx, 0, 1, b"x"),
        Err(CallbackError::Failure)
    );
    collector
        .on_frame_recv(&mut ctx, view(&settings()))
        .unwrap();
    collector.clear();
    collector.owned_bytes = usize::MAX;
    assert_eq!(collector.admit(1), Err(CallbackError::Failure));
    assert!(collector.is_empty());
    assert_eq!(collector.owned_bytes(), usize::MAX);
}

#[test]
fn unit_scenario_frame_not_sent_keeps_native_reason_and_obeys_count_limit() {
    let mut collector = EventCollector::with_limits(1, 0);
    let mut ctx = ctx();
    let frame = settings();
    let error = NgHttp2Error::from_raw(nghttp2_sys::NGHTTP2_ERR_STREAM_CLOSED);
    collector
        .on_frame_not_send(&mut ctx, view(&frame), error)
        .unwrap();
    assert_eq!(
        collector.on_frame_not_send(&mut ctx, view(&frame), error),
        Err(CallbackError::Failure)
    );
    assert_eq!(
        collector.next_event(),
        Some(SessionEvent::FrameNotSend {
            stream_id: 0,
            frame_type: 4,
            lib_error_code: error.raw()
        })
    );
}

#[test]
fn integration_scenario_native_receive_returns_callback_failure_on_budget_exhaustion() {
    use crate::callbacks::{CallbackBox, Callbacks};
    let callbacks = Callbacks::<EventCollector>::new().unwrap();
    let mut state = Box::new(CallbackBox::new(EventCollector::with_limits(1, 0)));
    let mut raw = core::ptr::null_mut();
    // SAFETY: the callbacks and boxed user data remain alive until session deletion;
    // the native output pointer is writable and the box has the matching handler.
    let result = unsafe {
        nghttp2_sys::nghttp2_session_server_new(
            &raw mut raw,
            callbacks.as_ptr(),
            core::ptr::from_mut(state.as_mut()).cast(),
        )
    };
    assert_eq!(result, 0);
    struct Session(*mut nghttp2_sys::nghttp2_session);
    impl Drop for Session {
        fn drop(&mut self) {
            // SAFETY: this guard owns the session and drops before its user data.
            unsafe { nghttp2_sys::nghttp2_session_del(self.0) };
        }
    }
    let session = Session(raw);
    // Two valid SETTINGS frames: the first is preserved, the second exceeds the
    // event-count budget while still inside the same native receive call.
    let mut wire = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec();
    wire.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 0, 0]);
    wire.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 0, 0]);
    // SAFETY: the session and the complete wire buffer are live for this call.
    let result =
        unsafe { nghttp2_sys::nghttp2_session_mem_recv2(session.0, wire.as_ptr(), wire.len()) };
    assert_eq!(result, nghttp2_sys::NGHTTP2_ERR_CALLBACK_FAILURE as isize);
    assert_eq!(state.handler_mut().len(), 1);
    assert!(matches!(
        state.handler_mut().next_event(),
        Some(SessionEvent::FrameRecv {
            frame_type: FrameType::SETTINGS,
            ..
        })
    ));
    assert_eq!(state.handler_mut().owned_bytes(), 0);
}

#[test]
fn unit_scenario_goaway_budget_accounts_debug_payload() {
    let mut collector = EventCollector::with_limits(2, 3);
    let mut ctx = ctx();
    let mut payload = *b"four";
    let mut frame = nghttp2_frame {
        goaway: nghttp2_sys::nghttp2_goaway {
            hd: nghttp2_frame_hd {
                length: 12,
                stream_id: 0,
                type_: 7,
                flags: 0,
                reserved: 0,
            },
            last_stream_id: 1,
            error_code: 0,
            opaque_data: payload.as_mut_ptr(),
            opaque_data_len: payload.len(),
            reserved: 0,
        },
    };
    assert_eq!(
        collector.on_frame_recv(&mut ctx, view(&frame)),
        Err(CallbackError::Failure)
    );
    assert_eq!((collector.len(), collector.owned_bytes()), (0, 0));
    // The shorter payload remains inside the same live allocation.
    frame.goaway.opaque_data_len = 3;
    collector.on_frame_recv(&mut ctx, view(&frame)).unwrap();
    assert_eq!(collector.owned_bytes(), 3);
    assert!(
        matches!(collector.next_event(), Some(SessionEvent::GoAway { opaque_data, .. }) if opaque_data == b"fou")
    );
    assert_eq!(collector.owned_bytes(), 0);
}
