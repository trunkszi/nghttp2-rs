use core::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};
use std::sync::Arc;

use nghttp2_sys::{
    NGHTTP2_DATA, NGHTTP2_DEFAULT_WEIGHT, NGHTTP2_ERR_INVALID_ARGUMENT, NGHTTP2_ERR_INVALID_STATE,
    nghttp2_frame, nghttp2_option, nghttp2_option_del, nghttp2_option_new,
    nghttp2_option_set_no_auto_window_update, nghttp2_pack_extension_callback,
    nghttp2_send_data_callback, nghttp2_session, nghttp2_session_callbacks,
    nghttp2_session_callbacks_del, nghttp2_session_callbacks_new,
    nghttp2_session_callbacks_set_pack_extension_callback,
    nghttp2_session_callbacks_set_send_data_callback, nghttp2_session_client_new2,
    nghttp2_session_del, nghttp2_session_mem_recv2, nghttp2_session_mem_send2,
    nghttp2_session_server_new2,
};

use super::*;
// The `nv!` expansion names the crate as `nghttp2`; this alias puts that name in scope.
use crate as nghttp2;
use crate::nv::nv;

/// Owns the raw session and callbacks so tests can drive the FFI directly.
struct TestSession {
    session: *mut nghttp2_session,
    callbacks: *mut nghttp2_session_callbacks,
}

impl TestSession {
    fn new(
        server: bool,
        option: *const nghttp2_option,
        user_data: *mut c_void,
        send_data: nghttp2_send_data_callback,
        pack: nghttp2_pack_extension_callback,
    ) -> Self {
        let mut callbacks = ptr::null_mut();
        // SAFETY: the out-pointer is writable and the call returns an owned object.
        let rv = unsafe { nghttp2_session_callbacks_new(&raw mut callbacks) };
        assert_eq!(rv, 0);
        assert!(!callbacks.is_null());
        // SAFETY: `callbacks` is live and the setters only store the trampoline.
        unsafe {
            nghttp2_session_callbacks_set_send_data_callback(callbacks, send_data);
            nghttp2_session_callbacks_set_pack_extension_callback(callbacks, pack);
        }
        let mut session = ptr::null_mut();
        // SAFETY: `callbacks` outlives the session; `option` is null or a copied owner.
        let rv = unsafe {
            if server {
                nghttp2_session_server_new2(&raw mut session, callbacks, user_data, option)
            } else {
                nghttp2_session_client_new2(&raw mut session, callbacks, user_data, option)
            }
        };
        assert_eq!(rv, 0);
        assert!(!session.is_null());
        Self { session, callbacks }
    }
    fn handle(&self) -> SessionPtr<'_> {
        // SAFETY: this session owns the native allocation throughout the borrow.
        unsafe {
            SessionPtr::from_raw(core::ptr::NonNull::new(self.session).expect("live session"))
        }
    }
    /// Serializes queued frames and returns them with the final return code.
    fn send(&self) -> (Vec<u8>, isize) {
        let mut out = Vec::new();
        loop {
            let mut data: *const u8 = ptr::null();
            // SAFETY: the session is live and the returned buffer is copied at once.
            let len = unsafe { nghttp2_session_mem_send2(self.session, &raw mut data) };
            if len <= 0 {
                return (out, len);
            }
            assert!(!data.is_null());
            // SAFETY: a positive return guarantees `len` readable bytes.
            out.extend_from_slice(unsafe { core::slice::from_raw_parts(data, len as usize) });
        }
    }
    fn receive(&self, bytes: &[u8]) {
        // SAFETY: the session is live and `bytes` is readable during the call.
        let n = unsafe { nghttp2_session_mem_recv2(self.session, bytes.as_ptr(), bytes.len()) };
        assert_eq!(n, bytes.len() as isize);
    }
}

impl Drop for TestSession {
    fn drop(&mut self) {
        // SAFETY: both pointers are owned here and the session goes first.
        unsafe {
            nghttp2_session_del(self.session);
            nghttp2_session_callbacks_del(self.callbacks);
        }
    }
}

