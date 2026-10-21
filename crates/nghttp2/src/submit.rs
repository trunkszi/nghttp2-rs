//! Outbound frame construction: submission validates arguments and queues frames
//! without blocking; DATA comes from a buffer or a [`DataSource`], which can defer.

use core::{
    any::Any,
    ffi::c_void,
    panic::AssertUnwindSafe,
    ptr::{self, NonNull},
};

use nghttp2_sys::{
    NGHTTP2_DATA_FLAG_EOF, NGHTTP2_DATA_FLAG_NO_COPY, NGHTTP2_DATA_FLAG_NO_END_STREAM,
    NGHTTP2_ERR_CALLBACK_FAILURE, nghttp2_nv, nghttp2_origin_entry, nghttp2_priority_spec,
    nghttp2_priority_spec_default_init, nghttp2_session,
    nghttp2_session_change_extpri_stream_priority, nghttp2_session_consume,
    nghttp2_session_consume_connection, nghttp2_session_consume_stream,
    nghttp2_session_get_extpri_stream_priority, nghttp2_session_resume_data,
    nghttp2_session_set_local_window_size, nghttp2_settings_entry, nghttp2_ssize,
    nghttp2_submit_altsvc, nghttp2_submit_data2, nghttp2_submit_extension, nghttp2_submit_goaway,
    nghttp2_submit_headers, nghttp2_submit_origin, nghttp2_submit_ping,
    nghttp2_submit_priority_update, nghttp2_submit_request2, nghttp2_submit_response2,
    nghttp2_submit_rst_stream, nghttp2_submit_settings, nghttp2_submit_shutdown_notice,
    nghttp2_submit_trailer, nghttp2_submit_window_update,
};
pub use nghttp2_sys::{nghttp2_data_provider2, nghttp2_data_source};

use crate::{
    context::{SessionCtx, SessionPtr},
    error::{Error, Result},
    nv::HeaderField,
    priority::ExtPri,
    types::{ErrorCode, FrameFlags, FrameType, SettingsId, SettingsValue},
};

/// Native streaming DATA provider.
pub type RawDataProvider = nghttp2_data_provider2;

/// Native source handed to a DATA provider callback.
pub type RawDataSource = nghttp2_data_source;

/// Provider callback result that postpones transmission.
pub const DEFERRED: i32 = nghttp2_sys::NGHTTP2_ERR_DEFERRED;

/// One poll of a [`DataSource`].
pub enum DataPoll<'a> {
    /// Copy these bytes into the library's frame buffer; excess is served next poll.
    Copy(&'a [u8]),
    /// Send these bytes through the session's `send_data` callback without copying; that callback
    /// must be installed and the bytes must stay valid and unmodified until sent.
    NoCopy(&'a [u8]),
    /// End of stream: no more bytes will follow.
    Eof,
    /// Finish this body while keeping the stream open for trailing HEADERS.
    EofWithTrailers,
    /// Nothing to send now; [`SessionSubmit::resume_data`] restarts the stream.
    Deferred,
}

/// Source of DATA frame payloads.
pub trait DataSource {
    /// Produces the next chunk; nghttp2 consumes at most `max_len` bytes per poll. An
    /// empty chunk without `Eof` keeps the stream open, so an idle source must defer.
    fn read(&mut self, max_len: usize) -> DataPoll<'_>;
}

/// Bytes held back when a source produced more than the current poll limit.
#[derive(Clone, Copy)]
struct Pending {
    ptr: *const u8,
    len: usize,
    no_copy: bool,
}

#[rustfmt::skip]
impl Pending {
    const EMPTY: Self = Self { ptr: ptr::null(), len: 0, no_copy: false };
    fn new(ptr: *const u8, len: usize, no_copy: bool) -> Self { Self { ptr, len, no_copy } }
}

/// State behind [`DataProvider`]; boxed so its address never changes.
struct ProviderState {
    source: NonNull<dyn DataSource>,
    pending: Pending,
    no_copy: Option<(*const u8, usize)>,
    panic: Option<Box<dyn Any + Send>>,
}

impl Drop for ProviderState {
    fn drop(&mut self) {
        // SAFETY: this pointer owns the source allocation returned by Box::into_raw.
        drop(unsafe { Box::from_raw(self.source.as_ptr()) });
    }
}

/// DATA provider backed by a [`DataSource`]. The state lives in a raw box allocation,
/// so `source.ptr` stays valid across moves and field access.
pub struct DataProvider {
    state: NonNull<ProviderState>,
    raw: RawDataProvider,
}

#[rustfmt::skip]
impl DataProvider {
    /// Wraps a caller-supplied source.
    pub fn new<S: DataSource + 'static>(source: S) -> Self {
        let source: Box<dyn DataSource> = Box::new(source);
        let allocation = Box::into_raw(source);
        // SAFETY: converting the owned box yields a non-null pointer; ProviderState
        // reconstructs the box on drop without retagging pending source bytes.
        let source = unsafe { NonNull::new_unchecked(allocation) };
        let allocation = Box::into_raw(Box::new(ProviderState { source, pending: Pending::EMPTY, no_copy: None, panic: None }));
        // SAFETY: `Box::into_raw` returns a non-null owned allocation.
        let state = unsafe { NonNull::new_unchecked(allocation) };
        let raw = RawDataProvider { source: RawDataSource { ptr: state.as_ptr().cast() }, read_callback: Some(provider_read) };
        Self { state, raw }
    }
    /// Defers its first poll, then ends an empty body after `resume_data`.
    pub fn deferred() -> Self { Self::new(DeferredSource { waiting: true }) }
    /// Builds a provider that copies `data` into the library buffer.
    pub fn from_vec(data: Vec<u8>) -> Self { Self::new(VecSource { data, pos: 0, no_copy: false }) }
    /// Builds a provider that sends `data` through the session's `send_data` callback.
    pub fn from_vec_no_copy(data: Vec<u8>) -> Self { Self::new(VecSource { data, pos: 0, no_copy: true }) }
    /// Returns the descriptor nghttp2 copies into its queue; valid while `self` is alive, because `source.ptr` points into the stable heap state.
    pub fn as_raw(&self) -> *const RawDataProvider { ptr::from_ref(&self.raw) }
    /// Removes the captured panic payload; a payload poisons the provider, and re-submitting it is unsafe.
    pub fn take_panic(&mut self) -> Option<Box<dyn Any + Send>> {
        // SAFETY: `self` owns the allocation and no callback runs at the same time.
        unsafe { (*self.state.as_ptr()).panic.take() }
    }
}

