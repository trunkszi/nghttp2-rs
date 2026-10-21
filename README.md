# nghttp2-rs

Safe, single-threaded Rust bindings for upstream [nghttp2](https://github.com/nghttp2/nghttp2). The workspace builds `libnghttp2` from source, checks the C ABI, and exposes standalone HPACK plus the RFC 9218 / RFC 9113 helpers on that ABI. `Session<Client, H>` and `Session<Server, H>` below are the safe-wrapper contract: re-entrant `SessionCtx`, `SessionHandler`, `EventCollector`, and frame submission. There is no async runtime.

HPACK contexts and `CustomAllocator` are `!Send` and `!Sync`. `RcBuf` must stay on the creating thread: its native reference count is not atomic, and the module contract forbids sharing it. Drive every native owner from that thread. Samples for `nv!`, HPACK, `SessionOptions`, `ExtPri`, ALPN, and `CustomAllocator` use types this crate exports today. Samples that call `Session::client`, `Session::server`, `submit_*`, or `DataProvider` follow the wrapper contract.

## Architecture

| Crate | Role |
| --- | --- |
| `nghttp2-sys` | Resolves the upstream tree, builds a static library with CMake, and exposes the public C ABI of `nghttp2.h` and `nghttp2ver.h`. |
| `nghttp2-macros` | `nv!` builds a zero-copy `HeaderField<'static>`. `#[nghttp2_callbacks]` rejects `SessionHandler` methods the session cannot bind. |
| `nghttp2` | Safe client and server sessions, submission, flow control, HPACK, options, priorities, `RcBuf`, and allocators. Raw declarations are re-exported as `nghttp2::sys`. |

```text
application
    |
    v
nghttp2::Session<Client|Server, H: SessionHandler>
    |  owns the native session and a heap-pinned CallbackBox<H>
    |  callbacks see (&mut H, SessionCtx<'_>) — two disjoint borrows
    v
nghttp2-sys  ->  libnghttp2.a
```

`Session<Client, H>` and `Session<Server, H>` share one state machine. The role marker selects `nghttp2_session_client_new3` or `nghttp2_session_server_new3` and which preface the peer must speak. `ClientSession` and `ServerSession` are the `EventCollector` aliases.

## Upstream source

`nghttp2-sys` does not look for a system `libnghttp2` and does not run `bindgen`. The FFI text is assembled from checked-in fragments, then checked against the headers that were just compiled. `nghttp2_set_debug_vprintf_callback` is omitted because `va_list` has no portable Rust ABI. The build requires a 64-bit target, `git`, a C compiler, and CMake.

Source resolution order:

1. `NGHTTP2_DIR` — an existing nghttp2 CMake tree (`CMakeLists.txt`, `lib/includes/nghttp2/nghttp2.h`, `lib/CMakeLists.txt`). An empty value is ignored. A non-empty path that is not a source tree fails the build.
2. `vendor/nghttp2` next to the `nghttp2-sys` crate, then `vendor/nghttp2` at the workspace root.
3. A depth-1 clone under the Cargo target directory, staged in a PID-specific directory and renamed into `target/nghttp2-<ref>`.

Features select the git ref used in step 3. `NGHTTP2_DIR` and `vendor/nghttp2` win over both features.

| Feature | Default | Git ref | Cache directory |
| --- | --- | --- | --- |
| `pre-release` | yes | upstream `master` | `target/nghttp2-master` |
| `stable` | no | newest `vMAJOR.MINOR.PATCH` tag | `target/nghttp2-<tag>` |

`stable` asks `git ls-remote --tags` first, then the newest cached `target/nghttp2-v*` tree, then `v1.70.0`. Pre-release tags (`v1.2.3-rc1`) are ignored. If both features are enabled, `stable` is the ref that is cloned. Turn the default off when you want a release tag:

```toml
[dependencies]
nghttp2 = { path = "crates/nghttp2" }
# nghttp2 = { path = "crates/nghttp2", default-features = false, features = ["stable"] }
# nghttp2-macros = { path = "crates/nghttp2-macros" }  # only for #[nghttp2_callbacks]
```

CMake is invoked with `ENABLE_LIB_ONLY=ON`, `ENABLE_STATIC_LIB=ON`, `ENABLE_SHARED_LIB=OFF`, `BUILD_STATIC_LIBS=ON`, `BUILD_SHARED_LIBS=OFF`, `BUILD_TESTING=OFF`, `ENABLE_DOC=OFF`, and `CMAKE_POSITION_INDEPENDENT_CODE=ON`. The static target is compiled with `NGHTTP2_STATICLIB`. The crate links `static=nghttp2` and refuses to finish if `libnghttp2.a` was not installed. Layout of every public value type is checked with C `_Static_assert` against that tree, and again from Rust with `size_of` and `offset_of!`.

Air-gapped builds need `NGHTTP2_DIR` or `vendor/nghttp2`. A populated `target/nghttp2-<ref>` cache is reused and its branch or tag is verified.

## Re-entrant FFI safety

`nghttp2` invokes callbacks on the session thread while a `mem_recv`, `mem_send`, `recv`, or `send` call is still on the stack. A handler often has to submit another frame or consume window from inside that callback. Rebuilding `&mut Session` there would alias the outer mutable borrow.

The split avoids that:

- The owning `Session<Role, H>` holds the `*mut nghttp2_session` and the handler.
- `user_data` is a heap-pinned `CallbackBox<H>`, not the session.
- Each trampoline rebuilds `&mut CallbackBox<H>` from `user_data` and a transient `SessionCtx<'_>` from the `session` pointer argument.
- `SessionCtx` stores `NonNull<nghttp2_session>` plus `PhantomData<&'a mut nghttp2_session>`. The phantom borrow names the native object, which no Rust reference points at. It does not borrow `Session` or `H`.
- The context is neither `Copy` nor `Clone`, and `'a` ends when the callback returns, so safe code cannot store it.

`ctx.submit_*`, `ctx.consume`, `ctx.resume_data`, and the stream queries are therefore defined under Stacked Borrows and Tree Borrows while `&mut H` is live. Call them on `SessionCtx`. Do not reach back into the outer `Session` from a callback.

Panic isolation uses `std::panic::catch_unwind` on all 24 C callbacks an application can implement once the six deprecated `ssize_t` duplicates are excluded. Those 24 are the 23 current fields of `nghttp2_session_callbacks` (`send_callback2`, `recv_callback2`, the frame callbacks, both `on_header_callback` and `on_header_callback2`, both invalid-header callbacks, `select_padding_callback2`, `read_length_callback2`, `on_begin_frame_callback`, `send_data_callback`, `pack_extension_callback2`, `unpack_extension_callback`, `on_extension_chunk_recv_callback`, `error_callback2`, `rand_callback`) plus the DATA provider `read_callback2`. The trait folds `on_header` and `on_header_rcbuf` onto `on_header_callback2`; the rcbuf method forwards to `on_header` unless overridden, and the byte-oriented header slots stay unset. `rand` has no `user_data`; its trampoline is still caught. The deprecated setters (`send_callback`, `recv_callback`, `select_padding_callback`, `data_source_read_length_callback`, `pack_extension_callback`, `error_callback`) stay unset.

On panic the trampoline stores `Box<dyn Any + Send>` in the callback box and returns `NGHTTP2_ERR_CALLBACK_FAILURE` (`-902`). Unwinding never crosses a C frame. When the outer C call returns, the session yields `Error::CallbackPanicked` and must be dropped. A panicked handler has unknown state; do not resume it.

`#[nghttp2_callbacks]` accepts only these method names. A typo fails at compile time instead of leaving a callback unset:

`send`, `recv`, `on_frame_recv`, `on_invalid_frame_recv`, `on_data_chunk_recv`, `before_frame_send`, `on_frame_send`, `on_frame_not_send`, `on_stream_close`, `on_begin_headers`, `on_header`, `on_header_rcbuf`, `on_invalid_header`, `select_padding`, `data_source_read_length`, `on_begin_frame`, `send_data`, `pack_extension`, `unpack_extension`, `on_extension_chunk_recv`, `error_callback2`, `rand`.

Defaults are deliberate. `send` and `recv` return `NGHTTP2_ERR_WOULDBLOCK`. `select_padding` returns the frame length (no padding). `data_source_read_length` returns the largest size the connection window, stream window, and peer `MAX_FRAME_SIZE` allow. `send_data` fails, because it runs only for `NGHTTP2_DATA_FLAG_NO_COPY`. `pack_extension` and `unpack_extension` return `NGHTTP2_ERR_CANCEL`.

## Feature matrix

**Sans-I/O.** `mem_recv(&[u8]) -> Result<usize>` feeds bytes the application already read. `mem_send()` borrows the next serialized chunk. Copy it out before the next send or receive: nghttp2 reuses that buffer. `want_read` and `want_write` report whether another call can make progress. This is the path for a custom poller or a shared-memory ring.

**Callback I/O.** `recv()` and `send()` call the handler's `recv` and `send` methods. Return a positive byte count, or `NGHTTP2_ERR_WOULDBLOCK` when the socket cannot proceed. The session stays synchronous; the handler does the actual read or write.

**`EventCollector`.** `ClientSession` / `ServerSession` record, in callback order: `BeginHeaders`, `Header`, `DataChunk`, `FrameRecv`, `FrameSend`, `StreamClose`, `GoAway`, `Ping`, `SettingsAck`, `WindowUpdate`, and `Error`. A `GOAWAY`, `PING`, `SETTINGS` ACK, or `WINDOW_UPDATE` is only its dedicated event. Other received frames are `FrameRecv`. Drain with `next_event()` after the pump. Extension payloads are delivered to `on_extension_chunk_recv` on a custom handler.

**Submission.** The same methods exist on `Session` and on `SessionCtx`:

| Method | What it queues |
| --- | --- |
| `submit_request` | Client `HEADERS`, optionally followed by `DATA` (`nghttp2_submit_request2`). Returns the new stream id. |
| `submit_response` | Server `HEADERS` plus an optional body. |
| `submit_headers` | A further header block on an open stream. |
| `submit_data` | `DATA` from a buffer or a `DataProvider`. |
| `submit_trailer` | Trailing `HEADERS` with `END_STREAM` (gRPC status). |
| `submit_rst_stream` | `RST_STREAM`. |
| `submit_settings` | `SETTINGS`. |
| `submit_ping` | `PING`. |
| `submit_goaway` | `GOAWAY`. |
| `submit_window_update` | `WINDOW_UPDATE` for a stream or for stream 0. |
| `submit_shutdown_notice` | Graceful shutdown notice. |
| `submit_altsvc` | `ALTSVC`. |
| `submit_origin` | `ORIGIN`. |
| `submit_priority_update` | RFC 9218 `PRIORITY_UPDATE`. |
| `submit_extension` | An extension frame packed by `pack_extension`. |
| `change_extpri` | Local RFC 9218 urgency without a frame of its own. |

**`DataProvider`.** A buffered provider copies from a byte slice. A streaming provider is polled until it reports end of stream. Two flags change that path:

- `NGHTTP2_DATA_FLAG_NO_COPY` skips nghttp2's internal copy (the library's send buffer is 16 KiB). The provider's bytes stay pinned until `on_frame_send`, `on_frame_not_send`, or `on_stream_close`. `send_data` then writes the 9-byte frame header plus those bytes.
- Returning `submit::DEFERRED` (`NGHTTP2_ERR_DEFERRED`) pauses the stream. `resume_data(stream_id)` continues it. The provider is not polled again before that call.

