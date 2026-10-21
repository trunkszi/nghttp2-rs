//! Native lifecycle and flow-control boundary regressions using in-memory transport.
use std::{cell::RefCell, rc::Rc};

use nghttp2::{
    Client, ClientSession, CustomAllocator, DataPoll, DataProvider, DataSource, FrameFlags,
    FrameType, HeaderField, RcBuf, Result, Server, ServerSession, Session, SessionCtx,
    SessionEvent, SessionHandler, SessionOptions, SettingsEntry, SettingsId,
    callbacks::{CallbackResult, Frame},
    nv,
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
) -> Result<()> {
    for _ in 0..128 {
        let wire = from.mem_send()?.to_vec();
        if wire.is_empty() {
            return Ok(());
        }
        assert_eq!(to.mem_recv(&wire)?, wire.len());
    }
    panic!("transport did not drain");
}
fn handshake<H: SessionHandler, G: SessionHandler>(
    client: &mut Session<Client, H>,
    server: &mut Session<Server, G>,
) -> Result<()> {
    client.submit_settings(&[])?;
    for _ in 0..3 {
        transfer(client, server)?;
        transfer(server, client)?;
    }
    Ok(())
}

struct KeepHeaders(Rc<RefCell<Vec<RcBuf>>>);
impl SessionHandler for KeepHeaders {
    fn on_header_rcbuf(
        &mut self,
        _: &mut SessionCtx<'_>,
        _: &Frame,
        name: &RcBuf,
        value: &RcBuf,
        _: u8,
    ) -> CallbackResult {
        if name.as_slice() == b"x-retained-header" {
            assert!(!name.is_static());
            assert!(!value.is_static());
            self.0.borrow_mut().extend([name.clone(), value.clone()]);
        }
        Ok(())
    }
}

#[test]
fn integration_lifecycle_dynamic_header_clones_outlive_owned_allocator_session() -> Result<()> {
    let retained = Rc::new(RefCell::new(Vec::new()));
    let mut server = Session::server(
        KeepHeaders(retained.clone()),
        None,
        Some(CustomAllocator::new()),
    )?;
    let mut client = ClientSession::new_default(None, None)?;
    server.submit_settings(&[])?;
    handshake(&mut client, &mut server)?;
    let mut fields = request().to_vec();
    fields.push(nv!("x-retained-header", "dynamic-header-value"));
    client.submit_request(&fields, None)?;
    transfer(&mut client, &mut server)?;
    assert_eq!(retained.borrow().len(), 2);
    drop(server);
    assert_eq!(retained.borrow()[0].as_slice(), b"x-retained-header");
    assert_eq!(retained.borrow()[1].as_slice(), b"dynamic-header-value");
    let final_clone = retained.borrow()[1].clone();
    retained.borrow_mut().clear();
    assert_eq!(final_clone.as_slice(), b"dynamic-header-value");
    drop(final_clone);
    Ok(())
}

struct FinalChunk {
    data: Vec<u8>,
    trailers: bool,
}
impl DataSource for FinalChunk {
    fn read(&mut self, _: usize) -> DataPoll<'_> {
        if self.trailers {
            DataPoll::CopyEofWithTrailers(&self.data)
        } else {
            DataPoll::CopyEof(&self.data)
        }
    }
}
fn manual_pair(stream_window: u32) -> Result<(ClientSession, ServerSession)> {
    let mut options = SessionOptions::new()?;
    options.set_no_auto_window_update(true);
    let mut client = ClientSession::new_default(None, None)?;
    let mut server = ServerSession::new_default(Some(&options), None)?;
    server.submit_settings(&[SettingsEntry::new(
        SettingsId::INITIAL_WINDOW_SIZE,
        stream_window,
    )])?;
    handshake(&mut client, &mut server)?;
    client.handler_mut().clear();
    server.handler_mut().clear();
    Ok((client, server))
}
fn received(events: &[SessionEvent], stream: i32) -> Vec<u8> {
    events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::DataChunk {
                stream_id, data, ..
            } if *stream_id == stream => Some(data.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect()
}
fn ended(events: &[SessionEvent], stream: i32) -> bool {
    events.iter().any(|e| matches!(e, SessionEvent::FrameRecv { frame_type, flags, stream_id }
        if *stream_id == stream && *frame_type == FrameType::DATA && flags & FrameFlags::END_STREAM.bits() != 0))
}

#[test]
fn integration_scenario_exact_window_final_payload_ends_without_window_update() -> Result<()> {
    // Exercise both windows together, only the stream window, and only the connection window.
    for (stream_window, size) in [(65_535, 65_535), (4_096, 4_096), (131_070, 65_535)] {
        for source in 0..3 {
            let (mut client, mut server) = manual_pair(stream_window)?;
            let bytes = vec![0x5a; size];
            let provider = match source {
                0 => DataProvider::from_vec(bytes.clone()),
                1 => DataProvider::bytes(&bytes),
                _ => DataProvider::new(FinalChunk {
                    data: bytes.clone(),
                    trailers: false,
                }),
            };
            let stream = client.submit_request(&request(), Some(provider))?;
            transfer(&mut client, &mut server)?;
            let events: Vec<_> = server.drain_events().collect();
            assert_eq!(received(&events, stream), bytes);
            assert!(ended(&events, stream));
            assert_eq!(client.remote_window_size(), 65_535 - size as i32);
            assert_eq!(
                client.stream_remote_window_size(stream),
                stream_window as i32 - size as i32
            );
            assert!(!client.want_write());
            assert!(client.mem_send()?.is_empty());
            // EOF is carried by the final nonempty DATA frame, rather than an extra empty frame.
            let frames = events.iter().filter(|e| matches!(e, SessionEvent::FrameRecv { frame_type, .. } if *frame_type == FrameType::DATA)).count();
            assert_eq!(frames, size.div_ceil(16_384));
        }
    }
    Ok(())
}

#[test]
fn integration_scenario_final_split_with_trailers_preserves_stream_at_zero_credit() -> Result<()> {
    let (mut client, mut server) = manual_pair(65_535)?;
    let bytes = vec![0x33; 65_535];
    let stream = client.submit_request(
        &request(),
        Some(DataProvider::new(FinalChunk {
            data: bytes.clone(),
            trailers: true,
        })),
    )?;
    transfer(&mut client, &mut server)?;
    let events: Vec<_> = server.drain_events().collect();
    assert_eq!(received(&events, stream), bytes);
    assert!(!ended(&events, stream));
    assert_eq!(client.remote_window_size(), 0);
    assert!(!client.want_write());
    client.submit_trailer(stream, &[nv!("x-completed", "yes")])?;
    transfer(&mut client, &mut server)?;
    assert!(server.drain_events().any(|e| matches!(e, SessionEvent::FrameRecv { frame_type, flags, stream_id }
        if stream_id == stream && frame_type == FrameType::HEADERS && flags & FrameFlags::END_STREAM.bits() != 0)));
    Ok(())
}
