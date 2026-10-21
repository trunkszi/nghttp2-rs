//! In-memory HTTP/2 request and response, with DATA and trailing headers.
use anyhow::{Context, Result, ensure};
use nghttp2::{
    event::SessionEvent,
    nv::nv,
    session::{ClientSession, ServerSession},
    submit::{DataPoll, DataProvider, DataSource},
    types::ErrorCode,
};

struct Body {
    chunks: std::vec::IntoIter<&'static [u8]>,
}
impl DataSource for Body {
    fn read(&mut self, _max_len: usize) -> DataPoll<'_> {
        match self.chunks.next() {
            Some(bytes) => DataPoll::Copy(bytes),
            None => DataPoll::EofWithTrailers,
        }
    }
}
fn body(chunks: Vec<&'static [u8]>) -> DataProvider {
    DataProvider::new(Body {
        chunks: chunks.into_iter(),
    })
}

fn pump(client: &mut ClientSession, server: &mut ServerSession) -> Result<()> {
    loop {
        // Copy each borrowed native buffer before the next operation on its owner.
        let request = client
            .mem_send()
            .context("serialize client frames")?
            .to_vec();
        let request_len = request.len();
        if request_len != 0 {
            ensure!(
                server.mem_recv(&request).context("parse request frames")? == request_len,
                "server did not consume the complete request buffer"
            );
        }
        let response = server
            .mem_send()
            .context("serialize server frames")?
            .to_vec();
        let response_len = response.len();
        if response_len != 0 {
            ensure!(
                client
                    .mem_recv(&response)
                    .context("parse response frames")?
                    == response_len,
                "client did not consume the complete response buffer"
            );
        }
        if request_len == 0 && response_len == 0 {
            return Ok(());
        }
    }
}
fn received(events: &[SessionEvent], stream: i32, expected: &[u8]) -> Result<()> {
    let data: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::DataChunk {
                stream_id, data, ..
            } if *stream_id == stream => Some(data.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect();
    ensure!(
        data == expected,
        "unexpected DATA payload on stream {stream}"
    );
    ensure!(
        events.iter().any(|event| matches!(event,
            SessionEvent::Header { stream_id, name, value, .. }
            if *stream_id == stream && name == b"grpc-status" && value == b"0"
        )),
        "stream {stream} did not receive grpc-status 0"
    );
    Ok(())
}

fn main() -> Result<()> {
    let mut client = ClientSession::new_default(None, None).context("create client session")?;
    let mut server = ServerSession::new_default(None, None).context("create server session")?;
    client.submit_settings(&[])?;
    server.submit_settings(&[])?;
    pump(&mut client, &mut server)?;

    let request = [
        nv!(":method", "POST"),
        nv!(":scheme", "https"),
        nv!(":authority", "example.com"),
        nv!(":path", "/hft.OrderService/PlaceOrder"),
        nv!("content-type", "application/grpc"),
        nv!("te", "trailers"),
    ];
    let stream = client.submit_request(&request, Some(body(vec![b"pi", b"ng"])))?;
    pump(&mut client, &mut server)?;
    client.submit_trailer(stream, &[nv!("grpc-status", "0")])?;
    pump(&mut client, &mut server)?;
    let request_events: Vec<_> = server.drain_events().collect();
    received(&request_events, stream, b"ping").context("validate request")?;

    server.submit_response(
        stream,
        &[
            nv!(":status", "200"),
            nv!("content-type", "application/grpc"),
        ],
        Some(body(vec![b"po", b"ng"])),
    )?;
    pump(&mut client, &mut server)?;
    server.submit_trailer(
        stream,
        &[nv!("grpc-status", "0"), nv!("grpc-message", "ok")],
    )?;
    pump(&mut client, &mut server)?;
    let response_events: Vec<_> = client.drain_events().collect();
    received(&response_events, stream, b"pong").context("validate response")?;
    for events in [&response_events, &server.drain_events().collect()] {
        ensure!(
            events.iter().any(|event| matches!(event,
                SessionEvent::StreamClose { stream_id, error_code }
                if *stream_id == stream && *error_code == ErrorCode::NO_ERROR
            )),
            "stream {stream} did not close cleanly on both endpoints"
        );
    }
    println!("stream {stream}: request ping, response pong, grpc-status 0; both endpoints closed");
    Ok(())
}