fn request_headers() -> [HeaderField<'static>; 4] {
    [
        nv!(":method", "GET"),
        nv!(":path", "/"),
        nv!(":scheme", "https"),
        nv!(":authority", "example.com"),
    ]
}

/// Submits a request on a fresh client and drives one send pass.
fn run_request(provider: Option<&mut DataProvider>) -> (Vec<u8>, isize) {
    let client = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
    let request = request_headers();
    // SAFETY: the session and provider are live for the call and the send pass.
    let id = unsafe {
        client
            .handle()
            .submit_request(None, &request, provider, ptr::null_mut())
    };
    assert_eq!(id, Ok(1));
    client.send()
}

#[test]
fn submit_request_response_headers_and_trailer() {
    let client = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
    let h = client.handle();
    let request = request_headers();
    // SAFETY: the session is live, the headers are static, and no provider is passed.
    let submitted = unsafe { h.submit_request(None, &request, None, ptr::null_mut()) };
    assert_eq!(submitted, Ok(1));
    let (wire, code) = client.send();
    assert_eq!(code, 0);
    let server = TestSession::new(true, ptr::null(), ptr::null_mut(), None, None);
    server.receive(&wire);
    // SAFETY: the session is live and the response headers are static.
    let submitted = unsafe {
        server
            .handle()
            .submit_response(1, &[nv!(":status", "200")], None)
    };
    assert_eq!(submitted, Ok(()));
    let (response, code) = server.send();
    assert_eq!(code, 0);
    assert!(!response.is_empty(), "response was not serialized");
    // SAFETY: `request` is static and the session is live.
    let submitted =
        unsafe { h.submit_headers(FrameFlags::NONE, -1, None, &request, ptr::null_mut()) };
    assert!(matches!(submitted, Ok(id) if id > 0));
    assert_eq!(client.send().1, 0);
    // SAFETY: the trailer headers are static and the session is live.
    let submitted = unsafe { h.submit_trailer(1, &[nv!("x-trailer", "done")]) };
    assert_eq!(submitted, Ok(()));
}

