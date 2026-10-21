//! Native session integration tests driven through public transport APIs.

use std::{cell::Cell, collections::VecDeque, rc::Rc};

use anyhow::{Context, Result, bail};
use nghttp2::{
    Client, ClientSession, CustomAllocator, DataPoll, DataProvider, DataSource, Error, ErrorCode,
    EventCollector, FrameFlags, FrameType, HeaderField, NvFlags, Result as NativeResult, Server,
    ServerSession, Session, SessionCtx, SessionEvent, SessionHandler, SessionOptions,
    SettingsEntry, SettingsId,
    callbacks::{CallbackResult, Frame},
    nv, sys,
};

fn request() -> [HeaderField<'static>; 4] {
    [
        nv!(":method", "POST"),
        nv!(":scheme", "https"),
        nv!(":authority", "example.com"),
        nv!(":path", "/"),
    ]
}

fn transfer<R, H: SessionHandler, S, G: SessionHandler>(
    from: &mut Session<R, H>,
    to: &mut Session<S, G>,
) -> NativeResult<usize> {
    let mut total = 0;
    for _ in 0..128 {
        let bytes = from.mem_send()?.to_vec();
        if bytes.is_empty() {
            return Ok(total);
        }
        let mut offset = 0;
        while offset < bytes.len() {
            let consumed = to.mem_recv(&bytes[offset..])?;
            assert!(consumed > 0, "receiver made no progress");
            offset += consumed;
        }
        total += bytes.len();
    }
    panic!("sender did not drain within 128 frames");
}

fn pump<H: SessionHandler, G: SessionHandler>(
    client: &mut Session<Client, H>,
    server: &mut Session<Server, G>,
) -> Result<()> {
    for _ in 0..16 {
        let sent = transfer(client, server)? + transfer(server, client)?;
        if sent == 0 {
            return Ok(());
        }
    }
    bail!("connection did not quiesce within 16 exchanges");
}

fn pair() -> Result<(ClientSession, ServerSession)> {
    let mut client = Session::client(EventCollector::new(), None, None).context("create client")?;
    let mut server = Session::server(EventCollector::new(), None, None).context("create server")?;
    client.submit_settings(&[])?;
    server.submit_settings(&[])?;
    pump(&mut client, &mut server)?;
    client.handler_mut().clear();
    server.handler_mut().clear();
    Ok((client, server))
}

fn body(events: &[SessionEvent], stream: i32) -> Vec<u8> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::DataChunk {
                stream_id, data, ..
            } if *stream_id == stream => Some(data.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect()
}

fn has_header(events: &[SessionEvent], stream: i32, name: &[u8], value: &[u8]) -> bool {
    events.iter().any(|event| {
        matches!(event,
        SessionEvent::Header { stream_id, name: n, value: v, .. }
            if *stream_id == stream && n == name && v == value)
    })
}

#[test]
fn integration_lifecycle_request_response_owns_ephemeral_headers_and_split_bodies() -> Result<()> {
    let (mut client, mut server) = pair()?;
    let payload = vec![0x5a; 40_000];
    let stream = {
        let name = b"x-owned-name".to_vec();
        let value = b"ephemeral-request".to_vec();
        let mut headers = request().to_vec();
        headers.push(HeaderField::from_static(
            &name,
            &value,
            NvFlags::NO_COPY_NO_INDEX,
        ));
        client.submit_request(&headers, Some(DataProvider::from_vec(payload.clone())))?
    };
    assert_eq!(stream, 1);
    assert_eq!(client.next_stream_id(), 3);
    assert!(client.want_write());
    pump(&mut client, &mut server)?;
    let events: Vec<_> = server.drain_events().collect();
    assert!(has_header(
        &events,
        stream,
        b"x-owned-name",
        b"ephemeral-request"
    ));
    assert_eq!(body(&events, stream), payload);
    assert_eq!(server.last_proc_stream_id(), stream);
    assert_eq!(server.find_stream(stream).map(|s| s.id()), Some(stream));
    {
        let status = b"200".to_vec();
        let headers = [HeaderField::from_static(
            b":status",
            &status,
            NvFlags::NO_COPY,
        )];
        server.submit_response(
            stream,
            &headers,
            Some(DataProvider::from_vec(b"response".to_vec())),
        )?;
    }
    pump(&mut client, &mut server)?;
    let events: Vec<_> = client.drain_events().collect();
    assert!(has_header(&events, stream, b":status", b"200"));
    assert_eq!(body(&events, stream), b"response");
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::StreamClose { stream_id, error_code } if *stream_id == stream && *error_code == ErrorCode::NO_ERROR)));
    assert!(!client.want_write());
    assert!(client.next_event().is_none());
    Ok(())
}