impl Drop for DataProvider {
    fn drop(&mut self) {
        // SAFETY: `state` came from `Box::into_raw` and is freed exactly once here.
        drop(unsafe { Box::from_raw(self.state.as_ptr()) });
    }
}

/// Recovers the chunk produced by the last [`DataPoll::NoCopy`] poll; `None` if empty.
///
/// # Safety
///
/// `source` must be the pointer nghttp2 passes to a `send_data` callback for a [`DataProvider`], during the matching call; the bytes expire when it returns.
#[rustfmt::skip]
pub unsafe fn no_copy_chunk(source: *mut RawDataSource) -> Option<(*const u8, usize)> {
    if source.is_null() { return None; }
    // SAFETY: the contract above guarantees a live data source of a provider built by
    // this module, whose `ptr` variant is the one written here.
    let state = unsafe { (*source).ptr.cast::<ProviderState>() };
    // SAFETY: the provider owns this box for the whole exchange and no read callback
    // runs at the same time as the send callback.
    let state = unsafe { state.as_ref() }?;
    let (ptr, len) = state.no_copy?;
    if len > 0 { Some((ptr, len)) } else { None }
}

struct DeferredSource {
    waiting: bool,
}
impl DataSource for DeferredSource {
    fn read(&mut self, _max_len: usize) -> DataPoll<'_> {
        if core::mem::replace(&mut self.waiting, false) {
            DataPoll::Deferred
        } else {
            DataPoll::Eof
        }
    }
}

/// Source used by [`DataProvider::from_vec`] and [`DataProvider::from_vec_no_copy`].
struct VecSource {
    data: Vec<u8>,
    pos: usize,
    no_copy: bool,
}

#[rustfmt::skip]
impl DataSource for VecSource {
    fn read(&mut self, _max_len: usize) -> DataPoll<'_> {
        if self.pos >= self.data.len() { return DataPoll::Eof; }
        let chunk = &self.data[self.pos..];
        self.pos = self.data.len();
        if self.no_copy { DataPoll::NoCopy(chunk) } else { DataPoll::Copy(chunk) }
    }
}

