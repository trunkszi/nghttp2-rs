use super::*;

struct TestHandler;

impl SessionHandler for TestHandler {}

fn test_box<H: SessionHandler>(handler: H) -> (Box<CallbackBox<H>>, *mut c_void) {
    let mut boxed = Box::new(CallbackBox::new(handler));
    let user_data = (&mut *boxed as *mut CallbackBox<H>).cast::<c_void>();
    (boxed, user_data)
}

fn frame(type_: u8, stream_id: i32) -> RawFrame {
    // SAFETY: native frame fields are integers and raw pointers. Zeroing the
    // entire union initializes every payload member, including DATA padding.
    let mut frame: RawFrame = unsafe { core::mem::zeroed() };
    frame.hd = FrameHd {
        length: 0,
        stream_id,
        type_,
        flags: 0,
        reserved: 0,
    };
    frame
}

struct RefusingSendHandler;

impl SessionHandler for RefusingSendHandler {
    fn send(&mut self, _: &mut SessionCtx<'_>, _: &[u8], _: c_int) -> nghttp2_ssize {
        CALLBACK_FAILURE as nghttp2_ssize
    }
}

#[test]
fn callbacks_fire_on_a_real_session() {
    let callbacks = Callbacks::<RefusingSendHandler>::new().expect("callback table");
    let (_boxed, user_data) = test_box(RefusingSendHandler);
    let mut session = core::ptr::null_mut();
    // SAFETY: nghttp2 copies the table; the user data outlives the session.
    let code = unsafe {
        nghttp2_sys::nghttp2_session_client_new(&mut session, callbacks.as_ptr(), user_data)
    };
    assert_eq!(code, 0);
    // SAFETY: the session is live and the handler rejects the queued client magic.
    let code = unsafe { nghttp2_sys::nghttp2_session_send(session) };
    assert_eq!(code, CALLBACK_FAILURE);
    // SAFETY: the session was created above and is deleted once.
    unsafe { nghttp2_sys::nghttp2_session_del(session) };
}

struct PanicHandler;

impl SessionHandler for PanicHandler {
    fn on_frame_recv(&mut self, _: &mut SessionCtx<'_>, _: &Frame) -> CallbackResult {
        panic!("boom");
    }
}

#[test]
fn panic_is_caught_and_stored() {
    let (mut boxed, user_data) = test_box(PanicHandler);
    let session = NonNull::<nghttp2_session>::dangling().as_ptr();
    let frame = frame(0, 1);
    // SAFETY: the frame is valid; the handler never touches the session pointer.
    let code = unsafe { trampoline_on_frame_recv::<PanicHandler>(session, &frame, user_data) };
    assert_eq!(code, CALLBACK_FAILURE);
    let payload = boxed.take_panic().expect("panic payload stored");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"boom"));
}

#[test]
fn reentrant_callback_is_refused_and_the_guard_resets() {
    let (_boxed, user_data) = test_box(TestHandler);
    let session = NonNull::<nghttp2_session>::dangling().as_ptr();
    // The nested call hits the re-entry guard before it can touch the handler.
    let outer = with_handler::<TestHandler, c_int>(user_data, session, CALLBACK_FAILURE, |_, _| {
        let inner =
            with_handler::<TestHandler, c_int>(user_data, session, CALLBACK_FAILURE, |_, _| 7);
        assert_eq!(inner, CALLBACK_FAILURE);
        42
    });
    assert_eq!(outer, 42);
    // The guard must be clear again after the outer call returns.
    let after = with_handler::<TestHandler, c_int>(user_data, session, CALLBACK_FAILURE, |_, _| 9);
    assert_eq!(after, 9);
}

#[test]
fn null_pointers_return_callback_failure() {
    let (_boxed, user_data) = test_box(TestHandler);
    let session = NonNull::<nghttp2_session>::dangling().as_ptr();
    // SAFETY: each trampoline checks its null argument before dereferencing.
    let code =
        unsafe { trampoline_on_frame_recv::<TestHandler>(session, core::ptr::null(), user_data) };
    assert_eq!(code, CALLBACK_FAILURE);
    // SAFETY: the frame is valid; a null user_data is rejected before use.
    let code = unsafe {
        trampoline_on_frame_recv::<TestHandler>(session, &frame(0, 1), core::ptr::null_mut())
    };
    assert_eq!(code, CALLBACK_FAILURE);
    // SAFETY: a null session is rejected before any use.
    let code = unsafe {
        trampoline_on_frame_recv::<TestHandler>(core::ptr::null_mut(), &frame(0, 1), user_data)
    };
    assert_eq!(code, CALLBACK_FAILURE);
}

#[test]
fn control_defaults_are_protocol_safe() {
    let mut handler = TestHandler;
    let raw = NonNull::<nghttp2_session>::dangling().as_ptr();
    // SAFETY: the handle only stores the pointer; its methods never dereference it.
    let mut ctx = unsafe { SessionCtx::from_raw(raw) }.expect("non-null");
    let raw_frame = frame(0, 1);
    // SAFETY: the initialized header is valid for the complete borrow.
    let data_frame = unsafe { Frame::from_raw_ref(&raw_frame) };
    let source = nghttp2_sys::nghttp2_data_source { fd: 0 };
    let framehd = [0_u8; FRAME_HEADER_LEN];

    assert_eq!(
        handler.send(&mut ctx, b"x", 0),
        NGHTTP2_ERR_WOULDBLOCK as nghttp2_ssize
    );
    assert_eq!(handler.select_padding(&mut ctx, data_frame, 100), 0);
    let read = handler.data_source_read_length(&mut ctx, FrameType::DATA, 1, 10, 20, 30);
    assert_eq!(read, 10);
    let data = handler.send_data(&mut ctx, data_frame, &framehd, 0, &source);
    assert_eq!(data, CALLBACK_FAILURE);
    let packed = handler.pack_extension(&mut ctx, &mut [], data_frame);
    assert_eq!(packed, NGHTTP2_ERR_CANCEL as nghttp2_ssize);
}