struct Source {
    bytes: Vec<u8>,
    ready: Rc<Cell<bool>>,
    sent: bool,
    dropped: Rc<Cell<usize>>,
}

impl DataSource for Source {
    fn read(&mut self, _: usize) -> DataPoll<'_> {
        if !self.ready.get() {
            return DataPoll::Deferred;
        }
        if self.sent {
            return DataPoll::Eof;
        }
        self.sent = true;
        DataPoll::Copy(&self.bytes)
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.dropped.set(self.dropped.get() + 1);
    }
}

#[test]
fn integration_lifecycle_scenario_deferred_owned_provider_resumes_and_drops_once() -> Result<()> {
    let (mut client, mut server) = pair()?;
    let ready = Rc::new(Cell::new(false));
    let dropped = Rc::new(Cell::new(0));
    let stream = client.submit_request(
        &request(),
        Some(DataProvider::new(Source {
            bytes: b"after-resume".to_vec(),
            ready: ready.clone(),
            sent: false,
            dropped: dropped.clone(),
        })),
    )?;
    pump(&mut client, &mut server)?;
    let events: Vec<_> = server.drain_events().collect();
    assert!(body(&events, stream).is_empty());
    assert_eq!(dropped.get(), 0);
    ready.set(true);
    client.resume_data(stream)?;
    pump(&mut client, &mut server)?;
    assert_eq!(
        body(&server.drain_events().collect::<Vec<_>>(), stream),
        b"after-resume"
    );
    server.submit_response(stream, &[nv!(":status", "204")], None)?;
    pump(&mut client, &mut server)?;
    assert_eq!(dropped.get(), 1, "closed stream retained its source");
    drop(client);
    assert_eq!(dropped.get(), 1);
    Ok(())
}

struct Respond {
    dropped: Rc<Cell<usize>>,
    submitted: Option<NativeResult<()>>,
}

impl SessionHandler for Respond {
    fn on_frame_recv(&mut self, ctx: &mut SessionCtx<'_>, frame: &Frame) -> CallbackResult {
        // SAFETY: every frame union variant starts with its common header.
        let hd = unsafe { frame.hd };
        if hd.type_ != FrameType::HEADERS.raw() {
            return Ok(());
        }
        let status = b"200".to_vec();
        let headers = [HeaderField::from_static(
            b":status",
            &status,
            NvFlags::NO_COPY,
        )];
        self.submitted = Some(ctx.submit_response(
            hd.stream_id,
            &headers,
            Some(DataProvider::new(Source {
                bytes: b"callback-response".to_vec(),
                ready: Rc::new(Cell::new(true)),
                sent: false,
                dropped: self.dropped.clone(),
            })),
        ));
        Ok(())
    }
}

#[test]
fn integration_lifecycle_callback_context_retains_response_provider_and_header_storage()
-> Result<()> {
    let dropped = Rc::new(Cell::new(0));
    let mut client = Session::client(EventCollector::new(), None, None)?;
    let mut server = Session::server(
        Respond {
            dropped: dropped.clone(),
            submitted: None,
        },
        None,
        None,
    )?;
    client.submit_settings(&[])?;
    server.submit_settings(&[])?;
    let stream = client.submit_request(&request(), None)?;
    pump(&mut client, &mut server)?;
    assert_eq!(server.handler().submitted, Some(Ok(())));
    let events: Vec<_> = client.drain_events().collect();
    assert!(has_header(&events, stream, b":status", b"200"));
    assert_eq!(body(&events, stream), b"callback-response");
    drop(server);
    assert_eq!(dropped.get(), 1);
    Ok(())
}

