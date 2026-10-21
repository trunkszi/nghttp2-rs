# nghttp2-rs

Safe, single-threaded Rust bindings and a high-level wrapper for upstream
[nghttp2](https://github.com/nghttp2/nghttp2.git).

## Workspace

| Crate | Purpose |
|---|---|
| `nghttp2-sys` | Resolves and statically compiles upstream `nghttp2`, then exposes the complete C ABI |
| `nghttp2-macros` | Compile-time descriptors such as `nv!` |
| `nghttp2` | Safe client and server sessions, HPACK, options, and priorities |

## Upstream source

`nghttp2-sys` resolves the C library in this order:

1. `NGHTTP2_DIR`, an existing nghttp2 source checkout;
2. `vendor/nghttp2` next to the crate or at the workspace root;
3. a `git clone` cached under `target/nghttp2-<ref>`.

The `pre-release` feature (default) tracks `master`; `stable` resolves the newest
`v*` release tag. Both compile `libnghttp2` statically with CMake, so no system
`libnghttp2` or `libclang` installation is required.

## Development

```bash
cargo fmt --all
bash .github/scripts/ci-incremental.sh
```

## License

MIT OR Apache-2.0.
