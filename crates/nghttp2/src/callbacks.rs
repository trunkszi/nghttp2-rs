//! Session callbacks with unwind isolation and failure propagation.
//! Handler reentry and dispatch after a panic fail closed.
use core::{
    any::Any,
    cell::{Cell, RefCell, UnsafeCell},
    ffi::{c_char, c_int, c_void},
    marker::PhantomData,
    panic::AssertUnwindSafe,
    ptr::NonNull,
};
use std::panic::catch_unwind;

use nghttp2_sys::{
    NGHTTP2_ERR_CANCEL, NGHTTP2_ERR_WOULDBLOCK, nghttp2_data_source, nghttp2_frame as RawFrame,
    nghttp2_frame_hd as FrameHd, nghttp2_session, nghttp2_session_callbacks, nghttp2_ssize,
};

use crate::{
    context::SessionCtx,
    error::{Error, NgHttp2Error, Result},
    rcbuf::RcBuf,
    types::{ErrorCode, FrameType},
};
/// Immutable native frame whose active payload remains owned by nghttp2.
/// Borrowing an arbitrary native union requires checking its native invariants:
///
/// ```compile_fail
/// fn forge(raw: &nghttp2::sys::nghttp2_frame) {
///     nghttp2::callbacks::Frame::from_raw_ref(raw);
/// }
/// ```
/// Safe handlers cannot overwrite native ownership fields:
/// ```compile_fail
/// fn corrupt(frame: &mut nghttp2::callbacks::Frame, raw: nghttp2::sys::nghttp2_frame) {
///     **frame = raw;
/// }
/// ```
#[repr(transparent)]
pub struct Frame(RawFrame);
impl Frame {
    /// Borrows a native frame without allowing its ownership fields to change.
    ///
    /// # Safety
    ///
    /// The active union member must match its frame type, and every referenced
    /// payload must remain valid and immutable for the duration of the borrow.
    pub unsafe fn from_raw_ref(raw: &RawFrame) -> &Self {
        // SAFETY: the transparent wrapper has the same layout as the native frame.
        unsafe { &*core::ptr::from_ref(raw).cast::<Self>() }
    }
}
impl core::ops::Deref for Frame {
    type Target = RawFrame;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
/// A handler failure that terminates native callback processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CallbackError {
    /// The callback cannot complete its operation.
    #[error("native callback processing failed")]
    Failure,
}
/// Result of a notification callback.
pub type CallbackResult = core::result::Result<(), CallbackError>;
fn notification_code(result: CallbackResult) -> c_int {
    match result {
        Ok(()) => 0,
        Err(CallbackError::Failure) => CALLBACK_FAILURE,
    }
}
/// Native callback failure code returned after a caught Rust panic.
pub const CALLBACK_FAILURE: c_int = nghttp2_sys::NGHTTP2_ERR_CALLBACK_FAILURE;
/// Size of the serialized frame header passed to `send_data`.
const FRAME_HEADER_LEN: usize = 9;
/// Session callbacks; notification errors terminate native processing.
pub trait SessionHandler {
    /// Transmits bytes produced by the library.
    fn send(&mut self, _: &mut SessionCtx<'_>, _: &[u8], _: c_int) -> nghttp2_ssize {
        NGHTTP2_ERR_WOULDBLOCK as nghttp2_ssize
    }
    /// Receives bytes into the supplied buffer.
    /// The buffer is initialized to zero before the handler is called.
    fn recv(&mut self, _: &mut SessionCtx<'_>, _: &mut [u8], _: c_int) -> nghttp2_ssize {
        NGHTTP2_ERR_WOULDBLOCK as nghttp2_ssize
    }
    /// A complete frame was received.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn on_frame_recv(&mut self, _: &mut SessionCtx<'_>, _: &Frame) -> CallbackResult {
        Ok(())
    }
    /// An invalid frame was received.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn on_invalid_frame_recv(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: &Frame,
        _: NgHttp2Error,
    ) -> CallbackResult {
        Ok(())
    }
    /// A chunk of `DATA` payload was received.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn on_data_chunk_recv(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: u8,
        _: i32,
        _: &[u8],
    ) -> CallbackResult {
        Ok(())
    }
    /// A frame is about to be sent.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn before_frame_send(&mut self, _: &mut SessionCtx<'_>, _: &Frame) -> CallbackResult {
        Ok(())
    }
    /// A frame was sent.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn on_frame_send(&mut self, _: &mut SessionCtx<'_>, _: &Frame) -> CallbackResult {
        Ok(())
    }
    /// A frame could not be sent.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn on_frame_not_send(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: &Frame,
        _: NgHttp2Error,
    ) -> CallbackResult {
        Ok(())
    }
    /// A stream was closed.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn on_stream_close(&mut self, _: &mut SessionCtx<'_>, _: i32, _: ErrorCode) -> CallbackResult {
        Ok(())
    }
    /// A header block started.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn on_begin_headers(&mut self, _: &mut SessionCtx<'_>, _: &Frame) -> CallbackResult {
        Ok(())
    }
    /// A header name/value pair was received.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn on_header(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: &Frame,
        _: &[u8],
        _: &[u8],
        _: u8,
    ) -> CallbackResult {
        Ok(())
    }
    /// A header pair with reference-counted buffers was received.
    /// The default forwards to [`SessionHandler::on_header`].
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn on_header_rcbuf(
        &mut self,
        ctx: &mut SessionCtx<'_>,
        frame: &Frame,
        name: &RcBuf,
        value: &RcBuf,
        flags: u8,
    ) -> CallbackResult {
        self.on_header(ctx, frame, name.as_slice(), value.as_slice(), flags)
    }
    /// An invalid header was received.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn on_invalid_header(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: &Frame,
        _: &[u8],
        _: &[u8],
        _: u8,
    ) -> CallbackResult {
        Ok(())
    }
    /// Chooses the total payload length, including padding, for a frame.
    fn select_padding(&mut self, _: &mut SessionCtx<'_>, frame: &Frame, _: usize) -> nghttp2_ssize {
        // SAFETY: `hd` is the common header prefix of every union member.
        unsafe { frame.hd.length as nghttp2_ssize }
    }
    /// Chooses how many bytes a `DATA` provider may supply in one read.
    fn data_source_read_length(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: FrameType,
        _: i32,
        session_remote_window: i32,
        stream_remote_window: i32,
        remote_max_frame_size: u32,
    ) -> nghttp2_ssize {
        let allowed = session_remote_window
            .unsigned_abs()
            .min(stream_remote_window.unsigned_abs())
            .min(remote_max_frame_size);
        allowed.max(1) as nghttp2_ssize
    }
    /// A frame header was received, before the payload.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn on_begin_frame(&mut self, _: &mut SessionCtx<'_>, _: &FrameHd) -> CallbackResult {
        Ok(())
    }
    /// Writes a complete `NO_COPY` DATA frame to the transport.
    fn send_data(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: &Frame,
        _: &[u8],
        _: usize,
        _: &nghttp2_data_source,
    ) -> c_int {
        CALLBACK_FAILURE
    }
    /// Serializes an extension frame into the supplied buffer.
    /// The buffer is initialized to zero before the handler is called.
    fn pack_extension(&mut self, _: &mut SessionCtx<'_>, _: &mut [u8], _: &Frame) -> nghttp2_ssize {
        NGHTTP2_ERR_CANCEL as nghttp2_ssize
    }
    /// Reads an extension frame payload.
    fn unpack_extension(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: &mut *mut c_void,
        _: &FrameHd,
    ) -> c_int {
        NGHTTP2_ERR_CANCEL
    }
    /// A chunk of an extension frame payload was received.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn on_extension_chunk_recv(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: &FrameHd,
        _: &[u8],
    ) -> CallbackResult {
        Ok(())
    }
    /// The library reported a diagnostic message.
    /// # Errors
    /// Returns [`CallbackError::Failure`] if the notification cannot be handled.
    fn error_callback2(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: NgHttp2Error,
        _: &[u8],
    ) -> CallbackResult {
        Ok(())
    }
}
/// Handler state and dispatch guards stored in the session user data.
pub(crate) struct CallbackBox<H: SessionHandler> {
    handler: UnsafeCell<H>,
    panic: RefCell<Option<Box<dyn Any + Send>>>,
    in_callback: Cell<bool>,
    poisoned: Cell<bool>,
}
impl<H: SessionHandler> CallbackBox<H> {
    pub(crate) fn new(handler: H) -> Self {
        Self {
            handler: UnsafeCell::new(handler),
            panic: RefCell::new(None),
            in_callback: Cell::new(false),
            poisoned: Cell::new(false),
        }
    }
    /// Borrows the handler outside an active native callback.
    /// # Panics
    /// Panics if a callback currently owns the mutable handler borrow.
    pub fn handler(&self) -> &H {
        assert!(!self.in_callback.get(), "handler callback is active");
        // SAFETY: dispatch is inactive and the shared owner borrow forbids mutation.
        unsafe { &*self.handler.get() }
    }
    pub(crate) fn handler_mut(&mut self) -> &mut H {
        self.handler.get_mut()
    }
    pub(crate) fn take_panic(&mut self) -> Option<Box<dyn Any + Send>> {
        self.panic.get_mut().take()
    }
}
fn with_handler<H: SessionHandler, R>(
    user_data: *mut c_void,
    session: *mut nghttp2_session,
    failure: R,
    invoke: impl FnOnce(&mut H, &mut SessionCtx<'_>) -> R,
) -> R {
    if user_data.is_null() || session.is_null() {
        return failure;
    }
    // SAFETY: the session's user_data is the CallbackBox created for this handler,
    // and nghttp2 invokes callbacks synchronously on the session's thread.
    let callback = unsafe { &*user_data.cast::<CallbackBox<H>>() };
    if callback.poisoned.get() {
        return failure;
    }
    if callback.in_callback.replace(true) {
        return failure;
    }
    // SAFETY: the flag above guarantees exclusive access to the handler.
    let handler = unsafe { &mut *callback.handler.get() };
    // SAFETY: the session pointer is live for the duration of the callback.
    let Some(mut ctx) = (unsafe { SessionCtx::from_raw(session) }) else {
        callback.in_callback.set(false);
        return failure;
    };
    let result = catch_unwind(AssertUnwindSafe(|| invoke(handler, &mut ctx)));
    callback.in_callback.set(false);
    result.unwrap_or_else(|payload| {
        callback.poisoned.set(true);
        let previous = callback.panic.borrow_mut().replace(payload);
        // A discarded arbitrary payload can panic in Drop. Dispatch is poisoned,
        // so replacement cannot normally occur; do not run its destructor here.
        if let Some(previous) = previous {
            core::mem::forget(previous);
        }
        failure
    })
}
macro_rules! frame_notification {
    ($name:ident, $method:ident) => {
        unsafe extern "C" fn $name<H: SessionHandler>(
            session: *mut nghttp2_session,
            frame: *const RawFrame,
            user_data: *mut c_void,
        ) -> c_int {
            if frame.is_null() {
                return CALLBACK_FAILURE;
            }
            with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
                // SAFETY: nghttp2 passes a live frame for this callback.
                notification_code(handler.$method(ctx, unsafe { Frame::from_raw_ref(&*frame) }))
            })
        }
    };
}
frame_notification!(trampoline_on_frame_recv, on_frame_recv);
frame_notification!(trampoline_before_frame_send, before_frame_send);
frame_notification!(trampoline_on_frame_send, on_frame_send);
frame_notification!(trampoline_on_begin_headers, on_begin_headers);
macro_rules! callback_bytes {
    ($ptr:expr, $len:expr, $failure:expr) => {{
        let ptr = $ptr;
        let len = $len;
        if len == 0 {
            &[][..]
        } else if ptr.is_null() {
            return $failure;
        } else {
            // SAFETY: nghttp2 provides `len` readable bytes for the callback.
            unsafe { core::slice::from_raw_parts(ptr, len) }
        }
    }};
}
macro_rules! callback_bytes_mut {
    ($ptr:expr, $len:expr, $failure:expr) => {{
        let ptr = $ptr;
        let len = $len;
        if len == 0 {
            &mut [][..]
        } else if ptr.is_null() {
            return $failure;
        } else {
            // SAFETY: nghttp2 provides `len` writable bytes for the callback.
            unsafe {
                core::ptr::write_bytes(ptr, 0, len);
                core::slice::from_raw_parts_mut(ptr, len)
            }
        }
    }};
}
unsafe extern "C" fn trampoline_send<H: SessionHandler>(
    session: *mut nghttp2_session,
    data: *const u8,
    length: usize,
    flags: c_int,
    user_data: *mut c_void,
) -> nghttp2_ssize {
    with_handler::<H, _>(
        user_data,
        session,
        CALLBACK_FAILURE as nghttp2_ssize,
        |handler, ctx| {
            let data: &[u8] = callback_bytes!(data, length, CALLBACK_FAILURE as nghttp2_ssize);
            let count = handler.send(ctx, data, flags);
            if count > 0 && count as usize > length {
                CALLBACK_FAILURE as nghttp2_ssize
            } else if count == 0 && !data.is_empty() {
                NGHTTP2_ERR_WOULDBLOCK as nghttp2_ssize
            } else {
                count
            }
        },
    )
}
unsafe extern "C" fn trampoline_recv<H: SessionHandler>(
    session: *mut nghttp2_session,
    buf: *mut u8,
    length: usize,
    flags: c_int,
    user_data: *mut c_void,
) -> nghttp2_ssize {
    with_handler::<H, _>(
        user_data,
        session,
        CALLBACK_FAILURE as nghttp2_ssize,
        |handler, ctx| {
            let buf: &mut [u8] =
                callback_bytes_mut!(buf, length, CALLBACK_FAILURE as nghttp2_ssize);
            checked_output_count(handler.recv(ctx, buf, flags), length)
        },
    )
}
macro_rules! frame_error_notification {
    ($name:ident, $method:ident) => {
        unsafe extern "C" fn $name<H: SessionHandler>(
            session: *mut nghttp2_session,
            frame: *const RawFrame,
            lib_error_code: c_int,
            user_data: *mut c_void,
        ) -> c_int {
            if frame.is_null() {
                return CALLBACK_FAILURE;
            }
            with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
                notification_code(handler.$method(
                    ctx,
                    // SAFETY: nghttp2 passes a live frame for this callback.
                    unsafe { Frame::from_raw_ref(&*frame) },
                    NgHttp2Error::from_raw(lib_error_code),
                ))
            })
        }
    };
}
frame_error_notification!(trampoline_on_invalid_frame_recv, on_invalid_frame_recv);
frame_error_notification!(trampoline_on_frame_not_send, on_frame_not_send);
unsafe extern "C" fn trampoline_on_data_chunk_recv<H: SessionHandler>(
    session: *mut nghttp2_session,
    flags: u8,
    stream_id: i32,
    data: *const u8,
    length: usize,
    user_data: *mut c_void,
) -> c_int {
    with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
        let data: &[u8] = callback_bytes!(data, length, CALLBACK_FAILURE);
        notification_code(handler.on_data_chunk_recv(ctx, flags, stream_id, data))
    })
}
unsafe extern "C" fn trampoline_on_stream_close<H: SessionHandler>(
    session: *mut nghttp2_session,
    stream_id: i32,
    error_code: u32,
    user_data: *mut c_void,
) -> c_int {
    with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
        notification_code(handler.on_stream_close(ctx, stream_id, ErrorCode::from_raw(error_code)))
    })
}
unsafe extern "C" fn trampoline_on_header<H: SessionHandler>(
    session: *mut nghttp2_session,
    frame: *const RawFrame,
    name: *mut nghttp2_sys::nghttp2_rcbuf,
    value: *mut nghttp2_sys::nghttp2_rcbuf,
    flags: u8,
    user_data: *mut c_void,
) -> c_int {
    if frame.is_null() {
        return CALLBACK_FAILURE;
    }
    with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
        // SAFETY: both buffers are live for the callback and `from_raw` takes its
        // own reference, released when the wrapper drops.
        let Some(name) = (unsafe { RcBuf::from_raw(name) }) else {
            return CALLBACK_FAILURE;
        };
        // SAFETY: the value buffer has the same contract as the name buffer.
        let Some(value) = (unsafe { RcBuf::from_raw(value) }) else {
            return CALLBACK_FAILURE;
        };
        // SAFETY: nghttp2 passes a live frame for this callback.
        notification_code(handler.on_header_rcbuf(
            ctx,
            unsafe { Frame::from_raw_ref(&*frame) },
            &name,
            &value,
            flags,
        ))
    })
}
unsafe extern "C" fn trampoline_on_invalid_header<H: SessionHandler>(
    session: *mut nghttp2_session,
    frame: *const RawFrame,
    name: *mut nghttp2_sys::nghttp2_rcbuf,
    value: *mut nghttp2_sys::nghttp2_rcbuf,
    flags: u8,
    user_data: *mut c_void,
) -> c_int {
    if frame.is_null() {
        return CALLBACK_FAILURE;
    }
    with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
        // SAFETY: both buffers are live for the callback; the wrappers are dropped
        // before returning, so the library's references are untouched.
        let Some(name) = (unsafe { RcBuf::from_raw(name) }) else {
            return CALLBACK_FAILURE;
        };
        // SAFETY: the value buffer has the same contract as the name buffer.
        let Some(value) = (unsafe { RcBuf::from_raw(value) }) else {
            return CALLBACK_FAILURE;
        };
        // SAFETY: nghttp2 passes a live frame for this callback.
        notification_code(handler.on_invalid_header(
            ctx,
            unsafe { Frame::from_raw_ref(&*frame) },
            name.as_slice(),
            value.as_slice(),
            flags,
        ))
    })
}
unsafe extern "C" fn trampoline_select_padding<H: SessionHandler>(
    session: *mut nghttp2_session,
    frame: *const RawFrame,
    max_payload: usize,
    user_data: *mut c_void,
) -> nghttp2_ssize {
    if frame.is_null() {
        return CALLBACK_FAILURE as nghttp2_ssize;
    }
    // SAFETY: nghttp2 passes a live frame for this callback.
    with_handler::<H, _>(
        user_data,
        session,
        CALLBACK_FAILURE as nghttp2_ssize,
        |handler, ctx| {
            // SAFETY: nghttp2 supplies a live frame with matching payload fields.
            handler.select_padding(ctx, unsafe { Frame::from_raw_ref(&*frame) }, max_payload)
        },
    )
}
unsafe extern "C" fn trampoline_data_source_read_length<H: SessionHandler>(
    session: *mut nghttp2_session,
    frame_type: u8,
    stream_id: i32,
    session_remote_window: i32,
    stream_remote_window: i32,
    remote_max_frame_size: u32,
    user_data: *mut c_void,
) -> nghttp2_ssize {
    with_handler::<H, _>(
        user_data,
        session,
        CALLBACK_FAILURE as nghttp2_ssize,
        |handler, ctx| {
            handler.data_source_read_length(
                ctx,
                FrameType::from_raw(frame_type),
                stream_id,
                session_remote_window,
                stream_remote_window,
                remote_max_frame_size,
            )
        },
    )
}
unsafe extern "C" fn trampoline_on_begin_frame<H: SessionHandler>(
    session: *mut nghttp2_session,
    hd: *const FrameHd,
    user_data: *mut c_void,
) -> c_int {
    if hd.is_null() {
        return CALLBACK_FAILURE;
    }
    with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
        // SAFETY: nghttp2 passes a live frame header for this callback.
        notification_code(handler.on_begin_frame(ctx, unsafe { &*hd }))
    })
}
unsafe extern "C" fn trampoline_send_data<H: SessionHandler>(
    session: *mut nghttp2_session,
    frame: *mut RawFrame,
    framehd: *const u8,
    length: usize,
    source: *mut nghttp2_data_source,
    user_data: *mut c_void,
) -> c_int {
    if frame.is_null() || framehd.is_null() || source.is_null() {
        return CALLBACK_FAILURE;
    }
    with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
        // SAFETY: nghttp2 passes a live frame and data source for this call.
        let (frame, source) = unsafe { (Frame::from_raw_ref(&*frame), &*source) };
        // SAFETY: `send_data` always passes a 9-byte serialized frame header.
        let framehd = unsafe { core::slice::from_raw_parts(framehd, FRAME_HEADER_LEN) };
        handler.send_data(ctx, frame, framehd, length, source)
    })
}
unsafe extern "C" fn trampoline_pack_extension<H: SessionHandler>(
    session: *mut nghttp2_session,
    buf: *mut u8,
    length: usize,
    frame: *const RawFrame,
    user_data: *mut c_void,
) -> nghttp2_ssize {
    if frame.is_null() {
        return CALLBACK_FAILURE as nghttp2_ssize;
    }
    with_handler::<H, _>(
        user_data,
        session,
        CALLBACK_FAILURE as nghttp2_ssize,
        |handler, ctx| {
            let buf: &mut [u8] =
                callback_bytes_mut!(buf, length, CALLBACK_FAILURE as nghttp2_ssize);
            // SAFETY: nghttp2 passes a live frame for this callback.
            checked_output_count(
                handler.pack_extension(ctx, buf, unsafe { Frame::from_raw_ref(&*frame) }),
                length,
            )
        },
    )
}
unsafe extern "C" fn trampoline_unpack_extension<H: SessionHandler>(
    session: *mut nghttp2_session,
    payload: *mut *mut c_void,
    hd: *const FrameHd,
    user_data: *mut c_void,
) -> c_int {
    if payload.is_null() || hd.is_null() {
        return CALLBACK_FAILURE;
    }
    with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
        // SAFETY: nghttp2 passes writable payload and a live header for this call.
        handler.unpack_extension(ctx, unsafe { &mut *payload }, unsafe { &*hd })
    })
}
unsafe extern "C" fn trampoline_on_extension_chunk_recv<H: SessionHandler>(
    session: *mut nghttp2_session,
    hd: *const FrameHd,
    data: *const u8,
    length: usize,
    user_data: *mut c_void,
) -> c_int {
    if hd.is_null() {
        return CALLBACK_FAILURE;
    }
    with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
        let data: &[u8] = callback_bytes!(data, length, CALLBACK_FAILURE);
        // SAFETY: nghttp2 passes a live frame header for this callback.
        notification_code(handler.on_extension_chunk_recv(ctx, unsafe { &*hd }, data))
    })
}
unsafe extern "C" fn trampoline_error_callback2<H: SessionHandler>(
    session: *mut nghttp2_session,
    lib_error_code: c_int,
    message: *const c_char,
    length: usize,
    user_data: *mut c_void,
) -> c_int {
    with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
        let message: &[u8] = callback_bytes!(message.cast::<u8>(), length, CALLBACK_FAILURE);
        notification_code(handler.error_callback2(
            ctx,
            NgHttp2Error::from_raw(lib_error_code),
            message,
        ))
    })
}
unsafe extern "C" fn rand_entropy(dest: *mut u8, destlen: usize) {
    if dest.is_null() || destlen == 0 {
        return;
    }
    // SAFETY: nghttp2 provides a writable buffer of `destlen` bytes.
    let dest = unsafe { core::slice::from_raw_parts_mut(dest, destlen) };
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| fill_entropy(dest))) {
        // No payload destructor may unwind through the native randomness callback.
        core::mem::forget(payload);
        dest.fill(0);
    }
}
fn fill_entropy(dest: &mut [u8]) {
    if let Ok(mut file) = std::fs::File::open("/dev/urandom")
        && std::io::Read::read_exact(&mut file, dest).is_ok()
    {
        return;
    }
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
        ^ (dest.as_ptr() as usize as u64);
    for byte in dest.iter_mut() {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *byte = (seed >> 33) as u8;
    }
}
macro_rules! install {
    ($raw:ident, $($setter:ident => $trampoline:ident),* $(,)?) => {
        $(nghttp2_sys::$setter($raw, Some($trampoline::<H>));)*
    };
}
/// Owned native callback table with every trampoline installed for `H`.
#[derive(Debug)]
pub struct Callbacks<H: SessionHandler> {
    raw: NonNull<nghttp2_session_callbacks>,
    _handler: PhantomData<fn() -> H>,
}
impl<H: SessionHandler> Callbacks<H> {
    /// Creates a table that routes every native callback to `H`.
    ///
    /// # Errors
    ///
    /// Returns the native allocation error when the table cannot be created.
    pub fn new() -> Result<Self> {
        let mut raw = core::ptr::null_mut();
        // SAFETY: `raw` is a writable output pointer.
        let code = unsafe { nghttp2_sys::nghttp2_session_callbacks_new(&mut raw) };
        if code != 0 {
            return Err(Error::from_native(code));
        }
        let raw = NonNull::new(raw).ok_or(Error::from_native(nghttp2_sys::NGHTTP2_ERR_NOMEM))?;
        let callbacks = Self {
            raw,
            _handler: PhantomData,
        };
        // SAFETY: every setter stores a monomorphized trampoline in the live table;
        // the table is not shared until construction finishes.
        unsafe {
            let raw = callbacks.raw.as_ptr();
            install! {
                raw,
                nghttp2_session_callbacks_set_send_callback2 => trampoline_send,
                nghttp2_session_callbacks_set_recv_callback2 => trampoline_recv,
                nghttp2_session_callbacks_set_on_frame_recv_callback => trampoline_on_frame_recv,
                nghttp2_session_callbacks_set_on_invalid_frame_recv_callback => trampoline_on_invalid_frame_recv,
                nghttp2_session_callbacks_set_on_data_chunk_recv_callback => trampoline_on_data_chunk_recv,
                nghttp2_session_callbacks_set_before_frame_send_callback => trampoline_before_frame_send,
                nghttp2_session_callbacks_set_on_frame_send_callback => trampoline_on_frame_send,
                nghttp2_session_callbacks_set_on_frame_not_send_callback => trampoline_on_frame_not_send,
                nghttp2_session_callbacks_set_on_stream_close_callback => trampoline_on_stream_close,
                nghttp2_session_callbacks_set_on_begin_headers_callback => trampoline_on_begin_headers,
                nghttp2_session_callbacks_set_on_header_callback2 => trampoline_on_header,
                nghttp2_session_callbacks_set_on_invalid_header_callback2 => trampoline_on_invalid_header,
                nghttp2_session_callbacks_set_select_padding_callback2 => trampoline_select_padding,
                nghttp2_session_callbacks_set_data_source_read_length_callback2 => trampoline_data_source_read_length,
                nghttp2_session_callbacks_set_on_begin_frame_callback => trampoline_on_begin_frame,
                nghttp2_session_callbacks_set_send_data_callback => trampoline_send_data,
                nghttp2_session_callbacks_set_pack_extension_callback2 => trampoline_pack_extension,
                nghttp2_session_callbacks_set_unpack_extension_callback => trampoline_unpack_extension,
                nghttp2_session_callbacks_set_on_extension_chunk_recv_callback => trampoline_on_extension_chunk_recv,
                nghttp2_session_callbacks_set_error_callback2 => trampoline_error_callback2,
            }
            nghttp2_sys::nghttp2_session_callbacks_set_rand_callback(raw, Some(rand_entropy));
        }
        Ok(callbacks)
    }
    /// Returns the native table for session construction.
    ///
    /// The session's `user_data` must point at the box built for the same `H`.
    #[must_use]
    pub fn as_ptr(&self) -> *const nghttp2_session_callbacks {
        self.raw.as_ptr()
    }
}
impl<H: SessionHandler> Drop for Callbacks<H> {
    fn drop(&mut self) {
        // SAFETY: this value owns the only reference to the table.
        unsafe { nghttp2_sys::nghttp2_session_callbacks_del(self.raw.as_ptr()) };
    }
}
fn checked_output_count(count: nghttp2_ssize, length: usize) -> nghttp2_ssize {
    if count > 0 && count as usize > length {
        CALLBACK_FAILURE as nghttp2_ssize
    } else {
        count
    }
}
#[cfg(test)]
#[path = "../tests/support/callbacks.rs"]
mod tests;