/// Trampoline installed on every [`DataProvider`].
///
/// # Safety
///
/// `source` must point at the data source of a live provider built by this module, `buf` must be writable for `len` bytes, and `flags` must be writable.
#[rustfmt::skip]
unsafe extern "C" fn provider_read(_session: *mut nghttp2_session, _stream_id: i32, buf: *mut u8,
                                   len: usize, flags: *mut u32, source: *mut RawDataSource,
                                   _user_data: *mut c_void) -> nghttp2_ssize {
    // SAFETY: the caller guarantees a live provider source for the variant read here.
    let state = unsafe { (*source).ptr.cast::<ProviderState>() };
    // SAFETY: `state` points into the provider's stable box and is never polled twice
    // concurrently.
    let Some(state) = (unsafe { state.as_mut() }) else {
        return NGHTTP2_ERR_CALLBACK_FAILURE as nghttp2_ssize;
    };
    if state.panic.is_some() { return NGHTTP2_ERR_CALLBACK_FAILURE as nghttp2_ssize; }
    if state.pending.len > 0 { return serve_pending(state, buf, len, flags); }
    // Owned return value: the borrow of `state.source` must not escape `catch_unwind`.
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the source allocation is owned by this state. Pending chunks were
        // drained above, so no retained source borrow overlaps another read.
        let source = unsafe { state.source.as_mut() };
        match source.read(len) {
        DataPoll::Copy(chunk) => {
            let take = chunk.len().min(len);
            // SAFETY: `buf` is writable for `len` bytes and `chunk` holds `take`.
            unsafe { ptr::copy_nonoverlapping(chunk.as_ptr(), buf, take) };
            if take < chunk.len() {
                // SAFETY: the source is not polled again until this remainder is drained.
                let rest = unsafe { chunk.as_ptr().add(take) };
                state.pending = Pending::new(rest, chunk.len() - take, false);
            }
            take as nghttp2_ssize
        }
        DataPoll::NoCopy(chunk) => {
            let take = chunk.len().min(len);
            state.no_copy = Some((chunk.as_ptr(), take));
            // SAFETY: `flags` is the writable out-parameter of this callback.
            unsafe { *flags |= NGHTTP2_DATA_FLAG_NO_COPY };
            if take < chunk.len() {
                // SAFETY: same remainder rule as the `Copy` arm.
                let rest = unsafe { chunk.as_ptr().add(take) };
                state.pending = Pending::new(rest, chunk.len() - take, true);
            }
            take as nghttp2_ssize
        }
        DataPoll::Eof => {
            // SAFETY: `flags` is the writable out-parameter of this callback.
            unsafe { *flags |= NGHTTP2_DATA_FLAG_EOF };
            0
        }
        DataPoll::EofWithTrailers => {
            // SAFETY: `flags` is the writable out-parameter of this callback.
            unsafe { *flags |= NGHTTP2_DATA_FLAG_EOF | NGHTTP2_DATA_FLAG_NO_END_STREAM };
            0
        }
        DataPoll::Deferred => DEFERRED as nghttp2_ssize,
        }
    }));
    match result {
        Ok(value) => value,
        Err(payload) => {
            state.panic = Some(payload);
            NGHTTP2_ERR_CALLBACK_FAILURE as nghttp2_ssize
        }
    }
}

/// Serves a retained remainder before the source is polled again.
#[rustfmt::skip]
fn serve_pending(state: &mut ProviderState, buf: *mut u8, len: usize, flags: *mut u32) -> isize {
    let take = state.pending.len.min(len);
    if state.pending.no_copy {
        state.no_copy = Some((state.pending.ptr, take));
        // SAFETY: `flags` is the writable out-parameter of the read callback.
        unsafe { *flags |= NGHTTP2_DATA_FLAG_NO_COPY };
    } else {
        // SAFETY: pending bytes stay readable until the source is polled again; `take <= len`.
        unsafe { ptr::copy_nonoverlapping(state.pending.ptr, buf, take) };
    }
    // SAFETY: the consumed bytes lie inside the pending allocation.
    state.pending.ptr = unsafe { state.pending.ptr.add(take) };
    state.pending.len -= take;
    if state.pending.len == 0 { state.pending.ptr = ptr::null(); }
    take as isize
}

/// RFC 7540 priority dependency, weight, and exclusivity, ignored by upstream HEADERS.
pub struct PrioritySpec(nghttp2_priority_spec);

#[rustfmt::skip]
impl PrioritySpec {
    /// Builds a dependency on `dependency` with `weight` (1..=256).
    pub fn new(dependency: i32, weight: i32, exclusive: bool) -> Self {
        Self(nghttp2_priority_spec { stream_id: dependency, weight, exclusive: u8::from(exclusive) })
    }
    /// Builds the default entry pointing at `dependency`: weight 16, not exclusive.
    pub fn default_for(dependency: i32) -> Self {
        let mut raw = nghttp2_priority_spec { stream_id: 0, weight: 0, exclusive: 0 };
        // SAFETY: `raw` is a writable local and the initializer assigns every field.
        unsafe { nghttp2_priority_spec_default_init(&raw mut raw) };
        raw.stream_id = dependency;
        Self(raw)
    }
    /// Returns the stream this entry depends on.
    pub fn dependency(&self) -> i32 { self.0.stream_id }
    /// Returns the weight, where higher values receive more bandwidth.
    pub fn weight(&self) -> i32 { self.0.weight }
    /// Returns whether the dependency replaces the current children.
    pub fn exclusive(&self) -> bool { self.0.exclusive != 0 }
    /// Returns a pointer to the native descriptor, valid while `self` is alive.
    pub fn as_raw(&self) -> *const nghttp2_priority_spec { ptr::from_ref(&self.0) }
}