#[test]
fn integration_scenario_headers_data_trailer_and_reset_use_public_submissions() -> Result<()> {
    let (mut client, mut server) = pair()?;
    let stream = {
        let path = b"/ephemeral".to_vec();
        let headers = [
            nv!(":method", "POST"),
            nv!(":scheme", "https"),
            nv!(":authority", "example.com"),
            HeaderField::from_static(b":path", &path, NvFlags::NO_COPY),
        ];
        client.submit_headers(0, -1, &headers)?
    };
    pump(&mut client, &mut server)?;
    let headers: Vec<_> = server.drain_events().collect();
    assert!(has_header(&headers, stream, b":path", b"/ephemeral"));
    client.submit_data(
        FrameFlags::END_STREAM.bits(),
        stream,
        DataProvider::from_vec(b"separate-data".to_vec()),
    )?;
    pump(&mut client, &mut server)?;
    assert_eq!(
        body(&server.drain_events().collect::<Vec<_>>(), stream),
        b"separate-data"
    );
    server.submit_headers(0, stream, &[nv!(":status", "200")])?;
    {
        let value = b"retained-trailer".to_vec();
        server.submit_trailer(
            stream,
            &[HeaderField::from_static(
                b"x-trailer",
                &value,
                NvFlags::NO_COPY,
            )],
        )?;
    }
    pump(&mut client, &mut server)?;
    assert!(has_header(
        &client.drain_events().collect::<Vec<_>>(),
        stream,
        b"x-trailer",
        b"retained-trailer"
    ));
    let reset = client.submit_request(&request(), None)?;
    pump(&mut client, &mut server)?;
    server.submit_rst_stream(reset, ErrorCode::CANCEL.raw())?;
    pump(&mut client, &mut server)?;
    assert!(client.drain_events().any(|e| matches!(e,
        SessionEvent::StreamClose { stream_id, error_code } if stream_id == reset && error_code == ErrorCode::CANCEL)));
    Ok(())
}

#[derive(Default)]
struct Transport {
    outgoing: Vec<u8>,
    incoming: VecDeque<u8>,
    frames: usize,
}

impl SessionHandler for Transport {
    fn send(&mut self, _: &mut SessionCtx<'_>, bytes: &[u8], _: i32) -> isize {
        let len = bytes.len().min(7);
        self.outgoing.extend_from_slice(&bytes[..len]);
        len as isize
    }
    fn recv(&mut self, _: &mut SessionCtx<'_>, buffer: &mut [u8], _: i32) -> isize {
        if self.incoming.is_empty() {
            return sys::NGHTTP2_ERR_WOULDBLOCK as isize;
        }
        let len = buffer.len().min(self.incoming.len()).min(5);
        for slot in &mut buffer[..len] {
            let Some(byte) = self.incoming.pop_front() else {
                panic!("incoming length changed")
            };
            *slot = byte;
        }
        len as isize
    }
    fn on_frame_recv(&mut self, _: &mut SessionCtx<'_>, _: &Frame) -> CallbackResult {
        self.frames += 1;
        Ok(())
    }
}

#[test]
fn integration_scenario_callback_transport_handles_short_writes_and_fragmented_reads() -> Result<()>
{
    let mut client = Session::client(Transport::default(), None, None)?;
    let mut server = Session::server(Transport::default(), None, None)?;
    client.submit_settings(&[])?;
    server.submit_settings(&[])?;
    let stream = client.submit_request(&request(), None)?;
    client.send()?;
    let wire = std::mem::take(&mut client.handler_mut().outgoing);
    server.handler_mut().incoming.extend(wire);
    server.recv()?;
    assert!(server.handler().frames >= 2);
    assert_eq!(server.last_proc_stream_id(), stream);
    server.submit_response(stream, &[nv!(":status", "204")], None)?;
    server.send()?;
    let wire = std::mem::take(&mut server.handler_mut().outgoing);
    client.handler_mut().incoming.extend(wire);
    client.recv()?;
    assert!(client.handler().frames >= 2);
    assert!(client.handler().incoming.is_empty());
    Ok(())
}