**HPACK.** `HpDeflater` and `HpInflater` are independent of a session. The deflater takes a hard dynamic-table cap; `change_table_size` applies the peer's `SETTINGS_HEADER_TABLE_SIZE` up to that cap. `deflate_bound` sizes the output; `deflate_vec` allocates it. A native encode error poisons that deflater. The inflater streams with `inflate_step` (`final_chunk` on the last slice, then `end_headers` once `finished` is set) or decodes a whole block with `inflate_block`. A decode error is permanent. Both types expose static plus dynamic table size and 1-based entry lookup.

**`SessionOptions`.** Nineteen setters. The session copies them at construction; drop the options immediately afterwards. Passing `true` to a `set_no_*` setter disables that automatic behavior.

| Setter | Effect |
| --- | --- |
| `set_no_auto_window_update` | Application must `consume` received data. |
| `set_peer_max_concurrent_streams` | Assumed peer limit before its `SETTINGS` arrives. |
| `set_no_recv_client_magic` | Skip the client connection preface check. |
| `set_no_http_messaging` | Skip HTTP messaging rules. |
| `set_max_reserved_remote_streams` | Cap on reserved remote streams. |
| `set_user_recv_extension_type` | Extension type the application will unpack. |
| `set_builtin_recv_extension_type` | Extension type nghttp2 understands (`ALTSVC`, `ORIGIN`, `PRIORITY_UPDATE`). |
| `set_no_auto_ping_ack` | Application answers `PING` itself. |
| `set_max_send_header_block_length` | Max outbound uncompressed header block. |
| `set_max_deflate_dynamic_table_size` | Cap on the session HPACK deflater. |
| `set_no_closed_streams` | Do not retain closed streams. |
| `set_max_outbound_ack` | Cap on queued ACK frames. |
| `set_max_settings` | Cap on entries in one `SETTINGS` frame. |
| `set_server_fallback_rfc7540_priorities` | Serve peers that still send RFC 7540 priority. |
| `set_no_rfc9113_leading_and_trailing_ws_validation` | Accept leading or trailing SP/HTAB in values. |
| `set_max_continuations` | Cap on `CONTINUATION` frames per header block. |
| `set_max_outbound_queue_size` | Cap on queued outbound frames. |
| `set_stream_reset_rate_limit` | Token bucket `(burst, rate)` for stream resets. |
| `set_glitch_rate_limit` | Token bucket `(burst, rate)` for protocol glitches. |

