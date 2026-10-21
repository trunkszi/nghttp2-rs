//! Stream introspection.
//!
//! `nghttp2_stream` is an opaque handle returned by `nghttp2_session_find_stream`.
//! It identifies a stream without transferring ownership. Except for the root
//! stream (ID 0), a returned handle is valid only until the next call to
//! `nghttp2_session_send`, `nghttp2_session_mem_send2`, `nghttp2_session_recv`, or
//! `nghttp2_session_mem_recv2`. Those calls may close and free streams, so a
//! handle retained past them can dangle.
//!
//! Queries through a stream handle are read-only and do not affect the session's
//! scheduling decisions.

use core::marker::PhantomData;

pub use nghttp2_sys::nghttp2_stream;

use crate::{
    context::{SessionPtr, StreamPtr},
    types::StreamProtoState,
};

/// Opaque native stream.
pub type RawStream = nghttp2_stream;

/// Native stream protocol state.
pub type RawStreamState = nghttp2_sys::nghttp2_stream_proto_state;

/// Returns the numeric stream identifier, or `0` for a null handle.
///
/// A null handle and the root stream both report `0`; callers that need to tell
/// them apart must test the pointer before calling this function.
///
/// # Safety
///
/// `stream` must be null or a live handle owned by a session.
#[must_use]
pub unsafe fn stream_id(stream: *mut RawStream) -> i32 {
    if stream.is_null() {
        return 0;
    }
    // SAFETY: the caller guarantees a live stream handle.
    unsafe { nghttp2_sys::nghttp2_stream_get_stream_id(stream) }
}

/// Non-owning view of a stream owned by a session.
///
/// The handle is valid only until the next send/receive call on the session
/// (`nghttp2_session_send`, `nghttp2_session_mem_send2`, `nghttp2_session_recv`,
/// or `nghttp2_session_mem_recv2`); the root stream is the exception and lives
/// for the duration of the process. The lifetime parameter ties the view to the
/// borrow that produced it, but that borrow is of the pointer value, not of the
/// session owner; neither the session lifetime nor the send/receive invalidation
/// window can be enforced by this type, so callers must respect both.
///
/// Two views compare equal exactly when they refer to the same native stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamRef<'a> {
    stream: StreamPtr,
    _session: PhantomData<&'a ()>,
}

impl<'a> StreamRef<'a> {
    /// Wraps a native stream pointer owned by a live session.
    ///
    /// The caller must guarantee that `stream` belongs to a session that is
    /// alive and that the view is not used across the send/receive calls listed
    /// on [`StreamRef`], except for the root stream.
    ///
    /// # Safety
    ///
    /// `stream` must be a live handle owned by a reachable session.
    pub(crate) unsafe fn from_ptr(stream: StreamPtr) -> Self {
        Self {
            stream,
            _session: PhantomData,
        }
    }

    /// Wraps an optional native stream pointer.
    ///
    /// `nghttp2` reports an absent dependency link or a missing stream as a null
    /// pointer; that maps to `None` rather than to an invalid handle.
    fn from_optional_ptr(stream: *mut RawStream) -> Option<Self> {
        let stream = StreamPtr::new(stream)?;
        // SAFETY: the public lookup that reaches this point requires a live session.
        Some(unsafe { Self::from_ptr(stream) })
    }

    /// Returns the numeric stream identifier.
    ///
    /// The root stream reports `0`; stream identifiers assigned to peer streams
    /// are positive.
    #[must_use]
    pub fn id(&self) -> i32 {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // only reads the identifier field.
        unsafe { nghttp2_sys::nghttp2_stream_get_stream_id(self.stream.as_ptr()) }
    }

    /// Returns the HTTP/2 protocol state of the stream.
    ///
    /// The state is derived from the stream's shutdown and close flags rather
    /// than set directly by the peer: a request sent with `END_STREAM` reports
    /// [`StreamProtoState::HALF_CLOSED_LOCAL`], while the root stream always
    /// reports [`StreamProtoState::IDLE`].
    #[must_use]
    pub fn state(&self) -> StreamProtoState {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // only reads the stream's state and shutdown fields.
        let raw = unsafe { nghttp2_sys::nghttp2_stream_get_state(self.stream.as_ptr()) };
        StreamProtoState::from_raw(raw)
    }

    /// Returns the RFC 7540 dependency weight of the stream.
    ///
    /// Upstream removed the RFC 7540 priority tree in favour of RFC 9218, so the
    /// native getter is a compatibility stub that reports
    /// `NGHTTP2_DEFAULT_WEIGHT` for every stream.
    #[must_use]
    pub fn weight(&self) -> i32 {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // does not dereference any caller-owned data.
        unsafe { nghttp2_sys::nghttp2_stream_get_weight(self.stream.as_ptr()) }
    }

