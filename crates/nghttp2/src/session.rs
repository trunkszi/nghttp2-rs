//! Owning, single-threaded HTTP/2 sessions and callback-safe submissions.
//!
//! Header submissions copy borrowed bytes into nghttp2. DATA sources remain owned
//! until their stream is gone; queued, not-yet-open streams retain their sources.

use std::{
    any::Any,
    cell::RefCell,
    collections::{HashMap, VecDeque},
    ffi::c_void,
    marker::PhantomData,
    ptr::{self, NonNull},
    rc::{Rc, Weak},
};

pub use ffi::nghttp2_session;
use nghttp2_sys as ffi;

use crate::{
    callbacks::{CallbackBox, Callbacks, SessionHandler},
    context::SessionCtx,
    error::{Error, Result},
    event::{EventCollector, SessionEvent},
    mem::CustomAllocator,
    nv::{HeaderField, NvFlags},
    option::SessionOptions,
    priority::ExtPri,
    stream::StreamRef,
    submit::{DataProvider, SessionSubmit, SettingsEntry},
    types::{ErrorCode, FrameFlags, FrameType, SettingsId},
};
/// Opaque native session.
pub type RawSession = nghttp2_session;
/// Client-side role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Client;
/// Server-side role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Server;
/// Client with queued owned events.
pub type ClientSession = Session<Client, EventCollector>;
/// Server with queued owned events.
pub type ServerSession = Session<Server, EventCollector>;