**Flow control and stream queries.** `remote_window_size`, `stream_remote_window_size`, `local_window_size`, `stream_local_window_size`, `effective_recv_data_length`, `stream_effective_recv_data_length`, `outbound_queue_size`, `next_stream_id`, `last_proc_stream_id`. Consumption is `consume`, `consume_connection`, `consume_stream`, and `set_local_window_size`. With automatic window updates left on, the library emits `WINDOW_UPDATE` itself. With `set_no_auto_window_update(true)`, received data holds window until `consume`.

**RFC 9218 `ExtPri`.** Urgency is `0..=7` (`URGENCY_HIGH` = 0, `URGENCY_DEFAULT` = 3, `URGENCY_LOW` = 7). `ExtPri::from_header("u=1, i")` parses a Priority header; omitted fields keep their previous values; a parse error leaves the value unchanged. `submit_priority_update` sends the frame. `change_extpri` updates local state.

**RFC 9113 header checks.** `check_header_name` requires a non-empty lowercase token with at most one leading colon. `check_header_value` rejects CR, LF, NUL, and other controls except HTAB and SP. `check_header_value_rfc9113` also rejects leading and trailing SP/HTAB. `check_authority` allows URI authority bytes and rejects SP, `/`, `?`, and `#`. `HeaderField::validate` uses the receive rules. `HeaderField::validate_rfc9113` adds the authority userinfo ban (`@` in `:authority` or `host`) and the edge-whitespace rule. `nv!` does not validate, so constants such as `METHOD_POST` can be built in a const context. Validate before submit.