    /// Returns the sum of the weights of the streams depending on this one.
    ///
    /// The native getter is a compatibility stub for the removed RFC 7540
    /// dependency tree and always reports `0`, which is never negative.
    #[must_use]
    pub fn sum_dependency_weight(&self) -> i32 {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // does not dereference any caller-owned data.
        unsafe { nghttp2_sys::nghttp2_stream_get_sum_dependency_weight(self.stream.as_ptr()) }
    }

    /// Returns the parent stream in the RFC 7540 dependency tree.
    ///
    /// The native getter returns null unconditionally because upstream removed
    /// that tree, so this is always `None` for the versions this crate tracks.
    #[must_use]
    pub fn parent(&self) -> Option<StreamRef<'a>> {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // only reads the parent link and returns null when there is none.
        let parent = unsafe { nghttp2_sys::nghttp2_stream_get_parent(self.stream.as_ptr()) };
        Self::from_optional_ptr(parent)
    }

    /// Returns the first child in the RFC 7540 dependency tree.
    ///
    /// The native getter returns null unconditionally because upstream removed
    /// that tree, so this is always `None` for the versions this crate tracks.
    #[must_use]
    pub fn first_child(&self) -> Option<StreamRef<'a>> {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // only reads the child link and returns null when there is none.
        let child = unsafe { nghttp2_sys::nghttp2_stream_get_first_child(self.stream.as_ptr()) };
        Self::from_optional_ptr(child)
    }

    /// Returns the next sibling in the RFC 7540 dependency tree.
    ///
    /// The native getter returns null unconditionally because upstream removed
    /// that tree, so this is always `None` for the versions this crate tracks.
    #[must_use]
    pub fn next_sibling(&self) -> Option<StreamRef<'a>> {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // only reads the sibling link and returns null when there is none.
        let sibling = unsafe { nghttp2_sys::nghttp2_stream_get_next_sibling(self.stream.as_ptr()) };
        Self::from_optional_ptr(sibling)
    }

    /// Returns the previous sibling in the RFC 7540 dependency tree.
    ///
    /// The native getter returns null unconditionally because upstream removed
    /// that tree, so this is always `None` for the versions this crate tracks.
    #[must_use]
    pub fn previous_sibling(&self) -> Option<StreamRef<'a>> {
        // SAFETY: `stream` is non-null and owned by a live session; the getter
        // only reads the sibling link and returns null when there is none.
        let sibling =
            unsafe { nghttp2_sys::nghttp2_stream_get_previous_sibling(self.stream.as_ptr()) };
        Self::from_optional_ptr(sibling)
    }
}

/// Stream lookup operations for a non-owning session handle.
///
/// [`SessionPtr`] is a type alias for `NonNull<nghttp2_session>`, and Rust
/// rejects inherent impls on a type defined outside the crate. The lookups are
/// therefore provided by this trait; import it to call `session.find_stream(id)`
/// or `session.root_stream()`.
///
/// `SessionPtr` is a bare pointer with a safe constructor, so a handle that does
/// not come from a live session makes every lookup undefined behavior; only the
/// session layer can hand out the encapsulated form.
pub trait SessionStreams {
    /// Returns the stream with the given identifier, or `None` when the session
    /// does not have one.
    ///
    /// The lookup is read-only. A request stream is registered only when its
    /// `HEADERS` frame is serialized, so it is not visible before the send call
    /// that emits it and is gone once the stream has closed.
    ///
    /// `stream_id == 0` resolves to the root stream:
    /// `nghttp2_session_find_stream` special-cases ID 0 instead of returning
    /// null, matching [`Self::root_stream`].
    fn find_stream(&self, stream_id: i32) -> Option<StreamRef<'_>>;

    /// Returns the session's root stream.
    ///
    /// The root stream is a static object outside the peer's stream numbering.
    /// It reports ID `0`, state [`StreamProtoState::IDLE`], and no dependency
    /// links, and it lives for the duration of the process, so the invalidation
    /// window of ordinary [`StreamRef`] handles does not apply to it.
    ///
    /// Returns `None` only if the native getter reports null; the versions this
    /// crate tracks always return the root object.
    fn root_stream(&self) -> Option<StreamRef<'_>>;
}

impl SessionStreams for SessionPtr {
    fn find_stream(&self, stream_id: i32) -> Option<StreamRef<'_>> {
        // SAFETY: the pointer comes from a live session; the lookup only reads
        // the session's stream map and the returned handle is not retained.
        let stream = unsafe { nghttp2_sys::nghttp2_session_find_stream(self.as_ptr(), stream_id) };
        StreamRef::from_optional_ptr(stream)
    }

    fn root_stream(&self) -> Option<StreamRef<'_>> {
        // SAFETY: the pointer comes from a live session; the root stream is a
        // static object, so the returned handle outlives the session itself.
        let stream = unsafe { nghttp2_sys::nghttp2_session_get_root_stream(self.as_ptr()) };
        StreamRef::from_optional_ptr(stream)
    }
}

#[cfg(test)]
mod tests {
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

        fn handle(&self) -> SessionPtr {
            let Some(handle) = SessionPtr::new(self.session) else {
                panic!("test session pointer is null");
            };
            handle
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
}