struct PanicHandler;
impl SessionHandler for PanicHandler {
    fn send(&mut self, _: &mut SessionCtx<'_>, _: &[u8], _: i32) -> isize {
        panic!("send panic")
    }
    fn recv(&mut self, _: &mut SessionCtx<'_>, _: &mut [u8], _: i32) -> isize {
        panic!("recv panic")
    }
    fn on_frame_recv(&mut self, _: &mut SessionCtx<'_>, _: &Frame) -> CallbackResult {
        panic!("frame panic")
    }
    fn before_frame_send(&mut self, _: &mut SessionCtx<'_>, _: &Frame) -> CallbackResult {
        panic!("frame send panic")
    }
}

fn assert_panic<R, H: SessionHandler>(session: &mut Session<R, H>, message: &str) {
    let Some(payload) = session.take_panic() else {
        panic!("missing captured panic")
    };
    assert_eq!(payload.downcast_ref::<&str>().copied(), Some(message));
    assert!(session.take_panic().is_none());
    assert_eq!(session.mem_recv(&[]), Err(Error::CallbackPanicked));
    assert_eq!(session.mem_send(), Err(Error::CallbackPanicked));
    assert_eq!(session.send(), Err(Error::CallbackPanicked));
    assert_eq!(session.recv(), Err(Error::CallbackPanicked));
    assert_eq!(session.submit_settings(&[]), Err(Error::CallbackPanicked));
}

#[test]
fn integration_scenario_all_transport_drivers_surface_caught_callback_panics() -> Result<()> {
    let mut send = Session::client(PanicHandler, None, None)?;
    assert_eq!(send.send(), Err(Error::CallbackPanicked));
    assert_panic(&mut send, "send panic");
    let mut recv = Session::server(PanicHandler, None, None)?;
    assert_eq!(recv.recv(), Err(Error::CallbackPanicked));
    assert_panic(&mut recv, "recv panic");
    let mut mem_send = Session::server(PanicHandler, None, None)?;
    mem_send.submit_settings(&[])?;
    assert_eq!(mem_send.mem_send(), Err(Error::CallbackPanicked));
    assert_panic(&mut mem_send, "frame send panic");
    let mut source = Session::client(EventCollector::new(), None, None)?;
    source.submit_settings(&[])?;
    let mut wire = Vec::new();
    for _ in 0..16 {
        let chunk = source.mem_send()?;
        if chunk.is_empty() {
            break;
        }
        wire.extend_from_slice(chunk);
    }
    assert!(source.mem_send()?.is_empty(), "source did not drain");
    let mut mem_recv = Session::server(PanicHandler, None, None)?;
    assert_eq!(mem_recv.mem_recv(&wire), Err(Error::CallbackPanicked));
    assert_panic(&mut mem_recv, "frame panic");
    Ok(())
}

struct PanicSource;
impl DataSource for PanicSource {
    fn read(&mut self, _: usize) -> DataPoll<'_> {
        panic!("provider panic")
    }
}

#[test]
fn integration_scenario_owned_provider_panic_is_returned_before_native_callback_failure()
-> Result<()> {
    let mut client = Session::client(EventCollector::new(), None, None)?;
    client.submit_request(&request(), Some(DataProvider::new(PanicSource)))?;
    for _ in 0..16 {
        match client.mem_send() {
            Err(error) => {
                assert_eq!(error, Error::CallbackPanicked);
                assert_panic(&mut client, "provider panic");
                return Ok(());
            }
            Ok(chunk) => assert!(!chunk.is_empty(), "provider was never polled"),
        }
    }
    panic!("provider was never polled within 16 frames");
}