#[test]
fn control_frames_follow_session_role() {
    let client = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
    let h = client.handle();
    let request = request_headers();
    // SAFETY: the session is live and the request headers are static.
    let submitted = unsafe { h.submit_request(None, &request, None, ptr::null_mut()) };
    assert_eq!(submitted, Ok(1));
    assert_eq!(client.send().1, 0);
    assert_eq!(
        h.submit_rst_stream(FrameFlags::NONE, 1, ErrorCode::CANCEL),
        Ok(())
    );
    assert_eq!(
        h.submit_settings(&[SettingsEntry::new(SettingsId::MAX_CONCURRENT_STREAMS, 100)]),
        Ok(())
    );
    assert_eq!(
        (
            h.submit_ping(FrameFlags::NONE, Some(&[1, 2, 3, 4, 5, 6, 7, 8])),
            h.submit_goaway(FrameFlags::NONE, 0, ErrorCode::NO_ERROR, b"bye")
        ),
        (Ok(()), Ok(()))
    );
    assert_eq!(
        (
            h.submit_window_update(FrameFlags::NONE, 0, 1000),
            h.submit_priority_update(FrameFlags::NONE, 1, b"u=1")
        ),
        (Ok(()), Ok(()))
    );
    // Stream 1 exists but no provider deferred it.
    assert_eq!(
        h.resume_data(1),
        Err(Error::from_native(NGHTTP2_ERR_INVALID_ARGUMENT))
    );
    assert_eq!(
        h.set_local_window_size(FrameFlags::NONE, 0, 100_000),
        Ok(())
    );
    let state = Error::from_native(NGHTTP2_ERR_INVALID_STATE);
    assert_eq!(
        (h.change_extpri(1, ExtPri::default(), false), h.extpri(1)),
        (Err(state), Err(state))
    );
    assert_eq!(
        (
            h.submit_altsvc(FrameFlags::NONE, 0, b"https://e.example", b"h3"),
            h.submit_origin(FrameFlags::NONE, &[b"https://e.example"])
        ),
        (Err(state), Err(state))
    );
    assert_eq!(h.submit_shutdown_notice(), Err(state));
    let server = TestSession::new(true, ptr::null(), ptr::null_mut(), None, None);
    let h = server.handle();
    // Without a pending SETTINGS_NO_RFC7540_PRIORITIES value the server ignores both
    // extensible-priority calls, even for a missing stream.
    assert_eq!(
        (h.change_extpri(1, ExtPri::default(), false), h.extpri(1)),
        (Ok(()), Ok(ExtPri::default()))
    );
    assert_eq!(
        h.submit_altsvc(FrameFlags::NONE, 0, b"https://e.example", b"h3"),
        Ok(())
    );
    assert_eq!(
        h.submit_origin(FrameFlags::NONE, &[b"https://e.example"]),
        Ok(())
    );
    assert_eq!(h.submit_shutdown_notice(), Ok(()));
    let mut option = ptr::null_mut();
    // SAFETY: the out-pointer is writable; the option is deleted after the constructor
    // copies its settings.
    unsafe {
        assert_eq!(nghttp2_option_new(&raw mut option), 0);
        nghttp2_option_set_no_auto_window_update(option, 1);
    }
    let consumable = TestSession::new(false, option, ptr::null_mut(), None, None);
    // SAFETY: the session constructor copied the option settings.
    unsafe { nghttp2_option_del(option) };
    let h = consumable.handle();
    assert_eq!(
        (
            h.consume(1, 10),
            h.consume_connection(10),
            h.consume_stream(1, 10)
        ),
        (Ok(()), Ok(()), Ok(()))
    );
    assert_eq!(
        TestSession::new(false, ptr::null(), ptr::null_mut(), None, None)
            .handle()
            .consume_connection(10),
        Err(state)
    );
    // Extension frames need a pack callback, and reject reserved frame types first.
    let mut payload = 0u8;
    let without = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
    // SAFETY: the session is live; the frame is rejected before `payload` is stored.
    let result = unsafe {
        without.handle().submit_extension(
            FrameType::ALTSVC,
            FrameFlags::NONE,
            0,
            (&raw mut payload).cast(),
        )
    };
    assert_eq!(result, Err(Error::from_native(NGHTTP2_ERR_INVALID_STATE)));
    let with = TestSession::new(
        false,
        ptr::null(),
        ptr::null_mut(),
        None,
        Some(pack_extension),
    );
    // SAFETY: the session is live and the frame is never serialized.
    let result = unsafe {
        with.handle().submit_extension(
            FrameType::ALTSVC,
            FrameFlags::NONE,
            0,
            (&raw mut payload).cast(),
        )
    };
    assert_eq!(result, Ok(()));
}

unsafe extern "C" fn pack_extension(
    _session: *mut nghttp2_session,
    _buf: *mut u8,
    _len: usize,
    _frame: *const nghttp2_frame,
    _user_data: *mut c_void,
) -> isize {
    0
}

struct TestSource {
    ready: Option<Arc<AtomicBool>>,
    chunk: &'static [u8],
    step: u8,
    panics: bool,
}

impl DataSource for TestSource {
    fn read(&mut self, _max_len: usize) -> DataPoll<'_> {
        if self.panics {
            panic!("provider boom");
        }
        if let Some(ready) = &self.ready
            && !ready.load(Ordering::SeqCst)
        {
            return DataPoll::Deferred;
        }
        if self.step > 0 {
            return DataPoll::Eof;
        }
        self.step = 1;
        DataPoll::Copy(self.chunk)
    }
}

