use core::ptr;

use nghttp2_sys::{
    NGHTTP2_DEFAULT_WEIGHT, nghttp2_nv, nghttp2_session, nghttp2_session_callbacks,
    nghttp2_session_callbacks_del, nghttp2_session_callbacks_new, nghttp2_session_client_new,
    nghttp2_session_del, nghttp2_session_mem_recv2, nghttp2_session_mem_send2,
    nghttp2_session_server_new, nghttp2_submit_request2, nghttp2_submit_rst_stream,
    nghttp2_submit_settings,
};

use super::*;

/// Owns the raw session and callbacks so tests can exercise the FFI directly.
struct TestSession {
    session: *mut nghttp2_session,
    callbacks: *mut nghttp2_session_callbacks,
}

impl TestSession {
    fn client() -> Self {
        Self::new(false)
    }

    fn server() -> Self {
        Self::new(true)
    }

    fn new(server: bool) -> Self {
        let mut callbacks = ptr::null_mut();
        // SAFETY: the out-pointer is a valid local; on success the library
        // returns an owned callbacks object that `Drop` releases.
        let rv = unsafe { nghttp2_session_callbacks_new(&raw mut callbacks) };
        assert_eq!(rv, 0, "nghttp2_session_callbacks_new failed: {rv}");
        assert!(!callbacks.is_null(), "callbacks were not allocated");

        let mut session = ptr::null_mut();
        // SAFETY: `callbacks` stays alive until `Drop`; the session borrows
        // it, and no user data is stored.
        let rv = unsafe {
            if server {
                nghttp2_session_server_new(&raw mut session, callbacks, ptr::null_mut())
            } else {
                nghttp2_session_client_new(&raw mut session, callbacks, ptr::null_mut())
            }
        };
        assert_eq!(rv, 0, "session creation failed: {rv}");
        assert!(!session.is_null(), "session was not allocated");
        Self { session, callbacks }
    }

    fn handle(&self) -> SessionPtr<'_> {
        // SAFETY: this session owns the native allocation throughout the borrow.
        unsafe {
            SessionPtr::from_raw(core::ptr::NonNull::new(self.session).expect("live session"))
        }
    }

    /// Queues a request with no body, so its HEADERS carries `END_STREAM`.
    fn submit_request(&self) -> i32 {
        let nva = request_headers();
        // SAFETY: the session is live; `nva` points at static byte strings
        // that the library copies during the call and never retains.
        let stream_id = unsafe {
            nghttp2_submit_request2(
                self.session,
                ptr::null(),
                nva.as_ptr(),
                nva.len(),
                ptr::null(),
                ptr::null_mut(),
            )
        };
        assert!(
            stream_id > 0,
            "nghttp2_submit_request2 returned {stream_id}"
        );
        stream_id
    }

    /// Queues an empty SETTINGS frame, which a server requires as the first
    /// frame after the client connection preface.
    fn submit_settings(&self) {
        // SAFETY: the session is live and an empty settings list is valid;
        // the null list is never dereferenced.
        let rv = unsafe { nghttp2_submit_settings(self.session, 0, ptr::null(), 0) };
        assert_eq!(rv, 0, "nghttp2_submit_settings failed: {rv}");
    }

    /// Serializes every queued frame and returns the bytes to send.
    fn send_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut data: *const u8 = ptr::null();
            // SAFETY: the session is live and `data` is a valid out-pointer;
            // the returned buffer is owned by the session and copied below
            // before the next call can overwrite it.
            let len = unsafe { nghttp2_session_mem_send2(self.session, &raw mut data) };
            assert!(len >= 0, "nghttp2_session_mem_send2 failed: {len}");
            if len == 0 {
                break;
            }
            let len = len as usize;
            assert!(!data.is_null(), "mem_send2 reported bytes without a buffer");
            // SAFETY: a positive return guarantees `len` initialized bytes at
            // `data` that stay valid until the next send call.
            let chunk = unsafe { core::slice::from_raw_parts(data, len) };
            out.extend_from_slice(chunk);
        }
        out
    }

    fn receive(&self, bytes: &[u8]) {
        // SAFETY: the session is live and `bytes` stays borrowed for the
        // call; the library copies anything it retains.
        let consumed =
            unsafe { nghttp2_session_mem_recv2(self.session, bytes.as_ptr(), bytes.len()) };
        assert_eq!(
            consumed,
            bytes.len() as isize,
            "mem_recv2 did not consume all input"
        );
    }
}

impl Drop for TestSession {
    fn drop(&mut self) {
        // SAFETY: both pointers were created by this struct, and the session
        // is destroyed before the callbacks it borrows.
        unsafe {
            nghttp2_session_del(self.session);
            nghttp2_session_callbacks_del(self.callbacks);
        }
    }
}

fn nv(name: &'static [u8], value: &'static [u8]) -> nghttp2_nv {
    nghttp2_nv {
        name: name.as_ptr().cast_mut(),
        value: value.as_ptr().cast_mut(),
        namelen: name.len(),
        valuelen: value.len(),
        flags: 0,
    }
}

fn request_headers() -> [nghttp2_nv; 4] {
    [
        nv(b":method", b"GET"),
        nv(b":path", b"/"),
        nv(b":scheme", b"https"),
        nv(b":authority", b"example.com"),
    ]
}

#[test]
fn null_stream_has_no_id() {
    // SAFETY: the function explicitly accepts a null handle.
    assert_eq!(unsafe { stream_id(core::ptr::null_mut()) }, 0);
}

