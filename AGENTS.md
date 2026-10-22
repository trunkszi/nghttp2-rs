# AGENTS.md

Operational contract for coding agents (OpenAI Codex, Claude Code, Gemini CLI) working in `nghttp2-rs`. Treat this file as the authoritative source for repository architecture, build commands, verification gates, and coding invariants.

## Project Overview

`nghttp2-rs` provides safe, synchronous, single-threaded Rust 2024 bindings for upstream C [nghttp2](https://github.com/nghttp2/nghttp2). The high-level crate exposes sans-I/O and callback-driven `Session<Client, H>` / `Session<Server, H>` state machines, standalone `HpDeflater` / `HpInflater` HPACK engines, RFC 9113 header validation, RFC 9218 extensible priorities, ALPN helpers, `RcBuf`, and `CustomAllocator`.

Protocol state is driven on the calling thread; socket I/O, TLS handshakes, and event-loop scheduling belong to the caller.

## Workspace Layout

```text
nghttp2-rs/
├── Cargo.toml                        # Workspace members, dependencies, lints, profiles
├── Cargo.lock                        # Locked dependency graph
├── LICENSE                           # MIT License (upstream nghttp2)
├── README.md                         # User-facing architecture and API documentation
├── rust-toolchain.toml               # Pinned nightly toolchain and components
├── rustfmt.toml                      # Workspace formatting configuration
├── crates/
│   ├── nghttp2-sys/                  # L1: native source resolution, static CMake build, FFI + ABI assertions
│   ├── nghttp2-macros/               # L2: compile-time `nv!` and `#[nghttp2_callbacks]` proc-macro
│   └── nghttp2/                      # L3: safe sessions, HPACK, priorities, allocators, examples, benchmarks
│       ├── examples/h2_client_server.rs
│       └── benches/h2_bench.rs
└── .github/
    ├── scripts/ci-incremental.sh     # Incremental CI and architectural invariant check
    └── workflows/ci.yml              # GitHub Actions workflow
```

## Crate Layering & Native Build

| Layer | Crate | Responsibilities & Dependency Boundaries |
| --- | --- | --- |
| L1 | `nghttp2-sys` | Raw FFI declarations, C/Rust ABI layout checks, and static CMake build of `libnghttp2.a` (`cmake`, `cc`). |
| L2 | `nghttp2-macros` | `nv!` and `#[nghttp2_callbacks]` (`syn`, `quote`, `proc-macro2`); does not depend on `nghttp2`. |
| L3 | `nghttp2` | Depends on L1 and L2; owns `Session`, `SessionCtx`, `DataProvider`, `RcBuf`, `CustomAllocator`, HPACK, RFC 9113 validation, ALPN helpers, examples, and benchmarks. |

`crates/nghttp2-sys/build.rs` resolves the upstream C source tree in this order:
1. `NGHTTP2_DIR` environment variable (must point to a valid nghttp2 CMake tree when non-empty).
2. `vendor/nghttp2` inside `crates/nghttp2-sys/` or at the workspace root.
3. Shallow clone cached under `target/nghttp2-<ref>`.

## Features

- `default = ["pre-release"]`: tracks upstream `master` (`target/nghttp2-master`).
- `stable`: selects the newest `vMAJOR.MINOR.PATCH` release tag (falling back to cached `target/nghttp2-v*` or `v1.70.0` when offline). Disable default features (`--no-default-features --features stable`) when testing `stable`.
- `nghttp2` forwards both features to `nghttp2-sys`. `nghttp2-macros` is version-independent.

## Build, Lint, and Test Commands

Run all commands from the workspace root:

| Command | Purpose |
| --- | --- |
| `cargo fmt --all -- --check` | Check formatting across all workspace crates |
| `cargo clippy --workspace --all-targets -- -D warnings` | Strict lint gate across library, test, example, and bench targets |
| `cargo test --workspace` | Run unit, integration, and doc tests with default (`pre-release`) feature |
| `cargo test --workspace --no-default-features --features stable` | Run unit, integration, and doc tests against the `stable` release tag |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | Build rustdoc with warnings denied |
| `cargo run --example h2_client_server` | Run the in-memory HTTP/2 request/response/trailer exchange example |
| `cargo bench --bench h2_bench` | Run HPACK and full-session exchange benchmarks |
| `bash .github/scripts/ci-incremental.sh` | Run the full incremental CI gate (line limits, zero-async check, fmt, clippy, default + stable tests) |

Before completing any code change, run `bash .github/scripts/ci-incremental.sh` and `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`. When session or transport APIs change, also run `cargo run --example h2_client_server`.

## Hard Invariants & Coding Standards

- **Platform & Toolchain**: Linux `x86_64`, Rust edition `2024`, pinned nightly toolchain in `rust-toolchain.toml`.
- **Strictly Synchronous & Single-Threaded**: Zero async runtime or future-executor dependencies (`tokio*`, `futures*`, `async*`, `smol*` are rejected by CI). Sessions, HPACK contexts, `RcBuf`, and `CustomAllocator` remain on their creating thread (`!Send + !Sync`).
- **File Size Limit**: Every tracked `.rs`, `.c`, `.cpp`, `.h`, and `.hpp` file must be `<= 800` lines (enforced by `.github/scripts/ci-incremental.sh`). Split modules or tests by responsibility before reaching the limit.
- **Manifest Conventions**: Declare shared dependencies in root `[workspace.dependencies]` using inline table syntax `{ ... }` and inherit with `{ workspace = true }` in member crates. Preserve root `[workspace.lints]` and `[profile.*]` settings.
- **Error Handling**: Library error types derive `thiserror::Error` and preserve native error codes and recovery classification (`ErrorKind::Temporal` vs. `ErrorKind::Fatal`). Examples, benchmarks, and fallible test entry points use `anyhow::Result`.
- **Safety & Documentation**:
  - Every `unsafe` block requires a preceding `// SAFETY:` comment explaining pointer validity, lifetime, aliasing, initialization, or ownership invariants.
  - Every `unsafe fn` requires a `# Safety` rustdoc section; fallible or panicking public functions require `# Errors` / `# Panics` sections (enforced by clippy).
  - All repository text—source comments, rustdoc, error messages, logs, and documentation—must be written in English.

## FFI Lifetimes & Callback Contract

- Keep callback state, `DataProvider` sources, and `CustomAllocator` resources alive until native session destruction completes.
- `SessionCtx` is valid only for the duration of the active synchronous callback and must never reconstruct a mutable reference to the outer `Session` or `H`.
- Outbound wire slices from `mem_send()` borrow nghttp2's internal buffer and must be copied or consumed before the next session mutation.
- `NGHTTP2_DATA_FLAG_NO_COPY` payloads must remain valid until `on_frame_send`, `on_frame_not_send`, or `on_stream_close`.
- Callback panics (`catch_unwind`) and fatal native errors (`< NGHTTP2_ERR_FATAL`) permanently poison the session; callers must drop the session rather than resuming it.
- When modifying FFI declarations, header checks, or upstream version selection, verify both default (`pre-release`) and `stable` feature builds.

## Change Hygiene

- Inspect `git status` and `git diff` before and after edits; keep diffs minimal and focused.
- Do not commit, push, reset, or rewrite history unless explicitly instructed by the user.
- Update `AGENTS.md` and `README.md` in the same change whenever crate names, paths, features, commands, or public API contracts change.
