//! Outbound frame construction: submission validates arguments and queues frames
//! without blocking; DATA comes from a buffer or a [`DataSource`], which can defer.

use core::{any::Any, ffi::c_void, panic::AssertUnwindSafe, ptr};

use nghttp2_sys::{
    NGHTTP2_DATA_FLAG_EOF, NGHTTP2_DATA_FLAG_NO_COPY, NGHTTP2_ERR_CALLBACK_FAILURE, nghttp2_nv,
    nghttp2_origin_entry, nghttp2_priority_spec, nghttp2_priority_spec_default_init,
    nghttp2_session, nghttp2_session_change_extpri_stream_priority, nghttp2_session_consume,
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
    context::SessionPtr,
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
    source: Box<dyn DataSource>,
    pending: Pending,
    no_copy: Option<(*const u8, usize)>,
    panic: Option<Box<dyn Any + Send>>,
}

/// DATA provider backed by a [`DataSource`]. The state lives in a raw box allocation,
/// so `source.ptr` stays valid across moves and field access.
pub struct DataProvider {
    state: *mut ProviderState,
    raw: RawDataProvider,
}

#[rustfmt::skip]
impl DataProvider {
    /// Wraps a caller-supplied source.
    pub fn new<S: DataSource + 'static>(source: S) -> Self {
        let state = Box::into_raw(Box::new(ProviderState { source: Box::new(source), pending: Pending::EMPTY, no_copy: None, panic: None }));
        let raw = RawDataProvider { source: RawDataSource { ptr: state.cast() }, read_callback: Some(provider_read) };
        Self { state, raw }
    }
    /// Builds a provider that copies `data` into the library buffer.
    pub fn from_vec(data: Vec<u8>) -> Self { Self::new(VecSource { data, pos: 0, no_copy: false }) }
    /// Builds a provider that sends `data` through the session's `send_data` callback.
    pub fn from_vec_no_copy(data: Vec<u8>) -> Self { Self::new(VecSource { data, pos: 0, no_copy: true }) }
    /// Returns the descriptor nghttp2 copies into its queue; valid while `self` is alive, because `source.ptr` points into the stable heap state.
    pub fn as_raw(&self) -> *const RawDataProvider { ptr::from_ref(&self.raw) }
    /// Removes the captured panic payload; a payload poisons the provider, and re-submitting it is unsafe.
    pub fn take_panic(&mut self) -> Option<Box<dyn Any + Send>> {
        // SAFETY: `self` owns the allocation and no callback runs at the same time.
        unsafe { (*self.state).panic.take() }
    }
}