struct Retained {
    stream: Option<i32>,
    seen: bool,
    provider: DataProvider,
}
#[derive(Default)]
struct Sources {
    entries: Vec<Retained>,
    failure: Option<Error>,
}
type Store = Rc<RefCell<Sources>>;
thread_local! {
    static STORES: RefCell<HashMap<usize, Weak<RefCell<Sources>>>> = RefCell::new(HashMap::new());
}
fn invalid_state() -> Error {
    Error::from_native(ffi::NGHTTP2_ERR_INVALID_STATE)
}
fn sources(raw: *mut RawSession) -> Result<Store> {
    STORES
        .try_with(|registry| {
            registry
                .try_borrow()
                .map_err(|_| invalid_state())?
                .get(&(raw as usize))
                .and_then(Weak::upgrade)
                .ok_or_else(invalid_state)
        })
        .map_err(|_| invalid_state())?
}
fn live(raw: *mut RawSession) -> Result<NonNull<RawSession>> {
    let store = sources(raw)?;
    let state = store.try_borrow().map_err(|_| invalid_state())?;
    match state.failure {
        Some(error) => Err(error),
        None => NonNull::new(raw).ok_or_else(invalid_state),
    }
}
fn headers(fields: &[HeaderField<'_>]) -> Vec<ffi::nghttp2_nv> {
    fields
        .iter()
        .map(|field| {
            let mut raw = field.as_raw();
            raw.flags &= !NvFlags::NO_COPY.bits();
            raw
        })
        .collect()
}
fn retain(
    raw: *mut RawSession,
    provider: Option<DataProvider>,
) -> Result<Option<(usize, ffi::nghttp2_data_provider2)>> {
    let Some(provider) = provider else {
        return Ok(None);
    };
    // SAFETY: the owned provider's descriptor is live and contains a stable source allocation.
    let descriptor = unsafe { *provider.as_raw() };
    let store = sources(raw)?;
    let mut store = store.try_borrow_mut().map_err(|_| invalid_state())?;
    if let Some(error) = store.failure {
        return Err(error);
    }
    let slot = store.entries.len();
    store.entries.push(Retained {
        stream: None,
        seen: false,
        provider,
    });
    Ok(Some((slot, descriptor)))
}
fn bind(
    raw: *mut RawSession,
    retained: &Option<(usize, ffi::nghttp2_data_provider2)>,
    stream: Option<i32>,
) -> Result<()> {
    if let Some((slot, _)) = retained {
        let store = sources(raw)?;
        let rejected = {
            let mut store = store.try_borrow_mut().map_err(|_| invalid_state())?;
            let entry = store.entries.get_mut(*slot).ok_or_else(invalid_state)?;
            entry.stream = stream;
            if stream.is_none() {
                Some(store.entries.remove(*slot))
            } else {
                None
            }
        };
        // User source destructors run after releasing the retention-table borrow.
        drop(rejected);
    }
    Ok(())
}
fn descriptor(
    retained: &Option<(usize, ffi::nghttp2_data_provider2)>,
) -> *const ffi::nghttp2_data_provider2 {
    retained
        .as_ref()
        .map_or(ptr::null(), |(_, raw)| ptr::from_ref(raw))
}
fn classified<T>(raw: *mut RawSession, result: Result<T>) -> Result<T> {
    if let Err(error) = &result
        && error.is_fatal()
    {
        let store = sources(raw)?;
        store
            .try_borrow_mut()
            .map_err(|_| invalid_state())?
            .failure
            .get_or_insert(*error);
    }
    result
}
fn outcome(raw: *mut RawSession, code: i32) -> Result<i32> {
    classified(
        raw,
        if code < 0 {
            Err(Error::from_native(code))
        } else {
            Ok(code)
        },
    )
}

/// Owns native state, callback storage, DATA sources and an optional allocator.
///
/// Callback storage has a stable heap address. Destruction releases the native
/// session first, then its callback box and sources, and finally the allocator.
/// No session or stream view may be driven from a different thread.
pub struct Session<Role, H: SessionHandler> {
    raw: NonNull<RawSession>,
    callbacks: NonNull<CallbackBox<H>>,
    sources: Store,
    _allocator: Option<CustomAllocator>,
    panics: VecDeque<Box<dyn Any + Send>>,
    _role: PhantomData<(Role, H, Rc<()>)>,
}
impl<Role, H: SessionHandler> Drop for Session<Role, H> {
    fn drop(&mut self) {
        // SAFETY: this owner uniquely owns the live session; callbacks and sources are still live.
        unsafe { ffi::nghttp2_session_del(self.raw.as_ptr()) };
        // TLS may already be unavailable during thread teardown; it only holds weak references.
        drop(STORES.try_with(|r| {
            r.try_borrow_mut()
                .map(|mut r| r.remove(&(self.raw() as usize)))
        }));
        // SAFETY: callbacks came from Box::into_raw and native users have all been destroyed.
        unsafe { drop(Box::from_raw(self.callbacks.as_ptr())) };
    }
}
macro_rules! constructor {
    ($role:ty, $name:ident, $raw_name:ident, $native:ident) => {
        impl<H: SessionHandler> Session<$role, H> {
            /// Creates a session owning its allocator and callbacks; options are copied natively.
            /// # Errors
            /// Returns native allocation/initialization or retention-registration failure.
            pub fn $name(
                handler: H,
                options: Option<&SessionOptions>,
                allocator: Option<CustomAllocator>,
            ) -> Result<Self> {
                let mem = allocator
                    .as_ref()
                    .map_or(ptr::null_mut(), |a| ptr::from_ref(a.as_raw()).cast_mut());
                // SAFETY: the allocator is moved into the owner and outlives every native use.
                unsafe { Self::construct(handler, options, mem, allocator, ffi::$native) }
            }
            /// Creates a session borrowing an externally managed raw allocator.
            /// # Errors
            /// Returns native initialization or retention-registration failure.
            /// # Safety
            /// A non-null table and all callback userdata must remain valid, single-threaded
            /// and unwind-free until the session is destroyed; callbacks must satisfy nghttp2's
            /// allocator contract.
            pub unsafe fn $raw_name(
                handler: H,
                options: Option<&SessionOptions>,
                mem: Option<*mut ffi::nghttp2_mem>,
            ) -> Result<Self> {
                // SAFETY: the caller guarantees the external allocator's validity and lifetime.
                unsafe {
                    Self::construct(
                        handler,
                        options,
                        mem.unwrap_or(ptr::null_mut()),
                        None,
                        ffi::$native,
                    )
                }
            }
        }
        impl Session<$role, EventCollector> {
            /// Creates a session with an empty event collector.
            /// # Errors
            /// Returns initialization failure.
            pub fn new_default(
                options: Option<&SessionOptions>,
                allocator: Option<CustomAllocator>,
            ) -> Result<Self> {
                Self::$name(EventCollector::new(), options, allocator)
            }
        }
    };
}
constructor!(
    Client,
    client,
    client_with_raw_mem,
    nghttp2_session_client_new3
);
constructor!(
    Server,
    server,
    server_with_raw_mem,
    nghttp2_session_server_new3
);
type NewSession = unsafe extern "C" fn(
    *mut *mut RawSession,
    *const ffi::nghttp2_session_callbacks,
    *mut c_void,
    *const ffi::nghttp2_option,
    *mut ffi::nghttp2_mem,
) -> i32;
impl<Role, H: SessionHandler> Session<Role, H> {
    unsafe fn construct(
        handler: H,
        options: Option<&SessionOptions>,
        mem: *mut ffi::nghttp2_mem,
        allocator: Option<CustomAllocator>,
        new: NewSession,
    ) -> Result<Self> {
        let table = Callbacks::<H>::new()?;
        let callbacks = Box::into_raw(Box::new(CallbackBox::new(handler)));
        let mut raw = ptr::null_mut();
        // SAFETY: callback storage is heap-stable, options are live and caller guarantees mem.
        let code = unsafe {
            new(
                &mut raw,
                table.as_ptr(),
                callbacks.cast(),
                options.map_or(ptr::null(), SessionOptions::as_ptr),
                mem,
            )
        };
        if code < 0 || raw.is_null() {
            // SAFETY: failed native construction does not retain callback userdata.
            unsafe { drop(Box::from_raw(callbacks)) };
            return Err(Error::from_native(if code < 0 {
                code
            } else {
                ffi::NGHTTP2_ERR_NOMEM
            }));
        }
        // SAFETY: both pointers were checked above or originate from non-null Box allocation.
        let mut session = Self {
            // SAFETY: successful construction returned a non-null native session.
            raw: unsafe { NonNull::new_unchecked(raw) },
            // SAFETY: the callback pointer originates from Box::into_raw.
            callbacks: unsafe { NonNull::new_unchecked(callbacks) },
            sources: Rc::new(RefCell::new(Sources::default())),
            _allocator: allocator,
            panics: VecDeque::new(),
            _role: PhantomData,
        };
        let registered = STORES
            .try_with(|r| {
                r.try_borrow_mut()
                    .map(|mut r| r.insert(raw as usize, Rc::downgrade(&session.sources)))
                    .is_ok()
            })
            .map_err(|_| invalid_state())?;
        if !registered {
            return Err(invalid_state());
        }
        session.check_panics()?;
        Ok(session)
    }
    fn raw(&self) -> *mut RawSession {
        self.raw.as_ptr()
    }
    /// Returns the handler between native drive calls.
    pub fn handler(&self) -> &H {
        // SAFETY: a shared owner borrow prevents a simultaneous mutable drive.
        unsafe { self.callbacks.as_ref().handler() }
    }
    /// Mutably accesses the handler between native drive calls.
    pub fn handler_mut(&mut self) -> &mut H {
        // SAFETY: exclusive owner access excludes native callbacks.
        unsafe { self.callbacks.as_mut().handler_mut() }
    }
    /// Removes the oldest captured panic payload. The session stays poisoned.
    pub fn take_panic(&mut self) -> Option<Box<dyn Any + Send>> {
        self.panics.pop_front()
    }
    fn check_panics(&mut self) -> Result<()> {
        // SAFETY: this runs after native dispatch returns, without active callbacks.
        if let Some(payload) = unsafe { self.callbacks.as_mut().take_panic() } {
            self.panics.push_back(payload);
        }
        let mut store = self.sources.try_borrow_mut().map_err(|_| invalid_state())?;
        for entry in &mut store.entries {
            if let Some(payload) = entry.provider.take_panic() {
                self.panics.push_back(payload);
            }
        }
        if !self.panics.is_empty() {
            store.failure = Some(Error::CallbackPanicked);
        }
        if let Some(error) = store.failure {
            return Err(error);
        }
        // SAFETY: these native getters do not dispatch callbacks, so table borrows stay local.
        let queued = unsafe { ffi::nghttp2_session_get_outbound_queue_size(self.raw()) };
        store.entries.retain_mut(|entry| {
            let Some(stream) = entry.stream else {
                return false;
            };
            // SAFETY: the session is still live; lookup neither dispatches callbacks nor retains a
            // Rust borrow.
            let exists = unsafe { !ffi::nghttp2_session_find_stream(self.raw(), stream).is_null() };
            let keep = exists || (!entry.seen && queued != 0);
            entry.seen |= exists;
            keep
        });
        Ok(())
    }
    fn ready(&self) -> Result<()> {
        live(self.raw()).map(|_| ())
    }
    fn driven(&mut self, code: isize) -> Result<usize> {
        self.check_panics()?;
        if code < 0 {
            let error = Error::from_native(i32::try_from(code).map_err(|_| invalid_state())?);
            if error.is_fatal() {
                self.sources.borrow_mut().failure = Some(error);
            }
            Err(error)
        } else {
            Ok(code as usize)
        }
    }
    /// Parses inbound HTTP/2 bytes and returns how many bytes were consumed.
    /// # Errors
    /// Returns callback panic before native error, or a previously poisoned state.
    pub fn mem_recv(&mut self, data: &[u8]) -> Result<usize> {
        self.ready()?;
        // SAFETY: input is borrowed for this synchronous call; callback storage remains live.
        let code = unsafe { ffi::nghttp2_session_mem_recv2(self.raw(), data.as_ptr(), data.len()) };
        self.driven(code)
    }
    /// Borrows serialized bytes until the next mutable session operation.
    /// # Errors
    /// Returns callback panic or a native serialization error.
    pub fn mem_send(&mut self) -> Result<&[u8]> {
        self.ready()?;
        let mut bytes = ptr::null();
        // SAFETY: output slot is valid and native bytes remain live under the exclusive owner
        // borrow.
        let code = unsafe { ffi::nghttp2_session_mem_send2(self.raw(), &mut bytes) };
        let length = self.driven(code)?;
        if length == 0 || bytes.is_null() {
            return Ok(&[]);
        }
        // SAFETY: nghttp2 supplied length initialized bytes, valid until the next session mutation.
        Ok(unsafe { std::slice::from_raw_parts(bytes, length) })
    }
    /// Drives outbound transport through the handler's send callback.
    /// # Errors
    /// Returns callback panic or native transport error.
    pub fn send(&mut self) -> Result<()> {
        self.ready()?;
        // SAFETY: callback userdata and retained providers outlive synchronous dispatch.
        let code = unsafe { ffi::nghttp2_session_send(self.raw()) };
        self.driven(code as isize).map(|_| ())
    }
    /// Drives inbound transport through the handler's receive callback.
    /// # Errors
    /// Returns callback panic or native transport error.
    pub fn recv(&mut self) -> Result<()> {
        self.ready()?;
        // SAFETY: callback userdata stays live throughout synchronous dispatch.
        let code = unsafe { ffi::nghttp2_session_recv(self.raw()) };
        self.driven(code as isize).map(|_| ())
    }
}
impl<Role> Session<Role, EventCollector> {
    /// Removes the oldest queued session event.
    pub fn next_event(&mut self) -> Option<SessionEvent> {
        self.handler_mut().next_event()
    }
    /// Drains events in callback order.
    pub fn drain_events(&mut self) -> impl Iterator<Item = SessionEvent> + '_ {
        std::iter::from_fn(|| self.next_event())
    }
}