#[test]
fn copy_provider_splits_payload_at_the_default_max_frame_size() {
    let payload = vec![0x5au8; 20_000];
    let (wire, code) = run_request(Some(&mut DataProvider::from_vec(payload.clone())));
    // Upstream caps one DATA payload at the remote SETTINGS_MAX_FRAME_SIZE, 16384 bytes.
    let data = &wire[wire.len() - (9 + 16_384 + 9 + 3_616 + 9)..];
    let second = 9 + 16_384;
    assert_eq!(
        (&data[..5], &data[second..second + 5]),
        (
            &[0x00u8, 0x40, 0x00, NGHTTP2_DATA as u8, 0][..],
            &[0x00u8, 0x0e, 0x20, NGHTTP2_DATA as u8, 0][..]
        )
    );
    assert_eq!(
        (&data[9..second], &data[second + 9..second + 9 + 3_616]),
        (&payload[..16_384], &payload[16_384..])
    );
    assert_eq!(
        (code, &data[data.len() - 9..]),
        (
            0,
            &[
                0u8,
                0,
                0,
                NGHTTP2_DATA as u8,
                FrameFlags::END_STREAM.bits(),
                0,
                0,
                0,
                1
            ][..]
        )
    );
}

#[test]
fn empty_payload_sends_a_zero_length_data_frame_with_end_stream() {
    let (wire, code) = run_request(Some(&mut DataProvider::from_vec(Vec::new())));
    assert_eq!(
        (code, &wire[wire.len() - 9..]),
        (
            0,
            &[
                0u8,
                0,
                0,
                NGHTTP2_DATA as u8,
                FrameFlags::END_STREAM.bits(),
                0,
                0,
                0,
                1
            ][..]
        )
    );
}

unsafe extern "C" fn capture(
    _session: *mut nghttp2_session,
    _frame: *mut nghttp2_frame,
    framehd: *const u8,
    _length: usize,
    source: *mut RawDataSource,
    user_data: *mut c_void,
) -> i32 {
    // SAFETY: the test installs a live Vec<u8> at `user_data` for the session.
    let captured = unsafe { &mut *user_data.cast::<Vec<u8>>() };
    // SAFETY: nghttp2 passes a nine-byte frame header valid for the call.
    captured.extend_from_slice(unsafe { core::slice::from_raw_parts(framehd, 9) });
    // SAFETY: `source` belongs to the provider that produced this DATA frame.
    if let Some((ptr, len)) = unsafe { no_copy_chunk(source) } {
        // SAFETY: the chunk stays valid until this callback returns.
        captured.extend_from_slice(unsafe { core::slice::from_raw_parts(ptr, len) });
    }
    0
}

#[test]
fn no_copy_provider_sends_through_the_send_data_callback() {
    let mut captured = Vec::new();
    {
        let mut provider = DataProvider::from_vec_no_copy(b"no-copy-data".to_vec());
        let client = TestSession::new(
            false,
            ptr::null(),
            (&raw mut captured).cast(),
            Some(capture),
            None,
        );
        let request = request_headers();
        // SAFETY: the session and provider are live and the provider outlives `send`.
        let id = unsafe {
            client
                .handle()
                .submit_request(None, &request, Some(&mut provider), ptr::null_mut())
        };
        assert_eq!(id, Ok(1));
        let (wire, code) = client.send();
        assert_eq!(code, 0);
        let tail = wire.len() - 9;
        assert_eq!(
            (wire[tail + 3], wire[tail + 4]),
            (NGHTTP2_DATA as u8, FrameFlags::END_STREAM.bits())
        );
    }
    assert_eq!((captured.len(), captured[3]), (9 + 12, NGHTTP2_DATA as u8));
    assert_eq!(&captured[0..3], &[0u8, 0, 12]);
    assert_eq!(&captured[9..21], b"no-copy-data");
}

