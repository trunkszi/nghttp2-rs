//! HTTP/2 integration scenarios over native sessions and an in-memory transport.
use std::{cell::Cell, rc::Rc};

use anyhow::{Context, Result, bail};
use nghttp2::{
    Client, ClientSession, CustomAllocator, DataPoll, DataProvider, DataSource, Error, ErrorCode,
    EventCollector, ExtPri, FrameFlags, FrameType, HeaderField, Server, ServerSession, Session,
    SessionCtx, SessionEvent, SessionHandler, SessionOptions, SettingsEntry, SettingsId,
    callbacks::{CallbackError, CallbackResult, Frame},
    nv, sys,
    types::HeadersCategory,
};

fn request() -> [HeaderField<'static>; 5] {
    [
        nv!(":method", "POST"),
        nv!(":scheme", "https"),
        nv!(":authority", "example.com"),
        nv!(":path", "/hft.OrderService/PlaceOrder"),
        nv!("content-type", "application/grpc"),
    ]
}

fn transfer<R, H: SessionHandler, S, G: SessionHandler>(
    from: &mut Session<R, H>,
    to: &mut Session<S, G>,
) -> Result<Vec<u8>> {
    let mut wire = Vec::new();
    for _ in 0..128 {
        let bytes = from.mem_send().context("serialize parity frames")?.to_vec();
        if bytes.is_empty() {
            return Ok(wire);
        }
        assert_eq!(to.mem_recv(&bytes)?, bytes.len());
        wire.extend_from_slice(&bytes);
    }
    bail!("sender did not quiesce");
}

fn pump<H: SessionHandler, G: SessionHandler>(
    client: &mut Session<Client, H>,
    server: &mut Session<Server, G>,
) -> Result<()> {
    for _ in 0..16 {
        if transfer(client, server)?.len() + transfer(server, client)?.len() == 0 {
            return Ok(());
        }
    }
    bail!("connection did not quiesce");
}

fn handshake<H: SessionHandler, G: SessionHandler>(
    client: &mut Session<Client, H>,
    server: &mut Session<Server, G>,
) -> Result<()> {
    client.submit_settings(&[])?;
    server.submit_settings(&[])?;
    pump(client, server)
}

fn pair() -> Result<(ClientSession, ServerSession)> {
    let mut client = ClientSession::new_default(None, None)?;
    let mut server = ServerSession::new_default(None, None)?;
    handshake(&mut client, &mut server)?;
    for events in [
        client.drain_events().collect::<Vec<_>>(),
        server.drain_events().collect(),
    ] {
        let inbound = received(events.clone());
        assert_eq!(
            inbound,
            vec![
                SessionEvent::FrameRecv {
                    frame_type: FrameType::SETTINGS,
                    flags: 0,
                    stream_id: 0
                },
                SessionEvent::SettingsAck,
            ]
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SessionEvent::Error { .. }))
        );
    }
    Ok((client, server))
}