**`RcBuf`.** `on_header_rcbuf` borrows name and value for the callback. `RcBuf::clone` (and `from_raw`) increments the native count; `Drop` decrements it once. `is_static` buffers ignore the count and live for the process. Dynamic buffers free when the last reference drops. The count is not atomic.

**ALPN.** `types::select_next_protocol` reads a length-prefixed protocol list and prefers `h2` over `http/1.1`. `AlpnProtocol::Http2` and `AlpnProtocol::Http11` are the only successes. Oversized lists return `None` instead of wrapping the C parser's length arithmetic. `version_info()` reports the linked library's version and `proto_str`.

**`CustomAllocator`.** `CustomAllocator::new` and `with_limit` install `malloc` / `free` / `calloc` / `realloc` over a boxed `nghttp2_mem`. The table and the tracking `State` are heap allocations, so moving the owner does not move the addresses nghttp2 stored. HPACK retains `context->mem` rather than copying the table; sessions do the same. Destroy every native user before dropping the allocator. `stats()` reports `live_bytes`, `peak_bytes`, `live_blocks`, and `failed_allocations` for the built-in tracker. Lowering the limit rejects later requests and does not free existing blocks. `from_raw` wraps an external table and does not account it. Pass `as_raw()` to `Session::client` / `Session::server` or to `HpDeflater::new_with_mem` / `HpInflater::new_with_mem`.