#[test]
fn no_copy_provider_splits_payload_across_send_data_calls() {
    let mut captured = Vec::new();
    let mut provider = DataProvider::from_vec_no_copy(vec![0xa5u8; 20_000]);
    let client = TestSession::new(
        false,
        ptr::null(),
        (&raw mut captured).cast(),
        Some(capture),
        None,
    );
    // SAFETY: the session and provider are live and the provider outlives `send`.
    let id = unsafe {
        client.handle().submit_request(
            None,
            &request_headers(),
            Some(&mut provider),
            ptr::null_mut(),
        )
    };
    assert_eq!(id, Ok(1));
    assert_eq!(client.send().1, 0);
    // Each send_data call contributes a nine-byte frame header before its chunk.
    assert_eq!(
        (&captured[..5], &captured[9 + 16_384..9 + 16_384 + 5]),
        (
            &[0x00u8, 0x40, 0x00, NGHTTP2_DATA as u8, 0][..],
            &[0x00u8, 0x0e, 0x20, NGHTTP2_DATA as u8, 0][..]
        )
    );
    assert_eq!(
        captured.iter().filter(|&&byte| byte == 0xa5).count(),
        20_000
    );
}

#[test]
fn deferred_provider_waits_for_resume_data() {
    let ready = Arc::new(AtomicBool::new(false));
    let mut provider = DataProvider::new(TestSource {
        ready: Some(Arc::clone(&ready)),
        chunk: b"deferred",
        step: 0,
        panics: false,
    });
    let client = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
    let request = request_headers();
    // SAFETY: the session and provider are live and the provider outlives both sends.
    let id = unsafe {
        client
            .handle()
            .submit_request(None, &request, Some(&mut provider), ptr::null_mut())
    };
    assert_eq!(id, Ok(1));
    let (wire, code) = client.send();
    assert_eq!(code, 0);
    assert!(
        !wire.windows(8).any(|window| window == b"deferred"),
        "sent before resume"
    );
    assert_eq!(client.handle().resume_data(1), Ok(()));
    ready.store(true, Ordering::SeqCst);
    let (wire, code) = client.send();
    assert_eq!(code, 0);
    assert!(
        wire.windows(8).any(|window| window == b"deferred"),
        "not sent after resume"
    );
}

#[test]
fn provider_panic_is_reported_as_callback_failure() {
    let mut provider = DataProvider::new(TestSource {
        ready: None,
        chunk: b"",
        step: 0,
        panics: true,
    });
    let (_, code) = run_request(Some(&mut provider));
    assert_eq!(code, NGHTTP2_ERR_CALLBACK_FAILURE as isize);
    let Some(payload) = provider.take_panic() else {
        panic!("panic payload missing")
    };
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"provider boom"));
}

#[test]
fn priority_and_settings_convert_to_the_native_layout() {
    let spec = PrioritySpec::new(3, 200, true);
    assert_eq!(
        (spec.dependency(), spec.weight(), spec.exclusive()),
        (3, 200, true)
    );
    // SAFETY: `as_raw` points at the descriptor owned by `spec`, still alive.
    let raw = unsafe { *spec.as_raw() };
    assert_eq!((raw.stream_id, raw.weight, raw.exclusive), (3, 200, 1));
    let default = PrioritySpec::default_for(7);
    assert_eq!(
        (default.dependency(), default.weight(), default.exclusive()),
        (7, NGHTTP2_DEFAULT_WEIGHT as i32, false)
    );
    let entry = SettingsEntry::new(SettingsId::MAX_FRAME_SIZE, 16_384);
    assert_eq!(
        (entry.id(), entry.value()),
        (SettingsId::MAX_FRAME_SIZE, 16_384)
    );
    assert_eq!(
        (entry.as_raw().settings_id, entry.as_raw().value),
        (5, 16_384)
    );
    let provider = DataProvider::from_vec(b"abc".to_vec());
    let raw = provider.as_raw();
    assert!(!raw.is_null());
    // SAFETY: the provider is alive and `as_raw` points at its descriptor.
    let before = unsafe { (*raw).source.ptr };
    assert!(!before.is_null());
    let moved = provider;
    // SAFETY: `moved` now owns the descriptor, which is still alive.
    assert_eq!(unsafe { (*moved.as_raw()).source.ptr }, before);
}

struct TrailerSource {
    body_sent: bool,
}