fn body(events: &[SessionEvent]) -> Vec<u8> {
    events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::DataChunk { data, .. } => Some(data.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect()
}

fn received(events: Vec<SessionEvent>) -> Vec<SessionEvent> {
    events
        .into_iter()
        .filter(|e| !matches!(e, SessionEvent::FrameSend { .. }))
        .collect()
}

fn headers(
    stream_id: i32,
    category: HeadersCategory,
    fields: &[HeaderField<'_>],
    end: bool,
) -> Vec<SessionEvent> {
    let mut events = vec![SessionEvent::BeginHeaders {
        stream_id,
        category,
    }];
    events.extend(fields.iter().map(|f| {
        let raw = f.as_raw();
        // SAFETY: these borrowed descriptors reference the live fields passed to this function.
        let (name, value) = unsafe {
            (
                std::slice::from_raw_parts(raw.name, raw.namelen),
                std::slice::from_raw_parts(raw.value, raw.valuelen),
            )
        };
        SessionEvent::Header {
            stream_id,
            name: name.to_vec(),
            value: value.to_vec(),
            flags: 0,
        }
    }));
    events.push(SessionEvent::FrameRecv {
        frame_type: FrameType::HEADERS,
        flags: FrameFlags::END_HEADERS.bits()
            | if end {
                FrameFlags::END_STREAM.bits()
            } else {
                0
            },
        stream_id,
    });
    events
}

struct Chunks {
    chunks: Vec<Vec<u8>>,
    next: usize,
}
impl DataSource for Chunks {
    fn read(&mut self, _: usize) -> DataPoll<'_> {
        if self.next == self.chunks.len() {
            return DataPoll::EofWithTrailers;
        }
        let index = self.next;
        self.next += 1;
        DataPoll::Copy(&self.chunks[index])
    }
}

#[test]
fn integration_lifecycle_grpc_request_response_trailers_have_exact_receive_order() -> Result<()> {
    let (mut client, mut server) = pair()?;
    let chunks = vec![b"order:".to_vec(), b"buy-42".to_vec()];
    let stream = client.submit_request(
        &request(),
        Some(DataProvider::new(Chunks {
            chunks: chunks.clone(),
            next: 0,
        })),
    )?;
    let trailers = [nv!("grpc-status", "0")];
    pump(&mut client, &mut server)?;
    client.submit_trailer(stream, &trailers)?;
    pump(&mut client, &mut server)?;
    let mut expected = headers(stream, HeadersCategory::REQUEST, &request(), false);
    for data in chunks {
        expected.push(SessionEvent::DataChunk {
            stream_id: stream,
            flags: 0,
            data,
        });
        expected.push(SessionEvent::FrameRecv {
            frame_type: FrameType::DATA,
            flags: 0,
            stream_id: stream,
        });
    }
    expected.extend(headers(stream, HeadersCategory::HEADERS, &trailers, true));
    assert_eq!(received(server.drain_events().collect()), expected);
    let response = [
        nv!(":status", "200"),
        nv!("content-type", "application/grpc"),
    ];
    let trailers = [nv!("grpc-status", "0"), nv!("grpc-message", "ok")];
    server.submit_response(
        stream,
        &response,
        Some(DataProvider::new(Chunks {
            chunks: vec![b"accepted".to_vec()],
            next: 0,
        })),
    )?;
    pump(&mut client, &mut server)?;
    server.submit_trailer(stream, &trailers)?;
    pump(&mut client, &mut server)?;
    let mut expected = headers(stream, HeadersCategory::RESPONSE, &response, false);
    expected.extend([
        SessionEvent::DataChunk {
            stream_id: stream,
            flags: 0,
            data: b"accepted".to_vec(),
        },
        SessionEvent::FrameRecv {
            frame_type: FrameType::DATA,
            flags: 0,
            stream_id: stream,
        },
    ]);
    expected.extend(headers(stream, HeadersCategory::HEADERS, &trailers, true));
    expected.push(SessionEvent::StreamClose {
        stream_id: stream,
        error_code: ErrorCode::NO_ERROR,
    });
    assert_eq!(received(client.drain_events().collect()), expected);
    assert_eq!(server.drain_events().filter(|e| matches!(e, SessionEvent::StreamClose { error_code, .. } if *error_code == ErrorCode::NO_ERROR)).count(), 1);
    assert!(client.find_stream(stream).is_none());
    assert!(server.find_stream(stream).is_none());
    Ok(())
}

#[derive(Default)]
struct Respond {
    pending: usize,
    consumed: usize,
    submitted: usize,
}
#[nghttp2_macros::nghttp2_callbacks]
impl SessionHandler for Respond {
    fn on_data_chunk_recv(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: u8,
        _: i32,
        data: &[u8],
    ) -> CallbackResult {
        self.pending += data.len();
        Ok(())
    }
    fn on_frame_recv(&mut self, ctx: &mut SessionCtx<'_>, frame: &Frame) -> CallbackResult {
        // SAFETY: every native union member begins with the initialized common header.
        let hd = unsafe { frame.hd };
        if hd.type_ == FrameType::DATA.raw() {
            let bytes = std::mem::take(&mut self.pending);
            ctx.consume(hd.stream_id, bytes)
                .map_err(|_| CallbackError::Failure)?;
            self.consumed += bytes;
        }
        if hd.stream_id != 0 && hd.flags & FrameFlags::END_STREAM.bits() != 0 {
            assert_eq!(
                ctx.find_stream(hd.stream_id).map(|s| s.id()),
                Some(hd.stream_id)
            );
            ctx.submit_response(
                hd.stream_id,
                &[nv!(":status", "200")],
                Some(DataProvider::from_vec(b"callback-response".to_vec())),
            )
            .map_err(|_| CallbackError::Failure)?;
            self.submitted += 1;
        }
        Ok(())
    }
}

#[test]
fn integration_scenario_callback_context_submits_response_and_consumes_data() -> Result<()> {
    let mut options = SessionOptions::new().map_err(Error::from_native)?;
    options.set_no_auto_window_update(true);
    let mut client = ClientSession::new_default(None, None)?;
    let mut server = Session::server(Respond::default(), Some(&options), None)?;
    handshake(&mut client, &mut server)?;
    let stream = client.submit_request(
        &request(),
        Some(DataProvider::from_vec(b"request-data".to_vec())),
    )?;
    pump(&mut client, &mut server)?;
    assert_eq!(server.handler().consumed, 12);
    assert_eq!(server.handler().submitted, 1);
    let events: Vec<_> = client.drain_events().collect();
    assert_eq!(body(&events), b"callback-response");
    assert!(events.contains(&SessionEvent::StreamClose {
        stream_id: stream,
        error_code: ErrorCode::NO_ERROR
    }));
    Ok(())
}

#[derive(Default)]
struct NoCopyWire {
    wire: Vec<u8>,
    lengths: Vec<usize>,
    padded: usize,
}
impl SessionHandler for NoCopyWire {
    fn select_padding(&mut self, _: &mut SessionCtx<'_>, frame: &Frame, max: usize) -> isize {
        // SAFETY: all frame variants have an initialized common header.
        let hd = unsafe { frame.hd };
        (hd.length
            + if hd.type_ == FrameType::DATA.raw() {
                3
            } else {
                0
            })
        .min(max) as isize
    }
    fn send_data(
        &mut self,
        _: &mut SessionCtx<'_>,
        frame: &Frame,
        header: &[u8],
        len: usize,
        source: &sys::nghttp2_data_source,
    ) -> i32 {
        // SAFETY: this callback receives the source of a live retained DataProvider.
        let (ptr, available) =
            unsafe { nghttp2::submit::no_copy_chunk(std::ptr::from_ref(source).cast_mut()) }
                .expect("pinned chunk");
        assert_eq!(len, available);
        assert_eq!(header.len(), 9);
        // SAFETY: DATA callback selects the initialized DATA union variant.
        let padding = unsafe { frame.data.padlen };
        self.padded += usize::from(padding > 0);
        self.wire.extend_from_slice(header);
        if padding > 0 {
            self.wire.push(u8::try_from(padding - 1).unwrap());
        }
        // SAFETY: no_copy_chunk guarantees these bytes remain readable until this callback returns.
        self.wire
            .extend_from_slice(unsafe { std::slice::from_raw_parts(ptr, len) });
        if padding > 0 {
            self.wire.resize(self.wire.len() + padding - 1, 0);
        }
        self.lengths.push(len);
        0
    }
}

#[test]
fn integration_scenario_no_copy_splits_payload_without_changing_peer_bytes() -> Result<()> {
    let mut client = Session::client(NoCopyWire::default(), None, None)?;
    let mut server = ServerSession::new_default(None, None)?;
    handshake(&mut client, &mut server)?;
    let payload: Vec<_> = (0..40_000).map(|i| (i % 251) as u8).collect();
    let stream = client.submit_request(
        &request(),
        Some(DataProvider::from_vec_no_copy(payload.clone())),
    )?;
    for _ in 0..32 {
        let copied = client.mem_send()?.to_vec();
        let direct = std::mem::take(&mut client.handler_mut().wire);
        if copied.is_empty() && direct.is_empty() {
            break;
        }
        if !direct.is_empty() {
            assert_eq!(server.mem_recv(&direct)?, direct.len());
        }
        if !copied.is_empty() {
            assert_eq!(server.mem_recv(&copied)?, copied.len());
        }
    }
    assert!(!client.want_write());
    assert_eq!(
        client.handler().lengths.iter().sum::<usize>(),
        payload.len()
    );
    assert!(client.handler().lengths.len() >= 3);
    assert!(client.handler().padded > 0);
    let events: Vec<_> = server.drain_events().collect();
    assert_eq!(body(&events), payload);
    assert!(events.iter().any(|e| matches!(e, SessionEvent::FrameRecv { frame_type, stream_id, flags }
        if *frame_type == FrameType::DATA && *stream_id == stream && flags & FrameFlags::END_STREAM.bits() != 0)));
    Ok(())
}

struct WakeSource {
    ready: Rc<Cell<bool>>,
    sent: bool,
}
impl DataSource for WakeSource {
    fn read(&mut self, _: usize) -> DataPoll<'_> {
        if !self.ready.get() {
            DataPoll::Deferred
        } else if std::mem::replace(&mut self.sent, true) {
            DataPoll::Eof
        } else {
            DataPoll::Copy(b"resumed")
        }
    }
}