/// One `SETTINGS` identifier/value pair.
pub struct SettingsEntry {
    id: SettingsId,
    value: SettingsValue,
}

#[rustfmt::skip]
impl SettingsEntry {
    /// Builds an entry; nghttp2 checks the value range at submission time.
    pub fn new(id: SettingsId, value: SettingsValue) -> Self { Self { id, value } }
    /// Returns the settings identifier.
    pub fn id(&self) -> SettingsId { self.id }
    /// Returns the settings value.
    pub fn value(&self) -> SettingsValue { self.value }
    /// Returns the native descriptor copied by the submission call.
    pub fn as_raw(&self) -> nghttp2_settings_entry { nghttp2_settings_entry { settings_id: self.id.raw() as i32, value: self.value } }
}

/// Frame submission on a live session handle. Frames that cannot be sent are
/// abandoned through `on_frame_not_send`.
#[rustfmt::skip]
pub trait SessionSubmit {
    /// Submits client request HEADERS and returns the new id; a provider suppresses `END_STREAM`.
    /// # Errors
    /// `NGHTTP2_ERR_PROTO` on a server, or `NGHTTP2_ERR_NOMEM`/no id available.
    /// # Safety
    /// nghttp2 retains `source.ptr`: keep the provider alive and unmodified until the stream closes or the frame is reported not sent; `NO_COPY` bytes must stay valid until sent.
    unsafe fn submit_request(&self, pri: Option<&PrioritySpec>, nva: &[HeaderField<'_>], data: Option<&mut DataProvider>, stream_user_data: *mut c_void) -> Result<i32>;
    /// Submits response HEADERS for `stream_id`.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_ARGUMENT` for a non-positive id, `NGHTTP2_ERR_PROTO` on a client, or `NGHTTP2_ERR_NOMEM`.
    /// # Safety
    /// Same provider and header-buffer rules as [`Self::submit_request`].
    unsafe fn submit_response(&self, stream_id: i32, nva: &[HeaderField<'_>], data: Option<&mut DataProvider>) -> Result<()>;
    /// Submits HEADERS, or a new request when `stream_id` is `-1` (returns the new id); the priority descriptor is ignored upstream.
    /// # Errors
    /// `NGHTTP2_ERR_PROTO` for `-1` on a server, `NGHTTP2_ERR_INVALID_ARGUMENT` for other ids, or `NGHTTP2_ERR_NOMEM`.
    /// # Safety
    /// `NO_COPY` header bytes must stay valid and unmodified until the frame is sent or abandoned; `stream_user_data` is only stored.
    unsafe fn submit_headers(&self, flags: FrameFlags, stream_id: i32, pri: Option<&PrioritySpec>, nva: &[HeaderField<'_>], stream_user_data: *mut c_void) -> Result<i32>;
    /// Submits an additional DATA frame on an existing stream.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_ARGUMENT` for stream 0, `NGHTTP2_ERR_STREAM_CLOSED` when gone, or `NGHTTP2_ERR_DATA_EXIST`.
    /// # Safety
    /// The provider is retained until the frame is sent, so keep it alive and unmodified until the stream closes or the frame is reported not sent.
    unsafe fn submit_data(&self, flags: FrameFlags, stream_id: i32, data: &mut DataProvider) -> Result<()>;
    /// Submits a trailing HEADERS frame carrying `END_STREAM`.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_ARGUMENT` for a non-positive id or `NGHTTP2_ERR_NOMEM`.
    /// # Safety
    /// Header buffer lifetime follows [`Self::submit_headers`].
    unsafe fn submit_trailer(&self, stream_id: i32, nva: &[HeaderField<'_>]) -> Result<()>;
    /// Submits `RST_STREAM`; idle or closed streams are ignored but still succeed.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_ARGUMENT` for stream 0.
    fn submit_rst_stream(&self, flags: FrameFlags, stream_id: i32, error_code: ErrorCode) -> Result<()>;
    /// Submits a non-ACK `SETTINGS` frame; the library answers peer settings itself.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_ARGUMENT` for a value outside its range or a conflicting `NO_RFC7540_PRIORITIES`.
    fn submit_settings(&self, entries: &[SettingsEntry]) -> Result<()>;
    /// Submits `PING`; `opaque` contributes the 8 payload bytes and may be `None`.
    /// # Errors
    /// `NGHTTP2_ERR_FLOODED` when too many `PING` acknowledgements are queued.
    fn submit_ping(&self, flags: FrameFlags, opaque: Option<&[u8; 8]>) -> Result<()>;
    /// Submits `GOAWAY`.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_ARGUMENT` when `last_stream_id` is ours or the opaque data is too long.
    fn submit_goaway(&self, flags: FrameFlags, last_stream_id: i32, error_code: ErrorCode, opaque: &[u8]) -> Result<()>;
    /// Submits `WINDOW_UPDATE`; stream 0 updates the connection window and a zero increment queues nothing.
    /// # Errors
    /// `NGHTTP2_ERR_FLOW_CONTROL` when the window would exceed its limit.
    fn submit_window_update(&self, flags: FrameFlags, stream_id: i32, increment: i32) -> Result<()>;
    /// Queues the server's pre-shutdown `GOAWAY` notice.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_STATE` on a client session.
    fn submit_shutdown_notice(&self) -> Result<()>;
    /// Submits an `ALTSVC` extension frame.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_STATE` on a client, `NGHTTP2_ERR_INVALID_ARGUMENT` for a bad origin or oversized payload, or `NGHTTP2_ERR_NOMEM`.
    fn submit_altsvc(&self, flags: FrameFlags, stream_id: i32, origin: &[u8], field_value: &[u8]) -> Result<()>;
    /// Submits an `ORIGIN` extension frame.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_STATE` on a client, `NGHTTP2_ERR_INVALID_ARGUMENT` for an oversized payload, or `NGHTTP2_ERR_NOMEM`.
    fn submit_origin(&self, flags: FrameFlags, origins: &[&[u8]]) -> Result<()>;
    /// Submits a `PRIORITY_UPDATE` frame; upstream queues nothing until the peer advertises `SETTINGS_NO_RFC7540_PRIORITIES` value 1.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_STATE` on a server; `NGHTTP2_ERR_INVALID_ARGUMENT` for stream 0 or an oversized payload once enabled.
    fn submit_priority_update(&self, flags: FrameFlags, stream_id: i32, field_value: &[u8]) -> Result<()>;
    /// Submits an extension frame; `type_` must be greater than 9 (`CONTINUATION`).
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_ARGUMENT` for a reserved type; `NGHTTP2_ERR_INVALID_STATE` without a `pack_extension` callback.
    /// # Safety
    /// `payload` must stay valid and unmodified until the frame is sent or abandoned.
    unsafe fn submit_extension(&self, type_: FrameType, flags: FrameFlags, stream_id: i32, payload: *mut c_void) -> Result<()>;
    /// Changes a stream's RFC 9218 priority; a server only acts when `SETTINGS_NO_RFC7540_PRIORITIES` is pending as 1.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_STATE` on a client; `NGHTTP2_ERR_INVALID_ARGUMENT` for stream 0 or an unknown stream once enabled.
    fn change_extpri(&self, stream_id: i32, extpri: ExtPri, ignore_client_signal: bool) -> Result<()>;
    /// Reads a stream's RFC 9218 priority; it stays at [`ExtPri::default`] while RFC 9218 priorities are not enabled.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_STATE` on a client; `NGHTTP2_ERR_INVALID_ARGUMENT` for stream 0, an unknown stream, or a bad result.
    fn extpri(&self, stream_id: i32) -> Result<ExtPri>;
    /// Marks `size` received bytes as consumed on the connection and the stream.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_ARGUMENT` for stream 0; `NGHTTP2_ERR_INVALID_STATE` without no-auto-window-update.
    fn consume(&self, stream_id: i32, size: usize) -> Result<()>;
    /// Marks `size` received bytes as consumed on the connection only.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_STATE` unless automatic window updates are disabled.
    fn consume_connection(&self, size: usize) -> Result<()>;
    /// Marks `size` received bytes as consumed on one stream only.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_ARGUMENT` for stream 0 and `NGHTTP2_ERR_INVALID_STATE` unless automatic window updates are disabled.
    fn consume_stream(&self, stream_id: i32, size: usize) -> Result<()>;
    /// Sets the local flow-control window, emitting `WINDOW_UPDATE` when it grows.
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_ARGUMENT` for a negative window; `NGHTTP2_ERR_FLOW_CONTROL` if it cannot be adjusted.
    fn set_local_window_size(&self, flags: FrameFlags, stream_id: i32, window_size: i32) -> Result<()>;
    /// Resumes a stream whose provider returned [`DataPoll::Deferred`].
    /// # Errors
    /// `NGHTTP2_ERR_INVALID_ARGUMENT` when the stream is missing or not deferred.
    fn resume_data(&self, stream_id: i32) -> Result<()>;
}

#[rustfmt::skip]
fn raw_headers(nva: &[HeaderField<'_>]) -> Vec<nghttp2_nv> { nva.iter().map(HeaderField::as_raw).collect() }

#[rustfmt::skip]
fn raw_priority(pri: Option<&PrioritySpec>) -> *const nghttp2_priority_spec { pri.map_or(ptr::null(), PrioritySpec::as_raw) }

#[rustfmt::skip]
fn status(code: i32) -> Result<()> { if code < 0 { Err(Error::from_native(code)) } else { Ok(()) } }

#[rustfmt::skip]
fn id_result(code: i32) -> Result<i32> { if code < 0 { Err(Error::from_native(code)) } else { Ok(code) } }

#[rustfmt::skip]
impl SessionSubmit for SessionPtr<'_> {
    unsafe fn submit_request(&self, pri: Option<&PrioritySpec>, nva: &[HeaderField<'_>], data: Option<&mut DataProvider>, user_data: *mut c_void) -> Result<i32> {
        let raw = raw_headers(nva);
        let provider = data.map_or(ptr::null(), |provider| provider.as_raw());
        // SAFETY: the session is live and the descriptor array outlives the call; nghttp2
        // copies the descriptors but retains `NO_COPY` buffers and `source.ptr`.
        id_result(unsafe { nghttp2_submit_request2(self.as_ptr(), raw_priority(pri), raw.as_ptr(), raw.len(), provider, user_data) })
    }
    unsafe fn submit_response(&self, stream_id: i32, nva: &[HeaderField<'_>], data: Option<&mut DataProvider>) -> Result<()> {
        let raw = raw_headers(nva);
        let provider = data.map_or(ptr::null(), |provider| provider.as_raw());
        // SAFETY: the session is live; NO_COPY buffers and the provider follow the trait contract.
        status(unsafe { nghttp2_submit_response2(self.as_ptr(), stream_id, raw.as_ptr(), raw.len(), provider) })
    }
    unsafe fn submit_headers(&self, flags: FrameFlags, stream_id: i32, pri: Option<&PrioritySpec>, nva: &[HeaderField<'_>], user_data: *mut c_void) -> Result<i32> {
        let raw = raw_headers(nva);
        // SAFETY: the session is live and the descriptor array outlives the call;
        // `NO_COPY` buffers and the priority descriptor follow the trait contract.
        id_result(unsafe { nghttp2_submit_headers(self.as_ptr(), flags.bits(), stream_id, raw_priority(pri), raw.as_ptr(), raw.len(), user_data) })
    }
    unsafe fn submit_data(&self, flags: FrameFlags, stream_id: i32, data: &mut DataProvider) -> Result<()> {
        // SAFETY: the session is live and nghttp2 retains `source.ptr` until the DATA
        // item is sent.
        status(unsafe { nghttp2_submit_data2(self.as_ptr(), flags.bits(), stream_id, data.as_raw()) })
    }
    unsafe fn submit_trailer(&self, stream_id: i32, nva: &[HeaderField<'_>]) -> Result<()> {
        let raw = raw_headers(nva);
        // SAFETY: the session is live; `NO_COPY` buffers are retained until sent.
        status(unsafe { nghttp2_submit_trailer(self.as_ptr(), stream_id, raw.as_ptr(), raw.len()) })
    }
    fn submit_rst_stream(&self, flags: FrameFlags, stream_id: i32, error_code: ErrorCode) -> Result<()> {
        // SAFETY: the session is live and the call borrows no other memory.
        status(unsafe { nghttp2_submit_rst_stream(self.as_ptr(), flags.bits(), stream_id, error_code.raw()) })
    }
    fn submit_settings(&self, entries: &[SettingsEntry]) -> Result<()> {
        let raw: Vec<nghttp2_settings_entry> = entries.iter().map(SettingsEntry::as_raw).collect();
        // SAFETY: the session is live; upstream discards the C flags and copies the entries.
        status(unsafe { nghttp2_submit_settings(self.as_ptr(), FrameFlags::NONE.bits(), raw.as_ptr(), raw.len()) })
    }
    fn submit_ping(&self, flags: FrameFlags, opaque: Option<&[u8; 8]>) -> Result<()> {
        let opaque = opaque.map_or(ptr::null(), |bytes| bytes.as_ptr());
        // SAFETY: the session is live; `opaque` is null or eight readable bytes that
        // nghttp2 copies into the frame.
        status(unsafe { nghttp2_submit_ping(self.as_ptr(), flags.bits(), opaque) })
    }
    fn submit_goaway(&self, flags: FrameFlags, last_stream_id: i32, error_code: ErrorCode, opaque: &[u8]) -> Result<()> {
        // SAFETY: the session is live and nghttp2 copies `opaque` during the call.
        status(unsafe { nghttp2_submit_goaway(self.as_ptr(), flags.bits(), last_stream_id, error_code.raw(), opaque.as_ptr(), opaque.len()) })
    }
    fn submit_window_update(&self, flags: FrameFlags, stream_id: i32, increment: i32) -> Result<()> {
        // SAFETY: the session is live and the call borrows no other memory.
        status(unsafe { nghttp2_submit_window_update(self.as_ptr(), flags.bits(), stream_id, increment) })
    }
    fn submit_shutdown_notice(&self) -> Result<()> {
        // SAFETY: the session is live and the call borrows no other memory.
        status(unsafe { nghttp2_submit_shutdown_notice(self.as_ptr()) })
    }
    fn submit_altsvc(&self, flags: FrameFlags, stream_id: i32, origin: &[u8], field_value: &[u8]) -> Result<()> {
        // SAFETY: the session is live and nghttp2 copies both slices.
        status(unsafe { nghttp2_submit_altsvc(self.as_ptr(), flags.bits(), stream_id, origin.as_ptr(), origin.len(), field_value.as_ptr(), field_value.len()) })
    }
    fn submit_origin(&self, flags: FrameFlags, origins: &[&[u8]]) -> Result<()> {
        let raw: Vec<nghttp2_origin_entry> = origins.iter().map(|origin| nghttp2_origin_entry { origin: origin.as_ptr().cast_mut(), origin_len: origin.len() }).collect();
        // SAFETY: the session is live and nghttp2 copies every origin during the call.
        status(unsafe { nghttp2_submit_origin(self.as_ptr(), flags.bits(), raw.as_ptr(), raw.len()) })
    }
    fn submit_priority_update(&self, flags: FrameFlags, stream_id: i32, field_value: &[u8]) -> Result<()> {
        // SAFETY: the session is live and nghttp2 copies `field_value`.
        status(unsafe { nghttp2_submit_priority_update(self.as_ptr(), flags.bits(), stream_id, field_value.as_ptr(), field_value.len()) })
    }
    unsafe fn submit_extension(&self, type_: FrameType, flags: FrameFlags, stream_id: i32, payload: *mut c_void) -> Result<()> {
        // SAFETY: the session is live; `payload` is only dereferenced by the
        // `pack_extension` callback while the frame can be sent.
        status(unsafe { nghttp2_submit_extension(self.as_ptr(), type_.raw(), flags.bits(), stream_id, payload) })
    }
    fn change_extpri(&self, stream_id: i32, extpri: ExtPri, ignore_client_signal: bool) -> Result<()> {
        let raw = extpri.as_raw();
        // SAFETY: the session is live and nghttp2 copies the local descriptor.
        status(unsafe { nghttp2_session_change_extpri_stream_priority(self.as_ptr(), stream_id, &raw const raw, i32::from(ignore_client_signal)) })
    }
    fn extpri(&self, stream_id: i32) -> Result<ExtPri> {
        let mut raw = ExtPri::default().as_raw();
        // SAFETY: the session is live and `raw` is a writable local.
        let code = unsafe { nghttp2_session_get_extpri_stream_priority(self.as_ptr(), &raw mut raw, stream_id) };
        status(code)?;
        ExtPri::new(raw.urgency, raw.inc != 0).map_err(Error::from_native)
    }
    fn consume(&self, stream_id: i32, size: usize) -> Result<()> {
        // SAFETY: the session is live and the call borrows no other memory.
        status(unsafe { nghttp2_session_consume(self.as_ptr(), stream_id, size) })
    }
    fn consume_connection(&self, size: usize) -> Result<()> {
        // SAFETY: the session is live and the call borrows no other memory.
        status(unsafe { nghttp2_session_consume_connection(self.as_ptr(), size) })
    }
    fn consume_stream(&self, stream_id: i32, size: usize) -> Result<()> {
        // SAFETY: the session is live and the call borrows no other memory.
        status(unsafe { nghttp2_session_consume_stream(self.as_ptr(), stream_id, size) })
    }
    fn set_local_window_size(&self, flags: FrameFlags, stream_id: i32, window_size: i32) -> Result<()> {
        // SAFETY: the session is live and the call borrows no other memory.
        status(unsafe { nghttp2_session_set_local_window_size(self.as_ptr(), flags.bits(), stream_id, window_size) })
    }
    fn resume_data(&self, stream_id: i32) -> Result<()> {
        // SAFETY: the session is live and the call borrows no other memory.
        status(unsafe { nghttp2_session_resume_data(self.as_ptr(), stream_id) })
    }
}

#[rustfmt::skip]
impl SessionSubmit for SessionCtx<'_> {
    unsafe fn submit_request(&self, pri: Option<&PrioritySpec>, nva: &[HeaderField<'_>], data: Option<&mut DataProvider>, user_data: *mut c_void) -> Result<i32> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        // SAFETY: retained buffers and provider ownership follow the caller contract.
        unsafe { handle.submit_request(pri, nva, data, user_data) }
    }
    unsafe fn submit_response(&self, stream_id: i32, nva: &[HeaderField<'_>], data: Option<&mut DataProvider>) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        // SAFETY: retained buffers and provider ownership follow the caller contract.
        unsafe { handle.submit_response(stream_id, nva, data) }
    }
    unsafe fn submit_headers(&self, flags: FrameFlags, stream_id: i32, pri: Option<&PrioritySpec>, nva: &[HeaderField<'_>], user_data: *mut c_void) -> Result<i32> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        // SAFETY: retained buffers and provider ownership follow the caller contract.
        unsafe { handle.submit_headers(flags, stream_id, pri, nva, user_data) }
    }
    unsafe fn submit_data(&self, flags: FrameFlags, stream_id: i32, data: &mut DataProvider) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        // SAFETY: retained buffers and provider ownership follow the caller contract.
        unsafe { handle.submit_data(flags, stream_id, data) }
    }
    unsafe fn submit_trailer(&self, stream_id: i32, nva: &[HeaderField<'_>]) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        // SAFETY: retained buffers and provider ownership follow the caller contract.
        unsafe { handle.submit_trailer(stream_id, nva) }
    }
    fn submit_rst_stream(&self, flags: FrameFlags, stream_id: i32, error_code: ErrorCode) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.submit_rst_stream(flags, stream_id, error_code)
    }
    fn submit_settings(&self, entries: &[SettingsEntry]) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.submit_settings(entries)
    }
    fn submit_ping(&self, flags: FrameFlags, opaque: Option<&[u8; 8]>) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.submit_ping(flags, opaque)
    }
    fn submit_goaway(&self, flags: FrameFlags, last_stream_id: i32, error_code: ErrorCode, opaque: &[u8]) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.submit_goaway(flags, last_stream_id, error_code, opaque)
    }
    fn submit_window_update(&self, flags: FrameFlags, stream_id: i32, increment: i32) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.submit_window_update(flags, stream_id, increment)
    }
    fn submit_shutdown_notice(&self) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.submit_shutdown_notice()
    }
    fn submit_altsvc(&self, flags: FrameFlags, stream_id: i32, origin: &[u8], field_value: &[u8]) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.submit_altsvc(flags, stream_id, origin, field_value)
    }
    fn submit_origin(&self, flags: FrameFlags, origins: &[&[u8]]) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.submit_origin(flags, origins)
    }
    fn submit_priority_update(&self, flags: FrameFlags, stream_id: i32, field_value: &[u8]) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.submit_priority_update(flags, stream_id, field_value)
    }
    unsafe fn submit_extension(&self, type_: FrameType, flags: FrameFlags, stream_id: i32, payload: *mut c_void) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        // SAFETY: retained buffers and provider ownership follow the caller contract.
        unsafe { handle.submit_extension(type_, flags, stream_id, payload) }
    }
    fn change_extpri(&self, stream_id: i32, extpri: ExtPri, ignore_client_signal: bool) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.change_extpri(stream_id, extpri, ignore_client_signal)
    }
    fn extpri(&self, stream_id: i32) -> Result<ExtPri> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.extpri(stream_id)
    }
    fn consume(&self, stream_id: i32, size: usize) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.consume(stream_id, size)
    }
    fn consume_connection(&self, size: usize) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.consume_connection(size)
    }
    fn consume_stream(&self, stream_id: i32, size: usize) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.consume_stream(stream_id, size)
    }
    fn set_local_window_size(&self, flags: FrameFlags, stream_id: i32, window_size: i32) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.set_local_window_size(flags, stream_id, window_size)
    }
    fn resume_data(&self, stream_id: i32) -> Result<()> {
        // SAFETY: the context guarantees a live session for this borrow.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new_unchecked(self.as_ptr())) };
        handle.resume_data(stream_id)
    }
}

#[cfg(test)]
#[path = "../tests/support/submit.rs"]
mod tests;