#[test]
fn integration_lifecycle_options_survive_owner_drop_and_manual_flow_control_is_observable()
-> Result<()> {
    let mut server = {
        let mut options = SessionOptions::new().map_err(Error::from_native)?;
        options
            .set_no_auto_window_update(true)
            .set_no_auto_ping_ack(true);
        Session::server(EventCollector::new(), Some(&options), None)?
    };
    let mut client = Session::client(EventCollector::new(), None, None)?;
    client.submit_settings(&[SettingsEntry::new(SettingsId::MAX_CONCURRENT_STREAMS, 17)])?;
    server.submit_settings(&[])?;
    let stream = client.submit_request(&request(), Some(DataProvider::from_vec(vec![7; 1024])))?;
    client.submit_ping(false, Some(b"12345678"))?;
    pump(&mut client, &mut server)?;
    assert_eq!(
        server.remote_settings(SettingsId::MAX_CONCURRENT_STREAMS),
        17
    );
    assert_eq!(
        client.local_settings(SettingsId::MAX_CONCURRENT_STREAMS),
        17
    );
    assert_eq!(server.effective_recv_data_length(), 1024);
    assert_eq!(server.stream_effective_recv_data_length(stream), 1024);
    assert!(server.local_window_size() > 0);
    assert!(server.stream_local_window_size(stream) > 0);
    assert!(client.remote_window_size() > 0);
    assert!(client.stream_remote_window_size(stream) > 0);
    server.consume(stream, 512)?;
    server.consume_connection(256)?;
    server.consume_stream(stream, 256)?;
    server.set_local_window_size(0, 0, 100_000)?;
    assert!(
        !client
            .drain_events()
            .any(|event| matches!(event, SessionEvent::Ping { ack: true, .. }))
    );
    server.submit_ping(true, Some(b"12345678"))?;
    pump(&mut client, &mut server)?;
    assert!(client.drain_events().any(|event| matches!(event,
        SessionEvent::Ping { ack: true, opaque_data } if opaque_data == *b"12345678")));
    Ok(())
}

#[test]
fn integration_scenario_control_frames_reach_peer_and_invalid_role_returns_native_error()
-> Result<()> {
    let (mut client, mut server) = pair()?;
    assert!(client.submit_shutdown_notice().is_err());
    assert!(
        client
            .submit_altsvc(0, b"https://example.com", b"h3=\":443\"")
            .is_err()
    );
    server.submit_altsvc(0, b"https://example.com", b"h3=\":443\"")?;
    server.submit_origin(&[b"https://example.com"])?;
    server.submit_window_update(0, 0, 4096)?;
    server.submit_goaway(0, ErrorCode::NO_ERROR.raw(), b"finished")?;
    assert!(server.outbound_queue_size() > 0);
    pump(&mut client, &mut server)?;
    let events: Vec<_> = client.drain_events().collect();
    assert!(events.iter().any(|event| matches!(
        event,
        SessionEvent::WindowUpdate {
            stream_id: 0,
            increment: 4096
        }
    )));
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::GoAway { last_stream_id: 0, error_code, opaque_data }
            if *error_code == ErrorCode::NO_ERROR && opaque_data == b"finished")));
    assert!(!server.want_write());
    Ok(())
}

#[test]
fn integration_lifecycle_native_allocator_is_released_on_session_drop_and_constructor_failure()
-> Result<()> {
    for limit in [0, 64, 512, 4096, 16384, usize::MAX] {
        let allocator = CustomAllocator::with_limit(limit);
        let raw = std::ptr::from_ref(allocator.as_raw()).cast_mut();
        // SAFETY: the allocator's table and state outlive the session and its deletion.
        let result =
            unsafe { Session::client_with_raw_mem(EventCollector::new(), None, Some(raw)) };
        match result {
            Ok(session) => drop(session),
            Err(error) => assert_eq!(error, Error::from_native(sys::NGHTTP2_ERR_NOMEM)),
        }
        let Some(stats) = allocator.stats() else {
            panic!("tracked allocator lost counters")
        };
        assert_eq!(stats.live_blocks, 0, "budget {limit}");
        assert_eq!(stats.live_bytes, 0, "budget {limit}");
        if limit == 0 {
            assert!(stats.failed_allocations > 0);
        }
    }
    let mut owned = Session::server(EventCollector::new(), None, Some(CustomAllocator::new()))?;
    owned.submit_settings(&[])?;
    assert!(!owned.mem_send()?.is_empty());
    assert!(
        Session::server(
            EventCollector::new(),
            None,
            Some(CustomAllocator::with_limit(0))
        )
        .is_err()
    );
    Ok(())
}