## Usage

Add the high-level crate as a path dependency (the workspace is `publish = false`). Names in `nv!` must already be lowercase: `NGHTTP2_NV_FLAG_NO_COPY_NAME` skips nghttp2's downcasing. The bytes live in the binary. `no_index` also sets `NGHTTP2_NV_FLAG_NO_INDEX`.

```rust
use nghttp2::nv::{nv, HeaderField, METHOD_POST};

let request: [HeaderField<'static>; 4] = [
    METHOD_POST,
    nv!(":scheme", "https"),
    nv!(":authority", "example.com"),
    nv!(":path", "/rpc"),
];
let sensitive = nv!("authorization", "Bearer secret", no_index);
request[0].validate_rfc9113().expect("static pseudo-header");
assert!(sensitive.flags().contains(nghttp2::nv::NvFlags::NO_INDEX));
```

Standalone HPACK, outside any session:

```rust
use nghttp2::hpack::{HpDeflater, HpInflater, HpackHeader};

let headers = [
    HpackHeader::new(b":status", b"200"),
    HpackHeader::new(b"grpc-status", b"0").with_no_index(true),
];
let mut deflater = HpDeflater::new(4096).expect("deflater");
let block = deflater.deflate_vec(&headers).expect("block");
let decoded = HpInflater::new().expect("inflater").inflate_block(&block).expect("headers");
assert_eq!(decoded[0].name, b":status");
```

Options, RFC 9218 priority, and ALPN:

```rust
use nghttp2::option::SessionOptions;
use nghttp2::priority::ExtPri;
use nghttp2::types::{select_next_protocol, AlpnProtocol};

let mut options = SessionOptions::new().expect("native options");
options
    .set_peer_max_concurrent_streams(100)
    .set_no_auto_window_update(true)
    .set_max_send_header_block_length(64 * 1024)
    .set_max_deflate_dynamic_table_size(4096)
    .set_stream_reset_rate_limit(1_000, 33)
    .set_glitch_rate_limit(1_000, 33);
let priority = ExtPri::from_header("u=1, i").expect("priority header");
assert_eq!(priority.urgency(), 1);
assert!(priority.incremental());

let offered = b"\x02h2\x08http/1.1";
assert_eq!(select_next_protocol(offered), Some(AlpnProtocol::Http2));
```

`SessionOptions::new` returns `Result<Self, RawErrorCode>` (`0` is success; the `Err` value is the native code). `ExtPri::from_header` returns `NGHTTP2_ERR_INVALID_ARGUMENT` for a malformed Priority header.

A tracked allocator must outlive every session and HPACK context that borrowed it. After those users drop, `live_blocks` is zero when the library freed what it allocated:

```rust
use nghttp2::hpack::HpDeflater;
use nghttp2::mem::CustomAllocator;

let allocator = CustomAllocator::with_limit(1 << 20);
let mut deflater = unsafe { HpDeflater::new_with_mem(4096, allocator.as_raw()) }.expect("deflater");
let _ = deflater
    .deflate_vec(&[nghttp2::hpack::HpackHeader::new(b":method", b"GET")])
    .expect("deflate");
drop(deflater);
assert_eq!(allocator.stats().map(|stats| stats.live_blocks), Some(0));
```

`new_with_mem` is unsafe because the caller keeps the table and its userdata alive until the native object is destroyed. The built-in `CustomAllocator` satisfies that as long as it is dropped last.

Client and server sessions over memory. `mem_send` borrows the library buffer, so the chunk is copied before the peer consumes it. The server answers from `on_frame_recv` through `SessionCtx`, not through the outer `&mut Session`. The types in this sample are the wrapper contract; this crate does not export `Session`, `SessionHandler`, `EventCollector`, or `DataProvider` yet.

