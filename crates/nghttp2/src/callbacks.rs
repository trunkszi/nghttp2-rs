//! Session callbacks and panic-safe FFI trampolines: `user_data` points to a
//! `CallbackBox<H>`, each method runs inside `catch_unwind`, and a panic becomes
//! `NGHTTP2_ERR_CALLBACK_FAILURE`. Re-entering the library from a handler is
//! refused with the callback's failure code. The native `rand` callback has no
//! `user_data`.

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
    NGHTTP2_ERR_CANCEL, NGHTTP2_ERR_WOULDBLOCK, nghttp2_data_source, nghttp2_frame as Frame,
    nghttp2_frame_hd as FrameHd, nghttp2_session, nghttp2_session_callbacks, nghttp2_ssize,
};

use crate::{
    context::SessionCtx,
    error::{Error, NgHttp2Error, Result},
    rcbuf::RcBuf,
    types::{ErrorCode, FrameType},
};

/// Native callback failure code returned after a caught Rust panic.
pub const CALLBACK_FAILURE: c_int = nghttp2_sys::NGHTTP2_ERR_CALLBACK_FAILURE;

/// Size of the serialized frame header passed to `send_data`.
const FRAME_HEADER_LEN: usize = 9;

/// Handler interface for the native session callbacks.
///
/// Every method has a default; control defaults make an unimplemented callback harmless.
pub trait SessionHandler {
    /// Transmits bytes produced by the library.
    fn send(&mut self, _: &mut SessionCtx<'_>, _: &[u8], _: c_int) -> nghttp2_ssize {
        NGHTTP2_ERR_WOULDBLOCK as nghttp2_ssize
    }
    /// Receives bytes into the supplied buffer.
    ///
    /// The buffer is uninitialized; write to it before reading.
    fn recv(&mut self, _: &mut SessionCtx<'_>, _: &mut [u8], _: c_int) -> nghttp2_ssize {
        NGHTTP2_ERR_WOULDBLOCK as nghttp2_ssize
    }
    /// A complete frame was received.
    fn on_frame_recv(&mut self, _: &mut SessionCtx<'_>, _: &Frame) {}
    /// An invalid frame was received.
    fn on_invalid_frame_recv(&mut self, _: &mut SessionCtx<'_>, _: &Frame, _: NgHttp2Error) {}
    /// A chunk of `DATA` payload was received.
    fn on_data_chunk_recv(&mut self, _: &mut SessionCtx<'_>, _: u8, _: i32, _: &[u8]) {}
    /// A frame is about to be sent.
    fn before_frame_send(&mut self, _: &mut SessionCtx<'_>, _: &Frame) {}
    /// A frame was sent.
    fn on_frame_send(&mut self, _: &mut SessionCtx<'_>, _: &Frame) {}
    /// A frame could not be sent.
    fn on_frame_not_send(&mut self, _: &mut SessionCtx<'_>, _: &Frame, _: NgHttp2Error) {}
    /// A stream was closed.
    fn on_stream_close(&mut self, _: &mut SessionCtx<'_>, _: i32, _: ErrorCode) {}
    /// A header block started.
    fn on_begin_headers(&mut self, _: &mut SessionCtx<'_>, _: &Frame) {}
    /// A header name/value pair was received.
    fn on_header(&mut self, _: &mut SessionCtx<'_>, _: &Frame, _: &[u8], _: &[u8], _: u8) {}
    /// A header pair with reference-counted buffers was received.
    /// The default forwards to [`SessionHandler::on_header`].
    fn on_header_rcbuf(
        &mut self,
        ctx: &mut SessionCtx<'_>,
        frame: &Frame,
        name: &RcBuf,
        value: &RcBuf,
        flags: u8,
    ) {
        self.on_header(ctx, frame, name.as_slice(), value.as_slice(), flags);
    }
    /// An invalid header was received.
    fn on_invalid_header(&mut self, _: &mut SessionCtx<'_>, _: &Frame, _: &[u8], _: &[u8], _: u8) {}
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
    fn on_begin_frame(&mut self, _: &mut SessionCtx<'_>, _: &FrameHd) {}
    /// Writes a complete `NO_COPY` DATA frame to the transport.
    fn send_data(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: &mut Frame,
        _: &[u8],
        _: usize,
        _: &mut nghttp2_data_source,
    ) -> c_int {
        CALLBACK_FAILURE
    }
    /// Serializes an extension frame into the supplied buffer.
    ///
    /// The buffer is uninitialized; write to it before reading.
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
    fn on_extension_chunk_recv(&mut self, _: &mut SessionCtx<'_>, _: &FrameHd, _: &[u8]) {}
    /// The library reported a diagnostic message.
    fn error_callback2(&mut self, _: &mut SessionCtx<'_>, _: NgHttp2Error, _: &[u8]) {}
}

/// Handler state stored in the session's `user_data` slot.
///
/// `in_callback` is the re-entry sentinel: while it is set, no other callback may
/// touch `handler`.
pub(crate) struct CallbackBox<H: SessionHandler> {
    handler: UnsafeCell<H>,
    panic: RefCell<Option<Box<dyn Any + Send>>>,
    in_callback: Cell<bool>,
}

#[allow(dead_code)] // consumed by the session layer, not by the callback table
impl<H: SessionHandler> CallbackBox<H> {
    pub(crate) fn new(handler: H) -> Self {
        Self {
            handler: UnsafeCell::new(handler),
            panic: RefCell::new(None),
            in_callback: Cell::new(false),
        }
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
    if callback.in_callback.replace(true) {
        // Re-entering the library would alias the outer handler reference.
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
        callback.panic.borrow_mut().get_or_insert(payload);
        failure
    })
}

macro_rules! frame_notification {
    ($name:ident, $method:ident) => {
        unsafe extern "C" fn $name<H: SessionHandler>(
            session: *mut nghttp2_session,
            frame: *const Frame,
            user_data: *mut c_void,
        ) -> c_int {
            if frame.is_null() {
                return CALLBACK_FAILURE;
            }
            with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
                // SAFETY: nghttp2 passes a live frame for this callback.
                handler.$method(ctx, unsafe { &*frame });
                0
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
            unsafe { core::slice::from_raw_parts_mut(ptr, len) }
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
            handler.send(ctx, data, flags)
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
            handler.recv(ctx, buf, flags)
        },
    )
}

macro_rules! frame_error_notification {
    ($name:ident, $method:ident) => {
        unsafe extern "C" fn $name<H: SessionHandler>(
            session: *mut nghttp2_session,
            frame: *const Frame,
            lib_error_code: c_int,
            user_data: *mut c_void,
        ) -> c_int {
            if frame.is_null() {
                return CALLBACK_FAILURE;
            }
            with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
                handler.$method(
                    ctx,
                    // SAFETY: nghttp2 passes a live frame for this callback.
                    unsafe { &*frame },
                    NgHttp2Error::from_raw(lib_error_code),
                );
                0
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
        handler.on_data_chunk_recv(ctx, flags, stream_id, data);
        0
    })
}

unsafe extern "C" fn trampoline_on_stream_close<H: SessionHandler>(
    session: *mut nghttp2_session,
    stream_id: i32,
    error_code: u32,
    user_data: *mut c_void,
) -> c_int {
    with_handler::<H, _>(user_data, session, CALLBACK_FAILURE, |handler, ctx| {
        handler.on_stream_close(ctx, stream_id, ErrorCode::from_raw(error_code));
        0
    })
}

unsafe extern "C" fn trampoline_on_header<H: SessionHandler>(
    session: *mut nghttp2_session,
    frame: *const Frame,
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
        handler.on_header_rcbuf(ctx, unsafe { &*frame }, &name, &value, flags);
        0
    })
}

unsafe extern "C" fn trampoline_on_invalid_header<H: SessionHandler>(
    session: *mut nghttp2_session,
    frame: *const Frame,
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
        handler.on_invalid_header(
            ctx,
            unsafe { &*frame },
            name.as_slice(),
            value.as_slice(),
            flags,
        );
        0
    })
}

unsafe extern "C" fn trampoline_select_padding<H: SessionHandler>(
    session: *mut nghttp2_session,
    frame: *const Frame,
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
        |handler, ctx| handler.select_padding(ctx, unsafe { &*frame }, max_payload),
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
        handler.on_begin_frame(ctx, unsafe { &*hd });
        0
    })
}

unsafe extern "C" fn trampoline_send_data<H: SessionHandler>(
    session: *mut nghttp2_session,
    frame: *mut Frame,
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
        let (frame, source) = unsafe { (&mut *frame, &mut *source) };
        // SAFETY: `send_data` always passes a 9-byte serialized frame header.
        let framehd = unsafe { core::slice::from_raw_parts(framehd, FRAME_HEADER_LEN) };
        handler.send_data(ctx, frame, framehd, length, source)
    })
}

unsafe extern "C" fn trampoline_pack_extension<H: SessionHandler>(
    session: *mut nghttp2_session,
    buf: *mut u8,
    length: usize,
    frame: *const Frame,
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
            handler.pack_extension(ctx, buf, unsafe { &*frame })
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
        handler.on_extension_chunk_recv(ctx, unsafe { &*hd }, data);
        0
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
        handler.error_callback2(ctx, NgHttp2Error::from_raw(lib_error_code), message);
        0
    })
}

unsafe extern "C" fn rand_entropy(dest: *mut u8, destlen: usize) {
    if dest.is_null() || destlen == 0 {
        return;
    }
    // SAFETY: nghttp2 provides a writable buffer of `destlen` bytes.
    let dest = unsafe { core::slice::from_raw_parts_mut(dest, destlen) };
    if catch_unwind(AssertUnwindSafe(|| fill_entropy(dest))).is_err() {
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHandler;

    impl SessionHandler for TestHandler {}

    fn test_box<H: SessionHandler>(handler: H) -> (Box<CallbackBox<H>>, *mut c_void) {
        let mut boxed = Box::new(CallbackBox::new(handler));
        let user_data = (&mut *boxed as *mut CallbackBox<H>).cast::<c_void>();
        (boxed, user_data)
    }

    fn frame(type_: u8, stream_id: i32) -> Frame {
        Frame {
            hd: FrameHd {
                length: 5,
                stream_id,
                type_,
                flags: 0,
                reserved: 0,
            },
        }
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
        fn on_frame_recv(&mut self, _: &mut SessionCtx<'_>, _: &Frame) {
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
        let outer =
            with_handler::<TestHandler, c_int>(user_data, session, CALLBACK_FAILURE, |_, _| {
                let inner = with_handler::<TestHandler, c_int>(
                    user_data,
                    session,
                    CALLBACK_FAILURE,
                    |_, _| 7,
                );
                assert_eq!(inner, CALLBACK_FAILURE);
                42
            });
        assert_eq!(outer, 42);
        // The guard must be clear again after the outer call returns.
        let after =
            with_handler::<TestHandler, c_int>(user_data, session, CALLBACK_FAILURE, |_, _| 9);
        assert_eq!(after, 9);
    }

    #[test]
    fn null_pointers_return_callback_failure() {
        let (_boxed, user_data) = test_box(TestHandler);
        let session = NonNull::<nghttp2_session>::dangling().as_ptr();
        // SAFETY: each trampoline checks its null argument before dereferencing.
        let code = unsafe {
            trampoline_on_frame_recv::<TestHandler>(session, core::ptr::null(), user_data)
        };
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
        let mut data_frame = frame(0, 1);
        let mut source = nghttp2_sys::nghttp2_data_source { fd: 0 };
        let framehd = [0_u8; FRAME_HEADER_LEN];

        assert_eq!(
            handler.send(&mut ctx, b"x", 0),
            NGHTTP2_ERR_WOULDBLOCK as nghttp2_ssize
        );
        assert_eq!(handler.select_padding(&mut ctx, &data_frame, 100), 5);
        let read = handler.data_source_read_length(&mut ctx, FrameType::DATA, 1, 10, 20, 30);
        assert_eq!(read, 10);
        let data = handler.send_data(&mut ctx, &mut data_frame, &framehd, 0, &mut source);
        assert_eq!(data, CALLBACK_FAILURE);
        let packed = handler.pack_extension(&mut ctx, &mut [], &data_frame);
        assert_eq!(packed, NGHTTP2_ERR_CANCEL as nghttp2_ssize);
    }
}