#[derive(Default)]
struct NoCopyTransport {
    wire: Vec<u8>,
}

impl SessionHandler for NoCopyTransport {
    fn send_data(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: &Frame,
        header: &[u8],
        length: usize,
        source: &sys::nghttp2_data_source,
    ) -> i32 {
        // SAFETY: nghttp2 passes this live provider's source to its send-data callback.
        let Some((pointer, len)) =
            (unsafe { nghttp2::submit::no_copy_chunk(std::ptr::from_ref(source).cast_mut()) })
        else {
            return sys::NGHTTP2_ERR_CALLBACK_FAILURE;
        };
        assert_eq!(length, len);
        self.wire.extend_from_slice(header);
        // SAFETY: the provider retains these len bytes until send_data returns.
        self.wire
            .extend_from_slice(unsafe { std::slice::from_raw_parts(pointer, len) });
        0
    }
}

#[test]
fn integration_lifecycle_owned_no_copy_provider_sends_native_data_frames() -> Result<()> {
    let mut client = Session::client(NoCopyTransport::default(), None, None)?;
    let mut server = Session::server(EventCollector::new(), None, None)?;
    client.submit_settings(&[])?;
    server.submit_settings(&[])?;
    pump(&mut client, &mut server)?;
    let payload = vec![0xa5; 20_000];
    let stream = client.submit_request(
        &request(),
        Some(DataProvider::from_vec_no_copy(payload.clone())),
    )?;
    for _ in 0..32 {
        let frame = client.mem_send()?.to_vec();
        let no_copy = std::mem::take(&mut client.handler_mut().wire);
        if frame.is_empty() && no_copy.is_empty() {
            break;
        }
        if !no_copy.is_empty() {
            assert_eq!(server.mem_recv(&no_copy)?, no_copy.len());
        }
        if !frame.is_empty() {
            assert_eq!(server.mem_recv(&frame)?, frame.len());
        }
    }
    assert!(!client.want_write(), "no-copy sender did not drain");
    assert_eq!(
        body(&server.drain_events().collect::<Vec<_>>(), stream),
        payload
    );
    Ok(())
}

fn tracked_source(dropped: &Rc<Cell<usize>>, ready: bool) -> DataProvider {
    DataProvider::new(Source {
        bytes: b"owned-source".to_vec(),
        ready: Rc::new(Cell::new(ready)),
        sent: false,
        dropped: dropped.clone(),
    })
}

#[test]
fn integration_lifecycle_scenario_rejected_sources_are_released_without_another_drive() -> Result<()>
{
    let mut client = ClientSession::new_default(None, None)?;
    let dropped = Rc::new(Cell::new(0));
    for count in 1..=4 {
        assert!(
            client
                .submit_data(0, 0, tracked_source(&dropped, true))
                .is_err()
        );
        assert_eq!(dropped.get(), count, "native rejection retained source");
    }
    drop(client);
    assert_eq!(dropped.get(), 4);
    let mut server = ServerSession::new_default(None, None)?;
    assert!(
        server
            .submit_request(&request(), Some(tracked_source(&dropped, true)))
            .is_err()
    );
    assert_eq!(dropped.get(), 5, "rejected request retained source");
    assert!(
        server
            .submit_response(
                0,
                &[nv!(":status", "200")],
                Some(tracked_source(&dropped, true))
            )
            .is_err()
    );
    assert_eq!(dropped.get(), 6, "rejected response retained source");
    Ok(())
}

#[test]
fn integration_lifecycle_scenario_sessions_with_matching_stream_ids_release_only_their_sources()
-> Result<()> {
    let (mut first, mut peer) = pair()?;
    let (mut other, mut other_peer) = pair()?;
    let first_dropped = Rc::new(Cell::new(0));
    let other_dropped = Rc::new(Cell::new(0));
    let stream = first.submit_request(&request(), Some(tracked_source(&first_dropped, false)))?;
    assert_eq!(
        other.submit_request(&request(), Some(tracked_source(&other_dropped, false)))?,
        stream
    );
    pump(&mut first, &mut peer)?;
    pump(&mut other, &mut other_peer)?;
    first.submit_rst_stream(stream, ErrorCode::CANCEL.raw())?;
    pump(&mut first, &mut peer)?;
    assert_eq!(first_dropped.get(), 1);
    assert_eq!(other_dropped.get(), 0);
    assert!(other.find_stream(stream).is_some());
    drop(first);
    assert_eq!(other_dropped.get(), 0);
    drop(other);
    assert_eq!(other_dropped.get(), 1);
    Ok(())
}