#[test]
fn integration_scenario_deferred_source_requires_resume_before_transmission() -> Result<()> {
    let (mut client, mut server) = pair()?;
    let ready = Rc::new(Cell::new(false));
    let stream = client.submit_request(
        &request(),
        Some(DataProvider::new(WakeSource {
            ready: ready.clone(),
            sent: false,
        })),
    )?;
    pump(&mut client, &mut server)?;
    assert!(body(&server.drain_events().collect::<Vec<_>>()).is_empty());
    assert!(!client.want_write());
    ready.set(true);
    assert!(
        client.mem_send()?.is_empty(),
        "readiness alone cannot resume a deferred source"
    );
    client.resume_data(stream)?;
    pump(&mut client, &mut server)?;
    assert_eq!(body(&server.drain_events().collect::<Vec<_>>()), b"resumed");
    server.submit_response(stream, &[nv!(":status", "204")], None)?;
    pump(&mut client, &mut server)?;
    assert!(client.find_stream(stream).is_none());
    Ok(())
}

#[test]
fn integration_scenario_manual_consumption_restores_exhausted_connection_and_stream_windows()
-> Result<()> {
    let mut options = SessionOptions::new().map_err(Error::from_native)?;
    options.set_no_auto_window_update(true);
    let mut client = ClientSession::new_default(None, None)?;
    let mut server = ServerSession::new_default(Some(&options), None)?;
    handshake(&mut client, &mut server)?;
    let initial = client.remote_window_size();
    let payload = vec![0x6a; 100_000];
    let stream =
        client.submit_request(&request(), Some(DataProvider::from_vec(payload.clone())))?;
    pump(&mut client, &mut server)?;
    let first = body(&server.drain_events().collect::<Vec<_>>());
    assert_eq!(first.len(), initial as usize);
    assert_eq!(client.remote_window_size(), 0);
    assert_eq!(client.stream_remote_window_size(stream), 0);
    assert_eq!(server.effective_recv_data_length(), initial);
    assert_eq!(server.stream_effective_recv_data_length(stream), initial);
    server.consume(stream, first.len())?;
    transfer(&mut server, &mut client)?;
    assert_eq!(client.remote_window_size(), initial);
    assert_eq!(client.stream_remote_window_size(stream), initial);
    assert_eq!(server.effective_recv_data_length(), 0);
    let updates: Vec<_> = client
        .drain_events()
        .filter(|e| matches!(e, SessionEvent::WindowUpdate { .. }))
        .collect();
    assert_eq!(
        updates,
        vec![
            SessionEvent::WindowUpdate {
                stream_id: 0,
                increment: initial
            },
            SessionEvent::WindowUpdate {
                stream_id: stream,
                increment: initial
            }
        ]
    );
    pump(&mut client, &mut server)?;
    let rest = body(&server.drain_events().collect::<Vec<_>>());
    assert_eq!([first, rest].concat(), payload);
    server.submit_response(stream, &[nv!(":status", "204")], None)?;
    pump(&mut client, &mut server)?;
    assert!(client.find_stream(stream).is_none());
    Ok(())
}