impl DataSource for TrailerSource {
    fn read(&mut self, _max_len: usize) -> DataPoll<'_> {
        if self.body_sent {
            DataPoll::EofWithTrailers
        } else {
            self.body_sent = true;
            DataPoll::Copy(b"response body")
        }
    }
}

fn wire_frames(mut wire: &[u8]) -> Vec<(u8, u8, i32, &[u8])> {
    let mut frames = Vec::new();
    while !wire.is_empty() {
        assert!(wire.len() >= 9);
        let len = (usize::from(wire[0]) << 16) | (usize::from(wire[1]) << 8) | usize::from(wire[2]);
        let id =
            i32::from_be_bytes(wire[5..9].try_into().expect("stream identifier")) & 0x7fff_ffff;
        let end = 9 + len;
        assert!(end <= wire.len());
        frames.push((wire[3], wire[4], id, &wire[9..end]));
        wire = &wire[end..];
    }
    frames
}

#[test]
fn integration_response_body_finishes_before_trailing_headers_close_stream() {
    let mut provider = DataProvider::new(TrailerSource { body_sent: false });
    let client = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
    client
        .handle()
        .submit_settings(&[])
        .expect("client settings");
    // SAFETY: headers are static and no provider is retained.
    unsafe {
        client
            .handle()
            .submit_request(None, &request_headers(), None, ptr::null_mut())
    }
    .expect("request");
    let (request, code) = client.send();
    assert_eq!(code, 0);
    let server = TestSession::new(true, ptr::null(), ptr::null_mut(), None, None);
    server.receive(&request);
    // SAFETY: static headers and the provider outlive the server and serialization.
    unsafe {
        server
            .handle()
            .submit_response(1, &[nv!(":status", "200")], Some(&mut provider))
    }
    .expect("response");
    let (body, code) = server.send();
    assert_eq!(code, 0);
    let frames = wire_frames(&body);
    let data: Vec<_> = frames
        .iter()
        .filter(|frame| frame.0 == NGHTTP2_DATA as u8)
        .collect();
    assert!(!data.is_empty());
    assert!(
        data.iter()
            .all(|frame| frame.1 & FrameFlags::END_STREAM.bits() == 0)
    );
    let payload: Vec<_> = data
        .iter()
        .flat_map(|frame| frame.3.iter().copied())
        .collect();
    assert_eq!(payload, b"response body");
    // SAFETY: trailer headers are static and remain valid until serialization completes.
    unsafe {
        server
            .handle()
            .submit_trailer(1, &[nv!("x-trailer", "done")])
    }
    .expect("trailer");
    let (trailers, code) = server.send();
    assert_eq!(code, 0);
    let frames = wire_frames(&trailers);
    assert!(
        frames
            .iter()
            .any(|frame| frame.0 == nghttp2_sys::NGHTTP2_HEADERS as u8
                && frame.2 == 1
                && frame.1 & FrameFlags::END_STREAM.bits() != 0)
    );
}

#[test]
fn integration_provider_move_and_panic_access_preserve_native_source() {
    let ready = Arc::new(AtomicBool::new(false));
    let mut provider = DataProvider::new(TestSource {
        ready: Some(Arc::clone(&ready)),
        chunk: b"retained",
        step: 0,
        panics: false,
    });
    // SAFETY: the descriptor belongs to the live provider.
    let address = unsafe { (*provider.as_raw()).source.ptr };
    assert!(provider.take_panic().is_none());
    let mut provider = provider;
    assert!(provider.take_panic().is_none());
    // SAFETY: the moved provider still owns the descriptor and state allocation.
    assert_eq!(unsafe { (*provider.as_raw()).source.ptr }, address);
    let client = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
    // SAFETY: the provider outlives the session and all send passes; headers are static.
    unsafe {
        client.handle().submit_request(
            None,
            &request_headers(),
            Some(&mut provider),
            ptr::null_mut(),
        )
    }
    .expect("request");
    assert_eq!(client.send().1, 0);
    assert!(provider.take_panic().is_none());
    let mut provider = provider;
    assert!(provider.take_panic().is_none());
    ready.store(true, Ordering::SeqCst);
    client
        .handle()
        .resume_data(1)
        .expect("resume deferred source");
    let (wire, code) = client.send();
    assert_eq!(code, 0);
    assert!(wire.windows(8).any(|bytes| bytes == b"retained"));
    assert!(provider.take_panic().is_none());
    // SAFETY: the provider continues to own this same allocation after native reads.
    assert_eq!(unsafe { (*provider.as_raw()).source.ptr }, address);
    drop(client);
}

