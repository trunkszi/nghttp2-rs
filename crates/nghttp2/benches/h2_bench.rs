//! Elapsed-time microbenchmarks; session measurements include construction and teardown.
use std::{hint::black_box, time::Instant};

use anyhow::{Context, Result, bail, ensure};
use nghttp2::{
    event::SessionEvent,
    hpack::{HpDeflater, HpInflater, HpackHeader},
    nv::nv,
    session::{ClientSession, ServerSession},
    submit::DataProvider,
};

fn session_exchange() -> Result<usize> {
    let mut client = ClientSession::new_default(None, None).context("create benchmark client")?;
    let mut server = ServerSession::new_default(None, None).context("create benchmark server")?;
    client.submit_settings(&[])?;
    server.submit_settings(&[])?;
    let stream = client.submit_request(
        &[
            nv!(":method", "POST"),
            nv!(":scheme", "https"),
            nv!(":authority", "example.com"),
            nv!(":path", "/bench"),
        ],
        Some(DataProvider::from_vec(b"ping".to_vec())),
    )?;
    let mut replied = false;
    let mut received = Vec::new();
    loop {
        let request = client.mem_send()?.to_vec();
        if !request.is_empty() {
            ensure!(
                server
                    .mem_recv(&request)
                    .context("parse benchmark request")?
                    == request.len(),
                "incomplete request buffer consumption"
            );
        }
        for event in server.drain_events().collect::<Vec<_>>() {
            if let SessionEvent::DataChunk {
                stream_id, data, ..
            } = event
            {
                ensure!(stream_id == stream, "response stream identifier changed");
                ensure!(data == b"ping", "unexpected benchmark request payload");
                replied = true;
            }
        }
        if replied {
            server.submit_response(
                stream,
                &[nv!(":status", "200")],
                Some(DataProvider::from_vec(b"pong".to_vec())),
            )?;
            replied = false;
        }
        let response = server.mem_send()?.to_vec();
        if !response.is_empty() {
            ensure!(
                client
                    .mem_recv(&response)
                    .context("parse benchmark response")?
                    == response.len(),
                "incomplete response buffer consumption"
            );
        }
        for event in client.drain_events() {
            if let SessionEvent::DataChunk { data, .. } = event {
                received.extend_from_slice(&data);
            }
        }
        if request.is_empty() && response.is_empty() {
            break;
        }
    }
    ensure!(received == b"pong", "unexpected benchmark response payload");
    Ok(received.len())
}
fn main() -> Result<()> {
    let iterations = std::env::var("H2_BENCH_ITERATIONS")
        .map_or(Ok(10_000usize), |text| text.parse())
        .context("parse H2_BENCH_ITERATIONS as an unsigned integer")?;
    if iterations == 0 {
        bail!("H2_BENCH_ITERATIONS must be positive");
    }
    let headers = [
        HpackHeader::new(b":method", b"POST"),
        HpackHeader::new(b":path", b"/pkg.Service/Call"),
        HpackHeader::new(b"content-type", b"application/grpc"),
    ];
    let mut encoder = HpDeflater::new(4096).context("create HPACK encoder")?;
    let mut decoder = HpInflater::new().context("create HPACK decoder")?;
    let start = Instant::now();
    for _ in 0..iterations {
        let block = encoder.deflate_vec(black_box(&headers))?;
        let decoded = decoder.inflate_block(black_box(&block))?;
        ensure!(
            decoded.len() == headers.len(),
            "HPACK changed the header count"
        );
        black_box(decoded);
    }
    println!(
        "HPACK encode+decode, reused tables: {:.1} ns/block ({iterations} iterations)",
        start.elapsed().as_nanos() as f64 / iterations as f64
    );
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(session_exchange().context("run HTTP/2 exchange")?);
    }
    println!(
        "HTTP/2 handshake+request+response+teardown: {:.1} ns/exchange ({iterations} iterations)",
        start.elapsed().as_nanos() as f64 / iterations as f64
    );
    Ok(())
}