fn with_pointer<T>(
    raw: *mut RawSession,
    operation: impl FnOnce(&crate::context::SessionPtr<'_>) -> Result<T>,
) -> Result<T> {
    let raw = live(raw)?;
    // SAFETY: the owning session/context keeps native state live on this thread through operation.
    let handle = unsafe { crate::context::SessionPtr::from_raw(raw) };
    let result = operation(&handle);
    classified(raw.as_ptr(), result)
}
macro_rules! forward {
    ($name:ident($($param:ident: $ty:ty),*) -> $out:ty; $($arg:expr),*) => {
        #[doc = concat!("Applies `", stringify!($name), "` using native validation.")]
        /// # Errors
        /// Returns native rejection or the session's sticky fatal/panic error.
        pub fn $name(&mut self, $($param: $ty),*) -> Result<$out> {
            with_pointer(self.raw(), |session| SessionSubmit::$name(session, $($arg),*))
        }
    };
}
macro_rules! query {
    ($name:ident($($param:ident: $ty:ty),*) -> $out:ty, $native:ident; $($arg:expr),*) => {
        #[doc = concat!("Reads `", stringify!($name), "` without driving callbacks.")]
        pub fn $name(&self, $($param: $ty),*) -> $out {
            // SAFETY: this live owner/context keeps native state valid during the read-only getter.
            unsafe { ffi::$native(self.raw(), $($arg),*) }
        }
    };
}
macro_rules! session_api {
    () => {
        /// Queues request headers and retains the optional source until the stream is gone.
        /// # Errors
        /// Returns native request rejection or a poisoned/unregistered session.
        pub fn submit_request(&mut self, fields: &[HeaderField<'_>], body: Option<DataProvider>) -> Result<i32> {
            live(self.raw())?;
            let fields = headers(fields);
            let retained = retain(self.raw(), body)?;
            // SAFETY: NO_COPY flags are cleared; nghttp2 copies headers/descriptor, and the source is already retained.
            let result = outcome(self.raw(), unsafe { ffi::nghttp2_submit_request2(self.raw(), ptr::null(), fields.as_ptr(), fields.len(), descriptor(&retained), ptr::null_mut()) });
            bind(self.raw(), &retained, match result { Ok(id) => Some(id), Err(_) => None })?;
            result
        }
        /// Queues response headers, retaining the source independently of caller storage.
        /// # Errors
        /// Returns native rejection or a poisoned/unregistered session.
        pub fn submit_response(&mut self, stream_id: i32, fields: &[HeaderField<'_>], body: Option<DataProvider>) -> Result<()> {
            live(self.raw())?;
            let fields = headers(fields);
            let retained = retain(self.raw(), body)?;
            // SAFETY: header bytes are copied and the source allocation is retained before native acceptance.
            let result = outcome(self.raw(), unsafe { ffi::nghttp2_submit_response2(self.raw(), stream_id, fields.as_ptr(), fields.len(), descriptor(&retained)) });
            bind(self.raw(), &retained, result.as_ref().map(|_| stream_id).ok())?;
            result.map(|_| ())
        }
        /// Queues a header block, copying even fields marked NO_COPY by their caller.
        /// # Errors
        /// Returns native validation or allocation failure.
        pub fn submit_headers(&mut self, flags: u8, stream_id: i32, fields: &[HeaderField<'_>]) -> Result<i32> {
            live(self.raw())?;
            let fields = headers(fields);
            // SAFETY: the native call copies every header, and no borrowed bytes are retained.
            outcome(self.raw(), unsafe { ffi::nghttp2_submit_headers(self.raw(), flags, stream_id, ptr::null(), fields.as_ptr(), fields.len(), ptr::null_mut()) })
        }
        /// Queues DATA and retains its source across deferred/native callbacks.
        /// # Errors
        /// Returns native rejection or a poisoned/unregistered session.
        pub fn submit_data(&mut self, flags: u8, stream_id: i32, provider: DataProvider) -> Result<()> {
            live(self.raw())?;
            let retained = retain(self.raw(), Some(provider))?;
            // SAFETY: the source is retained and nghttp2 copies the local native descriptor.
            let result = outcome(self.raw(), unsafe { ffi::nghttp2_submit_data2(self.raw(), flags, stream_id, descriptor(&retained)) });
            bind(self.raw(), &retained, result.as_ref().map(|_| stream_id).ok())?;
            result.map(|_| ())
        }
        /// Queues copied trailing headers with END_STREAM.
        /// # Errors
        /// Returns native validation or allocation failure.
        pub fn submit_trailer(&mut self, stream_id: i32, fields: &[HeaderField<'_>]) -> Result<()> {
            live(self.raw())?;
            let fields = headers(fields);
            // SAFETY: all fields are copied by the native submission before caller buffers expire.
            outcome(self.raw(), unsafe { ffi::nghttp2_submit_trailer(self.raw(), stream_id, fields.as_ptr(), fields.len()) }).map(|_| ())
        }
        forward!(submit_rst_stream(stream_id: i32, error_code: u32) -> (); FrameFlags::NONE, stream_id, ErrorCode::from_raw(error_code));
        forward!(submit_settings(entries: &[SettingsEntry]) -> (); entries);
        forward!(submit_ping(ack: bool, opaque_data: Option<&[u8; 8]>) -> (); if ack { FrameFlags::ACK } else { FrameFlags::NONE }, opaque_data);
        forward!(submit_goaway(last_stream_id: i32, error_code: u32, opaque_data: &[u8]) -> (); FrameFlags::NONE, last_stream_id, ErrorCode::from_raw(error_code), opaque_data);
        forward!(submit_window_update(flags: u8, stream_id: i32, window_size_increment: i32) -> (); FrameFlags::from_bits(flags), stream_id, window_size_increment);
        forward!(submit_shutdown_notice() -> (); );
        forward!(submit_altsvc(stream_id: i32, origin: &[u8], field_value: &[u8]) -> (); FrameFlags::NONE, stream_id, origin, field_value);
        forward!(submit_origin(origins: &[&[u8]]) -> (); FrameFlags::NONE, origins);
        /// Queues an RFC 9218 field serialized from the validated priority.
        /// # Errors
        /// Returns native role/stream validation or allocation failure.
        pub fn submit_priority_update(&mut self, stream_id: i32, priority: ExtPri) -> Result<()> {
            let field = format!("u={}, i=?{}", priority.urgency(), u8::from(priority.incremental()));
            with_pointer(self.raw(), |s| SessionSubmit::submit_priority_update(s, FrameFlags::NONE, stream_id, field.as_bytes()))
        }
        /// Queues a caller-managed extension payload.
        /// # Errors
        /// Returns native validation failure or a poisoned session.
        /// # Safety
        /// Payload storage and nested pointers must remain valid and obey the extension callback contract
        /// until serialization/abandonment; callbacks may not access expired application storage.
        pub unsafe fn submit_extension(&mut self, frame_type: u8, flags: u8, stream_id: i32, payload: *mut c_void) -> Result<()> {
            with_pointer(self.raw(), |s| {
                // SAFETY: caller supplies and retains a valid extension payload until completion.
                unsafe { SessionSubmit::submit_extension(s, FrameType::from_raw(frame_type), FrameFlags::from_bits(flags), stream_id, payload) }
            })
        }
        forward!(change_extpri(stream_id: i32, priority: ExtPri, ignore_client_signal: bool) -> (); stream_id, priority, ignore_client_signal);
        forward!(resume_data(stream_id: i32) -> (); stream_id);
        forward!(consume(stream_id: i32, size: usize) -> (); stream_id, size);
        forward!(consume_connection(size: usize) -> (); size);
        forward!(consume_stream(stream_id: i32, size: usize) -> (); stream_id, size);
        forward!(set_local_window_size(flags: u8, stream_id: i32, window_size: i32) -> (); FrameFlags::from_bits(flags), stream_id, window_size);
        /// Borrows an active stream until this owner/context can next be mutated.
        pub fn find_stream(&self, stream_id: i32) -> Option<StreamRef<'_>> {
            // SAFETY: the session is live, and this read-only lookup cannot dispatch callbacks.
            let stream = NonNull::new(unsafe { ffi::nghttp2_session_find_stream(self.raw(), stream_id) })?;
            // SAFETY: tying the view to &self prevents native mutation through this owner/context.
            Some(unsafe { StreamRef::from_ptr(stream) })
        }
        /// Returns the native library's permanent root stream.
        pub fn root_stream(&self) -> StreamRef<'_> {
            // SAFETY: nghttp2 always returns its non-null static root for a live session.
            unsafe { StreamRef::from_ptr(NonNull::new_unchecked(ffi::nghttp2_session_get_root_stream(self.raw()))) }
        }
        /// Reports whether inbound processing remains useful.
        pub fn want_read(&self) -> bool {
            // SAFETY: this getter only reads the live native session.
            unsafe { ffi::nghttp2_session_want_read(self.raw()) != 0 }
        }
        /// Reports whether outbound work is queued.
        pub fn want_write(&self) -> bool {
            // SAFETY: this getter only reads the live native session.
            unsafe { ffi::nghttp2_session_want_write(self.raw()) != 0 }
        }
        query!(remote_window_size() -> i32, nghttp2_session_get_remote_window_size; );
        query!(local_window_size() -> i32, nghttp2_session_get_local_window_size; );
        query!(stream_remote_window_size(stream_id: i32) -> i32, nghttp2_session_get_stream_remote_window_size; stream_id);
        query!(stream_local_window_size(stream_id: i32) -> i32, nghttp2_session_get_stream_local_window_size; stream_id);
        query!(effective_recv_data_length() -> i32, nghttp2_session_get_effective_recv_data_length; );
        query!(stream_effective_recv_data_length(stream_id: i32) -> i32, nghttp2_session_get_stream_effective_recv_data_length; stream_id);
        query!(outbound_queue_size() -> usize, nghttp2_session_get_outbound_queue_size; );
        query!(next_stream_id() -> u32, nghttp2_session_get_next_stream_id; );
        query!(last_proc_stream_id() -> i32, nghttp2_session_get_last_proc_stream_id; );
        query!(remote_settings(id: SettingsId) -> u32, nghttp2_session_get_remote_settings; id.raw());
        query!(local_settings(id: SettingsId) -> u32, nghttp2_session_get_local_settings; id.raw());
    };
}
impl<Role, H: SessionHandler> Session<Role, H> {
    session_api!();
}
impl SessionCtx<'_> {
    fn raw(&self) -> *mut RawSession {
        self.as_ptr()
    }
    session_api!();
}