struct WireFrame {
    kind: u8,
    flags: u8,
    stream: i32,
    payload: Vec<u8>,
}
fn frames(wire: &[u8]) -> Vec<WireFrame> {
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset < wire.len() {
        assert!(wire.len() - offset >= 9);
        let h = &wire[offset..offset + 9];
        let len = (usize::from(h[0]) << 16) | (usize::from(h[1]) << 8) | usize::from(h[2]);
        let stream = i32::from_be_bytes(h[5..9].try_into().unwrap()) & 0x7fff_ffff;
        offset += 9;
        frames.push(WireFrame {
            kind: h[3],
            flags: h[4],
            stream,
            payload: wire[offset..offset + len].to_vec(),
        });
        offset += len;
    }
    frames
}

#[derive(Default)]
struct PriorityEvents {
    events: EventCollector,
    changed: bool,
}
impl SessionHandler for PriorityEvents {
    fn on_frame_recv(&mut self, ctx: &mut SessionCtx<'_>, frame: &Frame) -> CallbackResult {
        // SAFETY: the native frame's common header is initialized for every variant.
        if unsafe { frame.hd.type_ } == FrameType::PRIORITY_UPDATE.raw() {
            assert_eq!(
                nghttp2::SessionSubmit::extpri(ctx, 1).unwrap(),
                ExtPri::new(1, true).unwrap()
            );
            ctx.change_extpri(1, ExtPri::new(2, false).unwrap(), true)
                .map_err(|_| CallbackError::Failure)?;
            assert_eq!(
                nghttp2::SessionSubmit::extpri(ctx, 1).unwrap(),
                ExtPri::new(2, false).unwrap()
            );
            self.changed = true;
        }
        self.events.on_frame_recv(ctx, frame)
    }
    fn on_stream_close(
        &mut self,
        ctx: &mut SessionCtx<'_>,
        stream: i32,
        code: ErrorCode,
    ) -> CallbackResult {
        self.events.on_stream_close(ctx, stream, code)
    }
}