struct ContextData {
    submitted: Option<NativeResult<()>>,
    panic: bool,
}

impl SessionHandler for ContextData {
    fn on_frame_recv(&mut self, ctx: &mut SessionCtx<'_>, frame: &Frame) -> CallbackResult {
        // SAFETY: every native frame union variant starts with its common header.
        let hd = unsafe { frame.hd };
        if hd.type_ != FrameType::HEADERS.raw() {
            return Ok(());
        }
        assert_eq!(
            ctx.find_stream(hd.stream_id).map(|stream| stream.id()),
            Some(hd.stream_id)
        );
        assert_eq!(ctx.root_stream().id(), 0);
        assert!(ctx.remote_window_size() > 0);
        assert!(ctx.stream_remote_window_size(hd.stream_id) > 0);
        let value = b"200".to_vec();
        let fields = [HeaderField::from_static(
            b":status",
            &value,
            NvFlags::NO_COPY,
        )];
        let result = ctx.submit_headers(0, hd.stream_id, &fields).and_then(|_| {
            let source = if self.panic {
                DataProvider::new(PanicSource)
            } else {
                DataProvider::from_vec(b"context-data".to_vec())
            };
            ctx.submit_data(FrameFlags::END_STREAM.bits(), hd.stream_id, source)
        });
        self.submitted = Some(result);
        Ok(())
    }
}

#[test]
fn integration_scenario_callback_owned_data_uses_the_owning_sessions_panic_and_lifetime_state()
-> Result<()> {
    for panics in [false, true] {
        let mut client = ClientSession::new_default(None, None)?;
        let mut server = Session::server(
            ContextData {
                submitted: None,
                panic: panics,
            },
            None,
            None,
        )?;
        client.submit_settings(&[])?;
        server.submit_settings(&[])?;
        let stream = client.submit_request(&request(), None)?;
        transfer(&mut client, &mut server)?;
        assert_eq!(server.handler().submitted, Some(Ok(())));
        let result = transfer(&mut server, &mut client);
        if panics {
            assert_eq!(result, Err(Error::CallbackPanicked));
            assert_panic(&mut server, "provider panic");
        } else {
            result?;
            let events: Vec<_> = client.drain_events().collect();
            assert!(has_header(&events, stream, b":status", b"200"));
            assert_eq!(body(&events, stream), b"context-data");
        }
    }
    Ok(())
}

#[test]
fn integration_scenario_submission_oom_remains_fatal_after_allocator_recovers() -> Result<()> {
    let allocator = CustomAllocator::new();
    let raw = std::ptr::from_ref(allocator.as_raw()).cast_mut();
    // SAFETY: allocator and its callback state outlive the session and its deletion.
    let mut client =
        unsafe { Session::client_with_raw_mem(EventCollector::new(), None, Some(raw)) }?;
    assert!(allocator.set_limit(0));
    let error = client
        .submit_ping(false, None)
        .expect_err("PING allocation must fail");
    assert_eq!(error, Error::from_native(sys::NGHTTP2_ERR_NOMEM));
    assert!(allocator.set_limit(usize::MAX));
    assert_eq!(client.submit_ping(false, None), Err(error));
    assert_eq!(client.mem_send(), Err(error));
    assert_eq!(client.mem_recv(&[]), Err(error));
    assert_eq!(client.send(), Err(error));
    assert_eq!(client.recv(), Err(error));
    drop(client);
    let Some(stats) = allocator.stats() else {
        panic!("tracked allocator lost counters")
    };
    assert_eq!(stats.live_blocks, 0);
    assert_eq!(stats.live_bytes, 0);
    Ok(())
}