```rust
use nghttp2::callbacks::SessionHandler;
use nghttp2::context::SessionCtx;
use nghttp2::event::{EventCollector, SessionEvent};
use nghttp2::mem::CustomAllocator;
use nghttp2::nv::nv;
use nghttp2::option::SessionOptions;
use nghttp2::session::{ClientSession, Server, Session};
use nghttp2::submit::DataProvider;
use nghttp2::sys::nghttp2_frame;

struct ServerApp;

#[nghttp2_macros::nghttp2_callbacks]
impl SessionHandler for ServerApp {
    fn on_frame_recv(&mut self, ctx: &mut SessionCtx<'_>, frame: &nghttp2_frame) {
        // `hd` is the common prefix of every union member.
        let stream_id = unsafe { frame.hd.stream_id };
        if stream_id == 0 {
            return;
        }
        let _ = ctx.submit_response(
            stream_id,
            &[nv!(":status", "200")],
            Some(DataProvider::bytes(b"pong")),
        );
    }
}

fn exchange() -> nghttp2::error::Result<()> {
    let allocator = CustomAllocator::new();
    let mut options = SessionOptions::new().map_err(nghttp2::error::Error::from_native)?;
    options
        .set_peer_max_concurrent_streams(100)
        .set_no_auto_window_update(true);

    let mut client = ClientSession::client(
        EventCollector::new(),
        Some(&options),
        Some(allocator.as_raw()),
    )?;
    let mut server = Session::<Server, ServerApp>::server(
        ServerApp,
        Some(&options),
        Some(allocator.as_raw()),
    )?;

    let headers = [
        nv!(":method", "POST"),
        nv!(":scheme", "https"),
        nv!(":authority", "example.com"),
        nv!(":path", "/rpc"),
        nv!("content-type", "application/grpc"),
    ];
    // `DataProvider::deferred` returns `submit::DEFERRED` until `resume_data`.
    let stream_id = client.submit_request(&headers, Some(DataProvider::deferred()))?;
    client.resume_data(stream_id)?;
    client.submit_trailer(stream_id, &[nv!("grpc-status", "0")])?;
    let priority = nghttp2::priority::ExtPri::new(0, true)
        .map_err(nghttp2::error::Error::from_native)?;
    client.submit_priority_update(stream_id, priority)?;

    let mut forward = Vec::new();
    while client.want_write() {
        let chunk = client.mem_send()?;
        if chunk.is_empty() {
            break;
        }
        forward.extend_from_slice(chunk);
    }
    server.mem_recv(&forward)?;

    forward.clear();
    while server.want_write() {
        let chunk = server.mem_send()?;
        if chunk.is_empty() {
            break;
        }
        forward.extend_from_slice(chunk);
    }
    client.mem_recv(&forward)?;

    while let Some(event) = client.next_event() {
        if let SessionEvent::DataChunk { data, .. } = event {
            assert_eq!(data, b"pong");
        }
    }
    client.consume(stream_id, 4)?;
    Ok(())
}
```

`submit::DEFERRED` (`NGHTTP2_ERR_DEFERRED`) pauses a provider. `Error::kind` reports `ErrorKind::Temporal` for `WOULDBLOCK`, `DEFERRED`, and `PAUSE`. Codes strictly below `NGHTTP2_ERR_FATAL` are `ErrorKind::Fatal`; drop that session.

Callback I/O uses the same session types. Implement `send` and `recv` on the handler, then call `session.send()` and `session.recv()` when the socket is ready. A positive return is a byte count. `NGHTTP2_ERR_WOULDBLOCK` means the socket buffer is empty or full. Override `on_header_rcbuf` and `clone` the `RcBuf` to keep a header after the callback; static buffers ignore the reference count.

## Development

The toolchain file pins nightly and includes rustfmt, clippy, and `rust-src`. From the workspace root:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash .github/scripts/ci-incremental.sh
```

## License

MIT OR Apache-2.0. Upstream nghttp2 is distributed under its own MIT license; the static build compiles that source and does not vendor it into this repository.