#[test]
fn integration_scenario_control_and_extension_frames_cross_the_wire() -> Result<()> {
    let mut options = SessionOptions::new().map_err(Error::from_native)?;
    options
        .set_builtin_recv_extension_type(FrameType::ALTSVC.raw())
        .set_builtin_recv_extension_type(FrameType::ORIGIN.raw());
    let mut client = ClientSession::new_default(Some(&options), None)?;
    let mut server_options = SessionOptions::new().map_err(Error::from_native)?;
    server_options.set_builtin_recv_extension_type(FrameType::PRIORITY_UPDATE.raw());
    let mut server = Session::server(PriorityEvents::default(), Some(&server_options), None)?;
    client.submit_settings(&[SettingsEntry::new(SettingsId::NO_RFC7540_PRIORITIES, 1)])?;
    server.submit_settings(&[SettingsEntry::new(SettingsId::NO_RFC7540_PRIORITIES, 1)])?;
    pump(&mut client, &mut server)?;
    assert_eq!(client.remote_settings(SettingsId::NO_RFC7540_PRIORITIES), 1);
    assert_eq!(server.remote_settings(SettingsId::NO_RFC7540_PRIORITIES), 1);
    client.handler_mut().clear();
    server.handler_mut().events.clear();
    let stream = client.submit_request(&request(), Some(DataProvider::deferred()))?;
    pump(&mut client, &mut server)?;
    client.submit_priority_update(stream, ExtPri::new(1, true).map_err(Error::from_native)?)?;
    let priority_wire = frames(&transfer(&mut client, &mut server)?);
    assert!(
        priority_wire
            .iter()
            .any(|f| f.kind == FrameType::PRIORITY_UPDATE.raw()
                && f.flags == 0
                && f.stream == 0
                && f.payload[..4] == stream.to_be_bytes()
                && &f.payload[4..] == b"u=1, i=?1")
    );
    assert!(server.handler().changed);
    let priority_events: Vec<_> =
        std::iter::from_fn(|| server.handler_mut().events.next_event()).collect();
    assert!(priority_events.iter().any(|e| matches!(e, SessionEvent::FrameRecv { frame_type, .. } if *frame_type == FrameType::PRIORITY_UPDATE)));
    server.submit_ping(false, Some(b"ping-123"))?;
    server.submit_altsvc(0, b"https://example.com", b"h2=\":443\"")?;
    server.submit_origin(&[b"https://example.com", b"https://other.example"])?;
    let wire = frames(&transfer(&mut server, &mut client)?);
    assert!(wire.iter().any(|f| f.kind == FrameType::PING.raw()
        && f.flags == 0
        && f.stream == 0
        && f.payload == b"ping-123"));
    let alt = wire
        .iter()
        .find(|f| f.kind == FrameType::ALTSVC.raw())
        .expect("ALTSVC on wire");
    assert_eq!((alt.flags, alt.stream), (0, 0));
    assert_eq!(
        alt.payload,
        [
            b"\x00\x13".as_slice(),
            b"https://example.com",
            b"h2=\":443\""
        ]
        .concat()
    );
    let origin = wire
        .iter()
        .find(|f| f.kind == FrameType::ORIGIN.raw())
        .expect("ORIGIN on wire");
    assert_eq!((origin.flags, origin.stream), (0, 0));
    assert_eq!(
        origin.payload,
        [
            b"\x00\x13".as_slice(),
            b"https://example.com",
            b"\x00\x15",
            b"https://other.example"
        ]
        .concat()
    );
    let events: Vec<_> = client.drain_events().collect();
    for kind in [FrameType::ALTSVC, FrameType::ORIGIN] {
        assert!(events.iter().any(
            |e| matches!(e, SessionEvent::FrameRecv { frame_type, .. } if *frame_type == kind)
        ));
    }
    assert!(events.contains(&SessionEvent::Ping {
        ack: false,
        opaque_data: *b"ping-123"
    }));
    transfer(&mut client, &mut server)?;
    assert!(
        std::iter::from_fn(|| server.handler_mut().events.next_event()).any(|e| e
            == SessionEvent::Ping {
                ack: true,
                opaque_data: *b"ping-123"
            })
    );
    let surviving = client.submit_request(&request(), None)?;
    pump(&mut client, &mut server)?;
    client.submit_rst_stream(stream, ErrorCode::CANCEL.raw())?;
    let rst = frames(&transfer(&mut client, &mut server)?);
    assert!(rst.iter().any(|f| f.kind == FrameType::RST_STREAM.raw()
        && f.stream == stream
        && f.payload == ErrorCode::CANCEL.raw().to_be_bytes()));
    assert!(
        std::iter::from_fn(|| server.handler_mut().events.next_event()).any(|e| e
            == SessionEvent::StreamClose {
                stream_id: stream,
                error_code: ErrorCode::CANCEL
            })
    );
    server.submit_shutdown_notice()?;
    transfer(&mut server, &mut client)?;
    assert!(client.drain_events().any(|e| matches!(e, SessionEvent::GoAway { last_stream_id: 0x7fff_ffff, error_code, .. } if error_code == ErrorCode::NO_ERROR)));
    server.submit_goaway(surviving, ErrorCode::NO_ERROR.raw(), b"finished")?;
    let goaway = frames(&transfer(&mut server, &mut client)?);
    assert!(
        goaway
            .iter()
            .any(|f| f.kind == FrameType::GOAWAY.raw() && f.payload.ends_with(b"finished"))
    );
    assert!(client.drain_events().any(|e| e
        == SessionEvent::GoAway {
            last_stream_id: surviving,
            error_code: ErrorCode::NO_ERROR,
            opaque_data: b"finished".to_vec()
        }));
    Ok(())
}