#[test]
fn integration_context_submits_control_frames_and_borrows_stream_views() {
    use crate::stream::SessionStreams;
    let client = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
    // SAFETY: the native session remains live for this scoped context.
    let ctx = unsafe { SessionCtx::from_raw(client.session) }.expect("live context");
    ctx.submit_settings(&[]).expect("settings through context");
    SessionSubmit::submit_ping(&ctx, FrameFlags::NONE, Some(b"ctx-ping"))
        .expect("ping through context");
    assert_eq!(
        SessionStreams::root_stream(&ctx).expect("root stream").id(),
        0
    );
    assert!(ctx.find_stream(99).is_none());
    let (wire, code) = client.send();
    assert_eq!(code, 0);
    assert!(wire.windows(8).any(|bytes| bytes == b"ctx-ping"));
}

#[test]
fn unit_lifecycle_provider_releases_its_owned_source_once_after_moves() {
    use core::sync::atomic::AtomicUsize;
    struct CountedSource(Arc<AtomicUsize>);
    impl DataSource for CountedSource {
        fn read(&mut self, _max_len: usize) -> DataPoll<'_> {
            DataPoll::Eof
        }
    }
    impl Drop for CountedSource {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicUsize::new(0));
    let provider = DataProvider::new(CountedSource(Arc::clone(&dropped)));
    let mut moved = provider;
    assert!(moved.take_panic().is_none());
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(moved);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn integration_provider_retains_inline_source_remainder_across_native_reads() {
    struct InlineSource {
        bytes: [u8; 20_000],
        emitted: bool,
    }
    impl DataSource for InlineSource {
        fn read(&mut self, _max_len: usize) -> DataPoll<'_> {
            if self.emitted {
                DataPoll::Eof
            } else {
                self.emitted = true;
                DataPoll::Copy(&self.bytes)
            }
        }
    }
    let mut provider = DataProvider::new(InlineSource {
        bytes: [0xa5; 20_000],
        emitted: false,
    });
    let (wire, code) = run_request(Some(&mut provider));
    assert_eq!(code, 0);
    let frames = wire_frames(&wire[24..]);
    let payload: Vec<_> = frames
        .iter()
        .filter(|frame| frame.0 == NGHTTP2_DATA as u8)
        .flat_map(|frame| frame.3.iter().copied())
        .collect();
    assert_eq!(payload, [0xa5; 20_000]);
}

#[test]
fn deferred_constructor_completes_empty_body_after_resume() {
    let mut provider = DataProvider::deferred();
    let client = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
    // SAFETY: headers are static and provider remains alive through all native drives.
    let id = unsafe {
        client.handle().submit_request(
            None,
            &request_headers(),
            Some(&mut provider),
            ptr::null_mut(),
        )
    }
    .unwrap();
    let (first, code) = client.send();
    assert_eq!(code, 0);
    assert!(
        !wire_frames(&first[24..])
            .iter()
            .any(|(kind, flags, _, _)| *kind == NGHTTP2_DATA as u8
                && flags & FrameFlags::END_STREAM.bits() != 0)
    );
    client.handle().resume_data(id).unwrap();
    let (last, code) = client.send();
    assert_eq!(code, 0);
    assert!(
        wire_frames(&last)
            .iter()
            .any(|(kind, flags, _, _)| *kind == NGHTTP2_DATA as u8
                && flags & FrameFlags::END_STREAM.bits() != 0)
    );
}