impl Drop for DataProvider {
    fn drop(&mut self) {
        // SAFETY: `state` came from `Box::into_raw` and is freed exactly once here.
        drop(unsafe { Box::from_raw(self.state) });
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
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| match state.source.read(len) {
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
        DataPoll::Deferred => DEFERRED as nghttp2_ssize,
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

/// Frame submission on a live `SessionPtr` (forging one is undefined behavior); frames that cannot be sent are abandoned through `on_frame_not_send`.
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
impl SessionSubmit for SessionPtr {
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

#[cfg(test)]
#[rustfmt::skip]
mod tests {
    use core::{ffi::c_void, ptr, sync::atomic::{AtomicBool, Ordering}};
    use std::sync::Arc;

    use nghttp2_sys::{
        NGHTTP2_DATA, NGHTTP2_DEFAULT_WEIGHT, NGHTTP2_ERR_INVALID_ARGUMENT,
        NGHTTP2_ERR_INVALID_STATE, nghttp2_frame, nghttp2_option, nghttp2_option_del,
        nghttp2_option_new, nghttp2_option_set_no_auto_window_update,
        nghttp2_pack_extension_callback, nghttp2_send_data_callback, nghttp2_session,
        nghttp2_session_callbacks, nghttp2_session_callbacks_del, nghttp2_session_callbacks_new,
        nghttp2_session_callbacks_set_pack_extension_callback,
        nghttp2_session_callbacks_set_send_data_callback, nghttp2_session_client_new2,
        nghttp2_session_del, nghttp2_session_mem_recv2, nghttp2_session_mem_send2,
        nghttp2_session_server_new2,
    };
    use super::*;
    use crate::nv::nv;

    // The `nv!` expansion names the crate as `nghttp2`; this alias puts that name in scope.
    use crate as nghttp2;

    /// Owns the raw session and callbacks so tests can drive the FFI directly.
    struct TestSession {
        session: *mut nghttp2_session,
        callbacks: *mut nghttp2_session_callbacks,
    }

    impl TestSession {
        fn new(server: bool, option: *const nghttp2_option, user_data: *mut c_void,
               send_data: nghttp2_send_data_callback, pack: nghttp2_pack_extension_callback) -> Self {
            let mut callbacks = ptr::null_mut();
            // SAFETY: the out-pointer is writable and the call returns an owned object.
            let rv = unsafe { nghttp2_session_callbacks_new(&raw mut callbacks) };
            assert_eq!(rv, 0); assert!(!callbacks.is_null());
            // SAFETY: `callbacks` is live and the setters only store the trampoline.
            unsafe {
                nghttp2_session_callbacks_set_send_data_callback(callbacks, send_data);
                nghttp2_session_callbacks_set_pack_extension_callback(callbacks, pack);
            }
            let mut session = ptr::null_mut();
            // SAFETY: `callbacks` outlives the session; `option` is null or a copied owner.
            let rv = unsafe {
                if server { nghttp2_session_server_new2(&raw mut session, callbacks, user_data, option) }
                else { nghttp2_session_client_new2(&raw mut session, callbacks, user_data, option) }
            };
            assert_eq!(rv, 0); assert!(!session.is_null());
            Self { session, callbacks }
        }
        fn handle(&self) -> SessionPtr {
            match SessionPtr::new(self.session) { Some(handle) => handle, None => panic!("null session") }
        }
        /// Serializes queued frames and returns them with the final return code.
        fn send(&self) -> (Vec<u8>, isize) {
            let mut out = Vec::new();
            loop {
                let mut data: *const u8 = ptr::null();
                // SAFETY: the session is live and the returned buffer is copied at once.
                let len = unsafe { nghttp2_session_mem_send2(self.session, &raw mut data) };
                if len <= 0 { return (out, len); }
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
            unsafe { nghttp2_session_del(self.session); nghttp2_session_callbacks_del(self.callbacks); }
        }
    }

    fn request_headers() -> [HeaderField<'static>; 4] {
        [nv!(":method", "GET"), nv!(":path", "/"), nv!(":scheme", "https"), nv!(":authority", "example.com")]
    }

    /// Submits a request on a fresh client and drives one send pass.
    fn run_request(provider: Option<&mut DataProvider>) -> (Vec<u8>, isize) {
        let client = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
        let request = request_headers();
        // SAFETY: the session and provider are live for the call and the send pass.
        let id = unsafe { client.handle().submit_request(None, &request, provider, ptr::null_mut()) };
        assert_eq!(id, Ok(1));
        client.send()
    }

    #[test]
    fn submit_request_response_headers_and_trailer() {
        let client = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
        let h = client.handle();
        let request = request_headers();
        // SAFETY: the session is live, the headers are static, and no provider is passed.
        assert_eq!(unsafe { h.submit_request(None, &request, None, ptr::null_mut()) }, Ok(1));
        let (wire, code) = client.send();
        assert_eq!(code, 0);
        let server = TestSession::new(true, ptr::null(), ptr::null_mut(), None, None);
        server.receive(&wire);
        // SAFETY: the session is live and the response headers are static.
        assert_eq!(unsafe { server.handle().submit_response(1, &[nv!(":status", "200")], None) }, Ok(()));
        let (response, code) = server.send();
        assert_eq!(code, 0);
        assert!(!response.is_empty(), "response was not serialized");
        // SAFETY: `request` is static and the session is live.
        assert!(matches!(unsafe { h.submit_headers(FrameFlags::NONE, -1, None, &request, ptr::null_mut()) }, Ok(id) if id > 0));
        assert_eq!(client.send().1, 0);
        // SAFETY: the trailer headers are static and the session is live.
        assert_eq!(unsafe { h.submit_trailer(1, &[nv!("x-trailer", "done")]) }, Ok(()));
    }

    #[test]
    fn control_frames_follow_session_role() {
        let client = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
        let h = client.handle();
        let request = request_headers();
        // SAFETY: the session is live and the request headers are static.
        assert_eq!(unsafe { h.submit_request(None, &request, None, ptr::null_mut()) }, Ok(1));
        assert_eq!(client.send().1, 0);
        assert_eq!(h.submit_rst_stream(FrameFlags::NONE, 1, ErrorCode::CANCEL), Ok(()));
        assert_eq!(h.submit_settings(&[SettingsEntry::new(SettingsId::MAX_CONCURRENT_STREAMS, 100)]), Ok(()));
        assert_eq!((h.submit_ping(FrameFlags::NONE, Some(&[1, 2, 3, 4, 5, 6, 7, 8])), h.submit_goaway(FrameFlags::NONE, 0, ErrorCode::NO_ERROR, b"bye")), (Ok(()), Ok(())));
        assert_eq!((h.submit_window_update(FrameFlags::NONE, 0, 1000), h.submit_priority_update(FrameFlags::NONE, 1, b"u=1")), (Ok(()), Ok(())));
        // Stream 1 exists but no provider deferred it.
        assert_eq!(h.resume_data(1), Err(Error::from_native(NGHTTP2_ERR_INVALID_ARGUMENT)));
        assert_eq!(h.set_local_window_size(FrameFlags::NONE, 0, 100_000), Ok(()));
        let state = Error::from_native(NGHTTP2_ERR_INVALID_STATE);
        assert_eq!((h.change_extpri(1, ExtPri::default(), false), h.extpri(1)), (Err(state), Err(state)));
        assert_eq!((h.submit_altsvc(FrameFlags::NONE, 0, b"https://e.example", b"h3"), h.submit_origin(FrameFlags::NONE, &[b"https://e.example"])), (Err(state), Err(state)));
        assert_eq!(h.submit_shutdown_notice(), Err(state));
        let server = TestSession::new(true, ptr::null(), ptr::null_mut(), None, None);
        let h = server.handle();
        // Without a pending SETTINGS_NO_RFC7540_PRIORITIES value the server ignores both
        // extensible-priority calls, even for a missing stream.
        assert_eq!((h.change_extpri(1, ExtPri::default(), false), h.extpri(1)), (Ok(()), Ok(ExtPri::default())));
        assert_eq!(h.submit_altsvc(FrameFlags::NONE, 0, b"https://e.example", b"h3"), Ok(()));
        assert_eq!(h.submit_origin(FrameFlags::NONE, &[b"https://e.example"]), Ok(()));
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
        assert_eq!((h.consume(1, 10), h.consume_connection(10), h.consume_stream(1, 10)), (Ok(()), Ok(()), Ok(())));
        assert_eq!(TestSession::new(false, ptr::null(), ptr::null_mut(), None, None).handle().consume_connection(10), Err(state));
        // Extension frames need a pack callback, and reject reserved frame types first.
        let mut payload = 0u8;
        let without = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
        // SAFETY: the session is live; the frame is rejected before `payload` is stored.
        let result = unsafe { without.handle().submit_extension(FrameType::ALTSVC, FrameFlags::NONE, 0, (&raw mut payload).cast()) };
        assert_eq!(result, Err(Error::from_native(NGHTTP2_ERR_INVALID_STATE)));
        let with = TestSession::new(false, ptr::null(), ptr::null_mut(), None, Some(pack_extension));
        // SAFETY: the session is live and the frame is never serialized.
        let result = unsafe { with.handle().submit_extension(FrameType::ALTSVC, FrameFlags::NONE, 0, (&raw mut payload).cast()) };
        assert_eq!(result, Ok(()));
    }

    unsafe extern "C" fn pack_extension(_session: *mut nghttp2_session, _buf: *mut u8, _len: usize,
                                        _frame: *const nghttp2_frame, _user_data: *mut c_void) -> isize { 0 }

    struct TestSource { ready: Option<Arc<AtomicBool>>, chunk: &'static [u8], step: u8, panics: bool }

    impl DataSource for TestSource {
        fn read(&mut self, _max_len: usize) -> DataPoll<'_> {
            if self.panics { panic!("provider boom"); }
            if let Some(ready) = &self.ready && !ready.load(Ordering::SeqCst) { return DataPoll::Deferred; }
            if self.step > 0 { return DataPoll::Eof; }
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
        assert_eq!((&data[..5], &data[second..second + 5]), (&[0x00u8, 0x40, 0x00, NGHTTP2_DATA as u8, 0][..], &[0x00u8, 0x0e, 0x20, NGHTTP2_DATA as u8, 0][..]));
        assert_eq!((&data[9..second], &data[second + 9..second + 9 + 3_616]), (&payload[..16_384], &payload[16_384..]));
        assert_eq!((code, &data[data.len() - 9..]), (0, &[0u8, 0, 0, NGHTTP2_DATA as u8, FrameFlags::END_STREAM.bits(), 0, 0, 0, 1][..]));
    }

    #[test]
    fn empty_payload_sends_a_zero_length_data_frame_with_end_stream() {
        let (wire, code) = run_request(Some(&mut DataProvider::from_vec(Vec::new())));
        assert_eq!((code, &wire[wire.len() - 9..]), (0, &[0u8, 0, 0, NGHTTP2_DATA as u8, FrameFlags::END_STREAM.bits(), 0, 0, 0, 1][..]));
    }

    unsafe extern "C" fn capture(_session: *mut nghttp2_session, _frame: *mut nghttp2_frame,
                                 framehd: *const u8, _length: usize, source: *mut RawDataSource,
                                 user_data: *mut c_void) -> i32 {
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
            let client = TestSession::new(false, ptr::null(), (&raw mut captured).cast(), Some(capture), None);
            let request = request_headers();
            // SAFETY: the session and provider are live and the provider outlives `send`.
            let id = unsafe { client.handle().submit_request(None, &request, Some(&mut provider), ptr::null_mut()) };
            assert_eq!(id, Ok(1));
            let (wire, code) = client.send();
            assert_eq!(code, 0);
            let tail = wire.len() - 9;
            assert_eq!((wire[tail + 3], wire[tail + 4]), (NGHTTP2_DATA as u8, FrameFlags::END_STREAM.bits()));
        }
        assert_eq!((captured.len(), captured[3]), (9 + 12, NGHTTP2_DATA as u8));
        assert_eq!(&captured[0..3], &[0u8, 0, 12]);
        assert_eq!(&captured[9..21], b"no-copy-data");
    }

    #[test]
    fn no_copy_provider_splits_payload_across_send_data_calls() {
        let mut captured = Vec::new();
        let mut provider = DataProvider::from_vec_no_copy(vec![0xa5u8; 20_000]);
        let client = TestSession::new(false, ptr::null(), (&raw mut captured).cast(), Some(capture), None);
        // SAFETY: the session and provider are live and the provider outlives `send`.
        let id = unsafe { client.handle().submit_request(None, &request_headers(), Some(&mut provider), ptr::null_mut()) };
        assert_eq!(id, Ok(1)); assert_eq!(client.send().1, 0);
        // Each send_data call contributes a nine-byte frame header before its chunk.
        assert_eq!((&captured[..5], &captured[9 + 16_384..9 + 16_384 + 5]), (&[0x00u8, 0x40, 0x00, NGHTTP2_DATA as u8, 0][..], &[0x00u8, 0x0e, 0x20, NGHTTP2_DATA as u8, 0][..]));
        assert_eq!(captured.iter().filter(|&&byte| byte == 0xa5).count(), 20_000);
    }

    #[test]
    fn deferred_provider_waits_for_resume_data() {
        let ready = Arc::new(AtomicBool::new(false));
        let mut provider = DataProvider::new(TestSource { ready: Some(Arc::clone(&ready)), chunk: b"deferred", step: 0, panics: false });
        let client = TestSession::new(false, ptr::null(), ptr::null_mut(), None, None);
        let request = request_headers();
        // SAFETY: the session and provider are live and the provider outlives both sends.
        let id = unsafe { client.handle().submit_request(None, &request, Some(&mut provider), ptr::null_mut()) };
        assert_eq!(id, Ok(1));
        let (wire, code) = client.send();
        assert_eq!(code, 0);
        assert!(!wire.windows(8).any(|window| window == b"deferred"), "sent before resume");
        assert_eq!(client.handle().resume_data(1), Ok(()));
        ready.store(true, Ordering::SeqCst);
        let (wire, code) = client.send();
        assert_eq!(code, 0);
        assert!(wire.windows(8).any(|window| window == b"deferred"), "not sent after resume");
    }

    #[test]
    fn provider_panic_is_reported_as_callback_failure() {
        let mut provider = DataProvider::new(TestSource { ready: None, chunk: b"", step: 0, panics: true });
        let (_, code) = run_request(Some(&mut provider));
        assert_eq!(code, NGHTTP2_ERR_CALLBACK_FAILURE as isize);
        let Some(payload) = provider.take_panic() else { panic!("panic payload missing") };
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"provider boom"));
    }

    #[test]
    fn priority_and_settings_convert_to_the_native_layout() {
        let spec = PrioritySpec::new(3, 200, true);
        assert_eq!((spec.dependency(), spec.weight(), spec.exclusive()), (3, 200, true));
        // SAFETY: `as_raw` points at the descriptor owned by `spec`, still alive.
        let raw = unsafe { *spec.as_raw() };
        assert_eq!((raw.stream_id, raw.weight, raw.exclusive), (3, 200, 1));
        let default = PrioritySpec::default_for(7);
        assert_eq!((default.dependency(), default.weight(), default.exclusive()), (7, NGHTTP2_DEFAULT_WEIGHT as i32, false));
        let entry = SettingsEntry::new(SettingsId::MAX_FRAME_SIZE, 16_384);
        assert_eq!((entry.id(), entry.value()), (SettingsId::MAX_FRAME_SIZE, 16_384));
        assert_eq!((entry.as_raw().settings_id, entry.as_raw().value), (5, 16_384));
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
}