struct PanicHandler;
impl SessionHandler for PanicHandler {
    fn on_frame_recv(&mut self, _: &mut SessionCtx<'_>, _: &Frame) -> CallbackResult {
        panic!("boom")
    }
}
struct PanicSource;
impl DataSource for PanicSource {
    fn read(&mut self, _: usize) -> DataPoll<'_> {
        panic!("source-boom")
    }
}

#[test]
fn integration_scenario_callback_and_provider_panics_preserve_payload_and_poison_session()
-> Result<()> {
    let mut client = ClientSession::new_default(None, None)?;
    let mut server = Session::server(PanicHandler, None, None)?;
    client.submit_settings(&[])?;
    let mut failed = false;
    for _ in 0..8 {
        let bytes = client.mem_send()?.to_vec();
        if bytes.is_empty() {
            break;
        }
        if let Err(error) = server.mem_recv(&bytes) {
            assert_eq!(error, Error::CallbackPanicked);
            failed = true;
            break;
        }
    }
    assert!(failed);
    let payload = server.take_panic().expect("original callback panic");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"boom"));
    assert_eq!(server.mem_recv(&[]), Err(Error::CallbackPanicked));
    assert_eq!(server.mem_send(), Err(Error::CallbackPanicked));
    let (mut client, mut server) = pair()?;
    client.submit_request(&request(), Some(DataProvider::new(PanicSource)))?;
    let mut failed = false;
    for _ in 0..8 {
        match client.mem_send() {
            Ok(bytes) => {
                let bytes = bytes.to_vec();
                if !bytes.is_empty() {
                    server.mem_recv(&bytes)?;
                }
            }
            Err(error) => {
                assert_eq!(error, Error::CallbackPanicked);
                failed = true;
                break;
            }
        }
    }
    assert!(failed);
    let payload = client.take_panic().expect("original source panic");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"source-boom"));
    assert_eq!(client.mem_send(), Err(Error::CallbackPanicked));
    Ok(())
}