#[test]
fn submitted_request_is_absent_until_the_first_send() {
    let session = TestSession::client();
    let handle = session.handle();
    let stream_id = session.submit_request();

    // Submitting only queues the HEADERS frame; nghttp2 opens the stream
    // while serializing that frame.
    assert!(
        handle.find_stream(stream_id).is_none(),
        "stream {stream_id} must not exist before its HEADERS is sent"
    );

    let _bytes = session.send_bytes();

    let Some(stream) = handle.find_stream(stream_id) else {
        panic!("stream {stream_id} missing after its HEADERS was sent");
    };
    assert_eq!(stream.id(), stream_id);
    // The request carried END_STREAM, so the local side is shut.
    assert_eq!(stream.state(), StreamProtoState::HALF_CLOSED_LOCAL);
}

#[test]
fn root_stream_is_stable_and_new_streams_have_no_parent() {
    let session = TestSession::client();
    let handle = session.handle();
    let Some(root) = handle.root_stream() else {
        panic!("root stream missing");
    };
    assert_eq!(root.id(), 0);
    assert_eq!(root.state(), StreamProtoState::IDLE);

    let stream_id = session.submit_request();
    let _bytes = session.send_bytes();
    let Some(stream) = handle.find_stream(stream_id) else {
        panic!("stream {stream_id} missing after send");
    };
    // nghttp2_stream_get_parent is a stub for the removed RFC 7540
    // dependency tree, so a new stream has no parent object.
    assert_eq!(stream.parent(), None);

    // The root handle is the same static object on every lookup.
    let Some(root_again) = handle.root_stream() else {
        panic!("root stream missing after send");
    };
    assert_eq!(root, root_again);
}

#[test]
fn created_streams_report_the_default_dependency_weights() {
    let session = TestSession::client();
    let handle = session.handle();
    let stream_id = session.submit_request();
    let _bytes = session.send_bytes();

    let Some(stream) = handle.find_stream(stream_id) else {
        panic!("stream {stream_id} missing after send");
    };
    assert_eq!(stream.weight(), NGHTTP2_DEFAULT_WEIGHT as i32);
    // The dependency tree is gone, so nothing depends on this stream.
    assert_eq!(stream.sum_dependency_weight(), 0);
}

#[test]
fn parent_child_and_sibling_links_are_absent() {
    let session = TestSession::client();
    let handle = session.handle();
    let first = session.submit_request();
    let second = session.submit_request();
    let _bytes = session.send_bytes();

    let Some(a) = handle.find_stream(first) else {
        panic!("stream {first} missing after send");
    };
    let Some(b) = handle.find_stream(second) else {
        panic!("stream {second} missing after send");
    };
    assert_ne!(a.id(), b.id());

    // The RFC 7540 dependency tree was removed, so the parent, child, and
    // sibling getters always report null: no ordering between the two
    // streams is observable through them.
    for stream in [a, b] {
        assert_eq!(stream.parent(), None);
        assert_eq!(stream.first_child(), None);
        assert_eq!(stream.next_sibling(), None);
        assert_eq!(stream.previous_sibling(), None);
    }
    let Some(root) = handle.root_stream() else {
        panic!("root stream missing");
    };
    assert_eq!(root.first_child(), None);
}

#[test]
fn missing_ids_are_absent_and_zero_resolves_to_the_root() {
    let session = TestSession::client();
    let handle = session.handle();
    assert!(handle.find_stream(2).is_none());

    // nghttp2_session_find_stream special-cases ID 0 and returns the static
    // root stream instead of null.
    let Some(root) = handle.find_stream(0) else {
        panic!("stream 0 must resolve to the root stream");
    };
    assert_eq!(root.id(), 0);
    assert_eq!(Some(root), handle.root_stream());
}

#[test]
fn repeated_lookups_of_one_stream_compare_equal() {
    let session = TestSession::client();
    let handle = session.handle();
    let first = session.submit_request();
    let second = session.submit_request();
    let _bytes = session.send_bytes();

    let Some(a) = handle.find_stream(first) else {
        panic!("stream {first} missing after send");
    };
    let Some(b) = handle.find_stream(first) else {
        panic!("stream {first} missing after send");
    };
    let Some(other) = handle.find_stream(second) else {
        panic!("stream {second} missing after send");
    };
    assert_eq!(a, b, "two lookups of one id must compare equal");
    assert_ne!(a, other);
}

#[test]
fn closed_streams_are_removed_from_the_lookup() {
    let session = TestSession::client();
    let handle = session.handle();
    let stream_id = session.submit_request();
    let _bytes = session.send_bytes();
    assert!(
        handle.find_stream(stream_id).is_some(),
        "stream {stream_id} missing after its HEADERS was sent"
    );

    // A sent RST_STREAM reaches nghttp2_session_close_stream, which destroys the
    // stream and removes it from the session map that find_stream consults.
    // SAFETY: the session is live and the id was returned by submit_request.
    let rv = unsafe { nghttp2_submit_rst_stream(session.session, 0, stream_id, 0) };
    assert_eq!(rv, 0, "nghttp2_submit_rst_stream failed: {rv}");
    let _bytes = session.send_bytes();
    assert!(
        handle.find_stream(stream_id).is_none(),
        "closed stream {stream_id} is still visible"
    );
}

#[test]
fn server_observes_the_request_stream_as_half_closed_remote() {
    let client = TestSession::client();
    client.submit_settings();
    let stream_id = client.submit_request();
    let bytes = client.send_bytes();

    let server = TestSession::server();
    server.receive(&bytes);

    let server_handle = server.handle();
    let Some(stream) = server_handle.find_stream(stream_id) else {
        panic!("server did not open stream {stream_id}");
    };
    assert_eq!(stream.id(), stream_id);
    // The request HEADERS carried END_STREAM, so the server's read side is
    // shut while its write side remains open.
    assert_eq!(stream.state(), StreamProtoState::HALF_CLOSED_REMOTE);
}
