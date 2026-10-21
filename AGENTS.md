# AGENTS.md

nghttp2-rs 项目指南。本文件是代理在本仓库工作的操作契约，也是命令、门禁和约束的唯一权威来源。`CLAUDE.md` 只导入本文件。

## 项目定位

nghttp2-rs 为上游 C `nghttp2` 提供安全、同步、低开销的 Rust 2024 绑定。高层接口包括 sans-I/O `Session<Client, H>`、`Session<Server, H>`，以及独立的 `HpDeflater` / `HpInflater` HPACK 引擎，作为 grpc-rs 的 HTTP/2 基础。

协议状态由调用线程推进；套接字读写、TLS 和调度由调用方提供。ALPN 选择辅助函数不构成 TLS 传输实现。

## 当前工作区

```text
nghttp2-rs/
  Cargo.toml                    # workspace members, dependencies, lints, profiles
  Cargo.lock                    # resolved dependency versions
  rust-toolchain.toml           # nightly toolchain and components
  crates/nghttp2-sys/            # native source resolution, CMake build, FFI
  crates/nghttp2-macros/         # compile-time header and callback macros
  crates/nghttp2/                # safe sessions and standalone protocol helpers
    examples/h2_client_server.rs # in-memory request, response, DATA and trailers
    benches/h2_bench.rs          # HPACK and complete session exchange benchmarks
  .github/                      # CI workflows and incremental gate scripts
```

## 包分层

| 层级 | Crate | 职责与依赖边界 |
|---|---|---|
| L1 | `nghttp2-sys` | FFI 声明、C/Rust ABI 校验和上游库的静态 CMake 构建；构建依赖为 `cmake`、`cc` |
| L2 | `nghttp2-macros` | `nv!` 与 `#[nghttp2_callbacks]`；使用 `syn`、`quote`、`proc-macro2`，不依赖高层 crate，展开代码引用其公开接口 |
| L3 | `nghttp2` | 依赖 L1、L2；拥有 `Session`、`SessionCtx`、`DataProvider`、`RcBuf`、`CustomAllocator`、HPACK、RFC 9113 校验、ALPN 辅助函数及示例和基准 |

原生源码解析由 `crates/nghttp2-sys/build.rs` 负责，顺序为 `NGHTTP2_DIR`、crate 或根目录的 `vendor/nghttp2`、`target/` 中的源码缓存；缺少缓存时获取所选上游分支或标签。无效的显式目录必须报错。

## Feature

- `default = ["pre-release"]` 跟踪上游 `master`。
- `stable` 选择最新正式 `v*` 发布标签；离线时使用构建脚本的缓存和回退规则。验证 stable 时关闭默认 feature。
- `nghttp2` 将这两个 feature 转发给 `nghttp2-sys`。宏 crate 不选择原生库版本。
- `NGHTTP2_DIR` 优先指定 C 源码树，适用于离线和可复现构建。使用该覆盖时记录实际版本或提交；feature 名称不能证明指定源码的版本。

## 命令

从工作区根目录执行。下表是这些命令的唯一定义位置。

| 命令 | 用途 |
|---|---|
| `cargo fmt --all -- --check` | 格式门禁 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 全目标严格 lint |
| `cargo test --workspace` | 默认 feature 的测试和 doctest |
| `cargo test --workspace --no-default-features --features stable` | stable feature 的测试和 doctest |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | 严格 rustdoc 门禁 |
| `cargo run --example h2_client_server` | 内存传输示例；校验请求、响应、DATA、trailer 和关闭结果 |
| `cargo bench --bench h2_bench` | HPACK 与含创建、握手、交换、销毁的会话基准 |

代码变更完成前执行格式、lint、默认和 stable 测试、rustdoc 门禁。会话或传输接口变更同时运行示例；性能变更运行基准并说明测量边界。纯文档变更核对命令、路径与实现的一致性，并检查差异，无需启动 Rust 构建。无法执行的门禁须报告具体前置条件，不计为通过。

## 硬约束

- 支持平台为 Linux x86_64；Rust edition 为 2024，工具链使用 `rust-toolchain.toml` 指定的 nightly。
- 保持严格同步，不引入 async、Future 执行器或异步运行时依赖。会话、HPACK 上下文、缓冲区引用和分配器留在其创建线程；原生句柄包装保持适用的 `!Send + !Sync` 约束。
- 每个 `.rs`、`.c`、`.h` 文件不超过 800 行；超限时按职责拆分实现或测试。
- 所有 `Cargo.toml` 中的依赖使用 `{ ... }` 内联表语法；共享依赖在根 `[workspace.dependencies]` 声明，成员通过 `{ workspace = true }` 继承。保持根 lint 和 profile 约束。
- 库错误类型派生 `thiserror::Error`，保留原生错误码、恢复分类和稳定的显示文案。示例、基准及可失败测试入口使用 `anyhow::Result` 添加操作上下文；需要精确匹配的原生结果保留其类型。
- 每个 `unsafe` 块带 `// SAFETY:`，说明指针、生命周期、别名、初始化或所有权不变量；每个 `unsafe fn` 记录 `# Safety`。可失败或会 panic 的公开函数分别记录 `# Errors` / `# Panics`。
- 本指南使用中文；源码注释、rustdoc、错误消息、日志和提交消息使用英文。临时笔记保持本地、不纳入版本控制；已提交文档只引用版本化路径。

## 生命周期与接口

- 原生会话删除完成前保留回调、DATA provider 和分配器资源。`SessionCtx` 只用于当前同步回调，不重建指向外层属主或 handler 的可变引用。
- 发送缓冲区和流视图的借用不得跨越会使其失效的会话操作。`NO_COPY` DATA 必须满足 provider 与传输回调的存储期限。
- 保留的 `RcBuf` 可以晚于会话释放；内建分配器跟踪资源直到最后一个原生分配释放，外部分配器的资源期限由其 unsafe 契约约束。
- 回调 panic 和原生致命错误使会话不可恢复；所有会话和回调上下文的变更接口保持一致的失败锁存行为。
- 修改 FFI 声明、头文件解析或原生版本选择时，同步核对 C/Rust 布局、符号覆盖和两个原生版本的行为，不将单一 feature 的通过当作完整验证。

## 工作范围与维护

修改前检查已暂存与未暂存差异，保留其他工作。仅按用户授权提交，不擅自推送、合并或执行破坏性的 Git 操作。

完成前检查最终变更范围、差异空白和源码行数。包名、路径、feature、命令或所有权契约变化时，在同次变更中同步本文件；`CLAUDE.md` 保持单行导入，不复制规则。