#[test]
fn integration_lifecycle_tracked_allocators_release_full_exchange_allocations() -> Result<()> {
    let client_mem = CustomAllocator::with_limit(2 * 1024 * 1024);
    let server_mem = CustomAllocator::with_limit(2 * 1024 * 1024);
    // SAFETY: both allocator tables and their userdata stay alive and unchanged until
    // after both sessions are dropped; the transport drives them on this thread only.
    let mut client = unsafe {
        ClientSession::client_with_raw_mem(
            EventCollector::new(),
            None,
            Some(std::ptr::from_ref(client_mem.as_raw()).cast_mut()),
        )
    }?;
    // SAFETY: server_mem likewise outlives all native callbacks and the native session.
    let mut server = unsafe {
        ServerSession::server_with_raw_mem(
            EventCollector::new(),
            None,
            Some(std::ptr::from_ref(server_mem.as_raw()).cast_mut()),
        )
    }?;
    handshake(&mut client, &mut server)?;
    let stream =
        client.submit_request(&request(), Some(DataProvider::from_vec(vec![42; 20_000])))?;
    pump(&mut client, &mut server)?;
    assert_eq!(
        body(&server.drain_events().collect::<Vec<_>>()),
        vec![42; 20_000]
    );
    server.submit_response(
        stream,
        &[nv!(":status", "200")],
        Some(DataProvider::from_vec(vec![24; 18_000])),
    )?;
    pump(&mut client, &mut server)?;
    let events: Vec<_> = client.drain_events().collect();
    assert_eq!(body(&events), vec![24; 18_000]);
    assert!(events.contains(&SessionEvent::StreamClose {
        stream_id: stream,
        error_code: ErrorCode::NO_ERROR
    }));
    for allocator in [&client_mem, &server_mem] {
        let stats = allocator.stats().unwrap();
        assert!(stats.peak_bytes > 0 && stats.live_blocks > 0);
        assert_eq!(stats.failed_allocations, 0);
    }
    drop(client);
    drop(server);
    for allocator in [&client_mem, &server_mem] {
        let stats = allocator.stats().unwrap();
        assert_eq!(stats.live_bytes, 0);
        assert_eq!(stats.live_blocks, 0);
    }
    Ok(())
}