struct CountHandler(nghttp2_ssize);
impl SessionHandler for CountHandler {
    fn send(&mut self, _: &mut SessionCtx<'_>, _: &[u8], _: c_int) -> nghttp2_ssize {
        self.0
    }
    fn recv(&mut self, _: &mut SessionCtx<'_>, buf: &mut [u8], _: c_int) -> nghttp2_ssize {
        assert!(buf.iter().all(|byte| *byte == 0));
        self.0
    }
    fn pack_extension(
        &mut self,
        _: &mut SessionCtx<'_>,
        buf: &mut [u8],
        _: &Frame,
    ) -> nghttp2_ssize {
        assert!(buf.iter().all(|byte| *byte == 0));
        self.0
    }
}

#[test]
fn unit_scenario_send_counts_reject_overruns_and_zero_progress() {
    for (count, expected) in [
        (nghttp2_ssize::MAX, CALLBACK_FAILURE as nghttp2_ssize),
        (0, NGHTTP2_ERR_WOULDBLOCK as nghttp2_ssize),
        (1, 1),
        (
            NGHTTP2_ERR_WOULDBLOCK as nghttp2_ssize,
            NGHTTP2_ERR_WOULDBLOCK as nghttp2_ssize,
        ),
    ] {
        let (_boxed, user_data) = test_box(CountHandler(count));
        // SAFETY: the input is live and the handler does not dereference the session.
        let result = unsafe {
            trampoline_send::<CountHandler>(
                NonNull::dangling().as_ptr(),
                b"x".as_ptr(),
                1,
                0,
                user_data,
            )
        };
        assert_eq!(result, expected);
    }
}

#[test]
fn unit_scenario_output_buffers_are_initialized_and_counts_are_checked() {
    for count in [0, 4, 5, NGHTTP2_ERR_WOULDBLOCK as nghttp2_ssize] {
        let (_boxed, user_data) = test_box(CountHandler(count));
        let mut output = [core::mem::MaybeUninit::<u8>::uninit(); 4];
        let raw_frame = frame(0x10, 1);
        let expected = if count > 4 {
            CALLBACK_FAILURE as nghttp2_ssize
        } else {
            count
        };
        // SAFETY: the writable destination spans four bytes and the initialized
        // frame is live; the handler never dereferences the session pointer.
        let received = unsafe {
            trampoline_recv::<CountHandler>(
                NonNull::dangling().as_ptr(),
                output.as_mut_ptr().cast(),
                4,
                0,
                user_data,
            )
        };
        assert_eq!(received, expected);
        // SAFETY: the same buffer is writable and the frame remains live.
        let packed = unsafe {
            trampoline_pack_extension::<CountHandler>(
                NonNull::dangling().as_ptr(),
                output.as_mut_ptr().cast(),
                4,
                &raw_frame,
                user_data,
            )
        };
        assert_eq!(packed, expected);
    }
}

struct RejectNotification;
impl SessionHandler for RejectNotification {
    fn on_frame_recv(&mut self, _: &mut SessionCtx<'_>, _: &Frame) -> CallbackResult {
        Err(CallbackError::Failure)
    }
}

#[test]
fn unit_scenario_notification_failure_reaches_native_caller() {
    let (_boxed, user_data) = test_box(RejectNotification);
    // SAFETY: the frame is initialized; the handler does not touch the session.
    let code = unsafe {
        trampoline_on_frame_recv::<RejectNotification>(
            NonNull::dangling().as_ptr(),
            &frame(0, 1),
            user_data,
        )
    };
    assert_eq!(code, CALLBACK_FAILURE);
}

struct PanicOnDrop;
impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("payload destructor");
    }
}
struct PayloadPanicHandler(usize);
impl SessionHandler for PayloadPanicHandler {
    fn on_frame_recv(&mut self, _: &mut SessionCtx<'_>, _: &Frame) -> CallbackResult {
        self.0 += 1;
        std::panic::panic_any(PanicOnDrop);
    }
}

#[test]
fn unit_scenario_panic_poisons_dispatch_even_after_payload_is_taken() {
    let (mut boxed, user_data) = test_box(PayloadPanicHandler(0));
    for _ in 0..2 {
        // SAFETY: the frame is initialized and the handler ignores the session.
        let code = unsafe {
            trampoline_on_frame_recv::<PayloadPanicHandler>(
                NonNull::dangling().as_ptr(),
                &frame(0, 1),
                user_data,
            )
        };
        assert_eq!(code, CALLBACK_FAILURE);
    }
    assert_eq!(boxed.handler_mut().0, 1);
    let payload = boxed.take_panic().expect("original payload");
    // The application owns the payload and controls where its destructor runs.
    assert!(catch_unwind(AssertUnwindSafe(|| drop(payload))).is_err());
    // SAFETY: the same initialized frame is used; poisoned dispatch cannot invoke H.
    let code = unsafe {
        trampoline_on_frame_recv::<PayloadPanicHandler>(
            NonNull::dangling().as_ptr(),
            &frame(0, 1),
            user_data,
        )
    };
    assert_eq!(code, CALLBACK_FAILURE);
    assert_eq!(boxed.handler_mut().0, 1);
}
