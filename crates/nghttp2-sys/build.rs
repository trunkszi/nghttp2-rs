//! Resolves the upstream `nghttp2` source tree, builds `libnghttp2` as a static C
//! library with CMake, and assembles the checked-in FFI declarations into
//! `$OUT_DIR/bindings.rs`.
//!
//! The bindings are assembled from text fragments instead of being generated with
//! `bindgen` at build time, so consumers do not need a `libclang` installation that
//! matches their C toolchain. The build script still parses the resolved headers and
//! verifies that every public function, type, and macro the source tree declares is
//! present in the assembled declarations.

use std::{
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const REPOSITORY: &str = "https://github.com/nghttp2/nghttp2.git";
const DEFAULT_BRANCH: &str = "master";
const FALLBACK_TAG: &str = "v1.70.0";
const PUBLIC_HEADER: &str = "lib/includes/nghttp2/nghttp2.h";
const VERSION_HEADER: &str = "lib/includes/nghttp2/nghttp2ver.h.in";

/// Functions excluded because their `va_list` parameter has no portable Rust ABI.
const BLOCKLISTED_FUNCTIONS: &[&str] = &["nghttp2_set_debug_vprintf_callback"];

/// Callback types excluded for the same reason as [`BLOCKLISTED_FUNCTIONS`].
const BLOCKLISTED_TYPES: &[&str] = &["nghttp2_debug_vprintf_callback"];

/// Include guard, which is not part of the public ABI.
const INCLUDE_GUARD_MACRO: &str = "H";

/// Version macros supplied from the resolved source tree instead of the fragments.
const GENERATED_VERSION_MACROS: &[&str] = &["VERSION", "VERSION_NUM"];

type StructLayout = (&'static str, usize, &'static [(&'static str, usize)]);

/// Expected layout of every public value type in `nghttp2.h` on a 64-bit target.
///
/// `build.rs` compiles these as C `_Static_assert`s against the resolved headers,
/// so an upstream layout change fails the build instead of silently corrupting the
/// ABI of the checked-in declarations.
#[rustfmt::skip]
const ABI_LAYOUT: &[StructLayout] = &[
    ("nghttp2_info", 24, &[("age", 0), ("version_num", 4), ("version_str", 8), ("proto_str", 16)]),
    ("nghttp2_vec", 16, &[("base", 0), ("len", 8)]),
    ("nghttp2_nv", 40, &[("name", 0), ("value", 8), ("namelen", 16), ("valuelen", 24), ("flags", 32)]),
    ("nghttp2_frame_hd", 16, &[("length", 0), ("stream_id", 8), ("type", 12), ("flags", 13), ("reserved", 14)]),
    ("nghttp2_data_source", 8, &[("fd", 0), ("ptr", 0)]),
    ("nghttp2_data_provider", 16, &[("source", 0), ("read_callback", 8)]),
    ("nghttp2_data_provider2", 16, &[("source", 0), ("read_callback", 8)]),
    ("nghttp2_data", 24, &[("hd", 0), ("padlen", 16)]),
    ("nghttp2_priority_spec", 12, &[("stream_id", 0), ("weight", 4), ("exclusive", 8)]),
    ("nghttp2_headers", 64, &[("hd", 0), ("padlen", 16), ("pri_spec", 24), ("nva", 40), ("nvlen", 48), ("cat", 56)]),
    ("nghttp2_priority", 32, &[("hd", 0), ("pri_spec", 16)]),
    ("nghttp2_rst_stream", 24, &[("hd", 0), ("error_code", 16)]),
    ("nghttp2_settings_entry", 8, &[("settings_id", 0), ("value", 4)]),
    ("nghttp2_settings", 32, &[("hd", 0), ("niv", 16), ("iv", 24)]),
    ("nghttp2_push_promise", 48, &[("hd", 0), ("padlen", 16), ("nva", 24), ("nvlen", 32), ("promised_stream_id", 40), ("reserved", 44)]),
    ("nghttp2_ping", 24, &[("hd", 0), ("opaque_data", 16)]),
    ("nghttp2_goaway", 48, &[("hd", 0), ("last_stream_id", 16), ("error_code", 20), ("opaque_data", 24), ("opaque_data_len", 32), ("reserved", 40)]),
    ("nghttp2_window_update", 24, &[("hd", 0), ("window_size_increment", 16), ("reserved", 20)]),
    ("nghttp2_extension", 24, &[("hd", 0), ("payload", 16)]),
    ("nghttp2_frame", 64, &[("hd", 0), ("data", 0), ("headers", 0), ("priority", 0), ("rst_stream", 0), ("settings", 0), ("push_promise", 0), ("ping", 0), ("goaway", 0), ("window_update", 0), ("ext", 0)]),
    ("nghttp2_extpri", 8, &[("urgency", 0), ("inc", 4)]),
    ("nghttp2_ext_altsvc", 32, &[("origin", 0), ("origin_len", 8), ("field_value", 16), ("field_value_len", 24)]),
    ("nghttp2_origin_entry", 16, &[("origin", 0), ("origin_len", 8)]),
    ("nghttp2_ext_origin", 16, &[("nov", 0), ("ov", 8)]),
    ("nghttp2_ext_priority_update", 24, &[("stream_id", 0), ("field_value", 8), ("field_value_len", 16)]),
    ("nghttp2_mem", 40, &[("mem_user_data", 0), ("malloc", 8), ("free", 16), ("calloc", 24), ("realloc", 32)]),
];

fn is_source_tree(path: &Path) -> bool {
    path.join("CMakeLists.txt").is_file()
        && path.join(PUBLIC_HEADER).is_file()
        && path.join("lib/CMakeLists.txt").is_file()
}

fn target_root(manifest: &Path, out_dir: &Path) -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                manifest.join("../..").join(path)
            }
        })
        .or_else(|| {
            out_dir
                .ancestors()
                .find(|path| path.file_name().is_some_and(|name| name == "build"))
                .and_then(Path::parent)
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| out_dir.to_path_buf())
}

fn parse_semver_tag(tag: &str) -> Option<(u32, u32, u32)> {
    let rest = tag.strip_prefix('v')?;
    if rest.contains('-') {
        return None;
    }
    let mut parts = rest.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn resolve_latest_release_tag(target_dir: &Path) -> String {
    // Ask the remote first so `stable` follows new releases; the cache scan below
    // keeps offline builds working once a tag has been fetched.
    if let Ok(output) = Command::new("git")
        .args(["ls-remote", "--tags", "--refs", REPOSITORY, "v*"])
        .output()
        && output.status.success()
        && let Ok(stdout) = String::from_utf8(output.stdout)
    {
        let mut tags: Vec<(u32, u32, u32, String)> = stdout
            .lines()
            .filter_map(|line| {
                let ref_name = line.split_whitespace().nth(1)?;
                let tag = ref_name.strip_prefix("refs/tags/")?;
                let (maj, min, pat) = parse_semver_tag(tag)?;
                Some((maj, min, pat, tag.to_owned()))
            })
            .collect();
        tags.sort();
        if let Some((_, _, _, tag)) = tags.pop() {
            return tag;
        }
    }

    if let Ok(entries) = fs::read_dir(target_dir) {
        let mut cached: Vec<(u32, u32, u32, String)> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if !is_source_tree(&path) {
                    return None;
                }
                let name = entry.file_name().into_string().ok()?;
                let tag = name.strip_prefix("nghttp2-")?;
                let (maj, min, pat) = parse_semver_tag(tag)?;
                Some((maj, min, pat, tag.to_owned()))
            })
            .collect();
        cached.sort();
        if let Some((_, _, _, tag)) = cached.pop() {
            return tag;
        }
    }

    FALLBACK_TAG.to_owned()
}

fn verify_cached_tag(source: &Path, expected_tag: &str) {
    if source.join(".git").exists() {
        let output = Command::new("git")
            .arg("-C")
            .arg(source)
            .args(["describe", "--tags", "--exact-match", "HEAD"])
            .output()
            .expect("verify nghttp2 tag");
        assert!(
            output.status.success(),
            "failed to describe nghttp2 tag at {}",
            source.display()
        );
        let actual = String::from_utf8(output.stdout)
            .expect("UTF-8 tag")
            .trim()
            .to_owned();
        assert_eq!(
            actual,
            expected_tag,
            "nghttp2 cached tag mismatch at {}",
            source.display()
        );
    }
}

fn verify_cached_branch(source: &Path, expected_branch: &str) {
    if source.join(".git").exists() {
        let output = Command::new("git")
            .arg("-C")
            .arg(source)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("verify nghttp2 branch");
        assert!(
            output.status.success(),
            "failed to verify nghttp2 branch at {}",
            source.display()
        );
        let actual = String::from_utf8(output.stdout)
            .expect("UTF-8 branch")
            .trim()
            .to_owned();
        assert_eq!(
            actual,
            expected_branch,
            "nghttp2 cached branch mismatch at {}",
            source.display()
        );
    }
}

fn clone_into_target(target_dir: &Path, git_ref: &str, verify_tag: bool) -> PathBuf {
    let cache = target_dir.join(format!("nghttp2-{git_ref}"));
    if is_source_tree(&cache) {
        if verify_tag {
            verify_cached_tag(&cache, git_ref);
        } else {
            verify_cached_branch(&cache, git_ref);
        }
        return cache;
    }

    let staging = target_dir.join(format!("nghttp2-{git_ref}.fetch-{}", std::process::id()));
    drop(fs::remove_dir_all(&staging));
    let status = Command::new("git")
        .args(["clone", "--depth", "1", "--branch", git_ref, REPOSITORY])
        .arg(&staging)
        .status()
        .expect("run git clone for nghttp2");
    assert!(
        status.success(),
        "failed to clone nghttp2 ref {git_ref} from {REPOSITORY}"
    );
    if verify_tag {
        verify_cached_tag(&staging, git_ref);
    } else {
        verify_cached_branch(&staging, git_ref);
    }
    if let Err(err) = fs::rename(&staging, &cache) {
        assert!(
            is_source_tree(&cache),
            "failed to move staged nghttp2 clone into {}: {err}",
            cache.display()
        );
        drop(fs::remove_dir_all(&staging));
    }
    if verify_tag {
        verify_cached_tag(&cache, git_ref);
    } else {
        verify_cached_branch(&cache, git_ref);
    }
    cache
}

fn resolve_source(manifest: &Path, out_dir: &Path) -> PathBuf {
    if let Ok(configured) = env::var("NGHTTP2_DIR") {
        let trimmed = configured.trim();
        if !trimmed.is_empty() {
            let candidate = PathBuf::from(trimmed);
            if is_source_tree(&candidate) {
                return candidate;
            }
            panic!(
                "NGHTTP2_DIR={} does not point to a valid nghttp2 CMake source tree",
                candidate.display()
            );
        }
    }

    for vendor in [
        manifest.join("vendor/nghttp2"),
        manifest.join("../../vendor/nghttp2"),
    ] {
        if is_source_tree(&vendor) {
            return vendor;
        }
    }

    let target_dir = target_root(manifest, out_dir);
    fs::create_dir_all(&target_dir).expect("create target directory");
    if cfg!(feature = "stable") {
        let tag = resolve_latest_release_tag(&target_dir);
        clone_into_target(&target_dir, &tag, true)
    } else {
        clone_into_target(&target_dir, DEFAULT_BRANCH, false)
    }
}

fn build_static_library(source: &Path) -> PathBuf {
    // Upstream gates the two library targets on BUILD_SHARED_LIBS/BUILD_STATIC_LIBS.
    // The ENABLE_* spellings are the interface this crate promises callers and are
    // kept in the cache for tooling that inspects build configuration.
    cmake::Config::new(source)
        .define("ENABLE_LIB_ONLY", "ON")
        .define("ENABLE_STATIC_LIB", "ON")
        .define("ENABLE_SHARED_LIB", "OFF")
        .define("BUILD_STATIC_LIBS", "ON")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("BUILD_TESTING", "OFF")
        .define("ENABLE_DOC", "OFF")
        .define("CMAKE_POSITION_INDEPENDENT_CODE", "ON")
        .define("CMAKE_INSTALL_LIBDIR", "lib")
        .profile("Release")
        .build()
}

fn parse_integer_literal(value: &str) -> Option<i128> {
    let value = value.trim().trim_end_matches(',');
    let (negative, digits) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value),
    };
    let digits = digits.replace('_', "");
    let magnitude = match digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Some(hex) => i128::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<i128>().ok()?,
    };
    Some(if negative { -magnitude } else { magnitude })
}

fn binding_constant<'a>(bindings: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("pub const {name}:");
    let start = bindings.find(&marker)? + marker.len();
    let rest = &bindings[start..];
    let equals = rest.find('=')?;
    let end = rest[equals..].find(';')?;
    Some(rest[equals + 1..equals + end].trim())
}

fn append_constant_assert(c_source: &mut String, bindings: &str, name: &str) {
    let literal = binding_constant(bindings, name)
        .unwrap_or_else(|| panic!("{name} is missing from the FFI fragments"));
    let value = parse_integer_literal(literal)
        .unwrap_or_else(|| panic!("{name} does not have a numeric value: {literal}"));
    writeln!(
        c_source,
        "_Static_assert({name} == {value}, \"{name} value\");"
    )
    .expect("write C constant assertion");
}

fn append_enum_asserts(source: &Path, bindings: &str, c_source: &mut String) {
    let text = fs::read_to_string(source.join(PUBLIC_HEADER))
        .unwrap_or_else(|err| panic!("read {PUBLIC_HEADER}: {err}"));
    let mut in_enum = false;
    let mut next_value = 0_i128;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("typedef enum {") {
            in_enum = true;
            next_value = 0;
            continue;
        }
        if in_enum && trimmed.starts_with('}') {
            in_enum = false;
            continue;
        }
        if !in_enum || !trimmed.starts_with("NGHTTP2_") {
            continue;
        }
        let (name, header_value) = match trimmed.split_once('=') {
            Some((name, value)) => (
                name.trim(),
                parse_integer_literal(value)
                    .unwrap_or_else(|| panic!("non-literal enum value in {trimmed}")),
            ),
            None => (trimmed.trim_end_matches(',').trim(), next_value),
        };
        next_value = header_value + 1;
        let literal = binding_constant(bindings, name)
            .unwrap_or_else(|| panic!("{name} is missing from the FFI fragments"));
        let rust_value = parse_integer_literal(literal)
            .unwrap_or_else(|| panic!("{name} does not have a numeric value: {literal}"));
        assert_eq!(
            rust_value, header_value,
            "{name} has value {rust_value} in the FFI fragments but {header_value} in nghttp2.h"
        );
        writeln!(
            c_source,
            "_Static_assert({name} == {rust_value}, \"{name} value\");"
        )
        .expect("write C enum assertion");
    }
}

fn verify_c_layout(install_dir: &Path, out_dir: &Path, source: &Path, bindings: &str) {
    let mut c_source = String::from(
        "/* Generated by nghttp2-sys/build.rs. The checked-in Rust declarations must\nmatch the \
         layout and constant values of the resolved C headers. */\n#include <stddef.h>\n#include \
         <nghttp2/nghttp2.h>\n\n",
    );
    for &(name, size, fields) in ABI_LAYOUT {
        writeln!(
            c_source,
            "_Static_assert(sizeof({name}) == {size}, \"{name} size\");"
        )
        .expect("write C layout assertion");
        for &(field, offset) in fields {
            writeln!(
                c_source,
                "_Static_assert(offsetof({name}, {field}) == {offset}, \"{name}.{field} offset\");"
            )
            .expect("write C layout assertion");
        }
    }
    // These two macros use `sizeof`/cast expressions that rust-bindgen cannot
    // render, so the compiler checks the values that the fragments declare.
    append_constant_assert(&mut c_source, bindings, "NGHTTP2_PROTO_ALPN_LEN");
    append_constant_assert(&mut c_source, bindings, "NGHTTP2_MAX_WINDOW_SIZE");
    append_enum_asserts(source, bindings, &mut c_source);

    let path = out_dir.join("layout_asserts.c");
    fs::write(&path, c_source).expect("write layout_asserts.c");
    let objects = cc::Build::new()
        .file(&path)
        .include(install_dir.join("include"))
        .flag_if_supported("-std=c11")
        .warnings(false)
        .compile_intermediates();
    assert!(
        !objects.is_empty(),
        "the C layout assertions did not produce an object file"
    );
}

fn parse_version_header(path: &Path) -> (String, u32) {
    let text =
        fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let version = text
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("#define NGHTTP2_VERSION ")
                .map(str::trim)
                .and_then(|value| value.strip_prefix('"'))
                .and_then(|value| value.strip_suffix('"'))
                .map(str::to_owned)
        })
        .expect("NGHTTP2_VERSION in nghttp2ver.h");
    let version_num = text
        .lines()
        .find_map(|line| {
            let value = line
                .trim()
                .strip_prefix("#define NGHTTP2_VERSION_NUM ")?
                .trim();
            match value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
            {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => value.parse().ok(),
            }
        })
        .expect("NGHTTP2_VERSION_NUM in nghttp2ver.h");
    assert!(
        version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-'),
        "unexpected NGHTTP2_VERSION value: {version}"
    );
    (version, version_num)
}

fn assemble_bindings(manifest: &Path, version: &str, version_num: u32) -> String {
    let ffi_dir = manifest.join("ffi");
    let mut fragments: Vec<PathBuf> = fs::read_dir(&ffi_dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", ffi_dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".rs.in"))
        })
        .collect();
    fragments.sort();
    assert!(
        !fragments.is_empty(),
        "no FFI fragments found in {}",
        ffi_dir.display()
    );

    let mut out = String::from(
        "// Auto-generated from upstream nghttp2 headers by nghttp2-sys/build.rs.\n// Rust \
         declarations are produced by rust-bindgen without doc comments: the indented C\n// \
         examples in the upstream documentation are parsed as Rust doctests by rustdoc.\n// \
         Version macros are parsed from the resolved source tree at build time.\n\n",
    );
    for fragment in &fragments {
        let text = fs::read_to_string(fragment)
            .unwrap_or_else(|err| panic!("read {}: {err}", fragment.display()));
        out.push_str(&text);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "pub const NGHTTP2_VERSION: &[u8; {}] = b\"{version}\\0\";\n",
        version.len() + 1
    ));
    out.push_str(&format!(
        "pub const NGHTTP2_VERSION_NUM: u32 = {version_num};\n"
    ));
    out
}

fn function_name(declaration: &str) -> Option<String> {
    let head = &declaration[..declaration.find('(')?];
    let name: String = head
        .chars()
        .rev()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

fn verify_public_functions(header: &str, bindings: &str) {
    let mut declarations = 0_usize;
    let mut checked = 0_usize;
    let mut current = String::new();
    for line in header.lines() {
        if line.starts_with("NGHTTP2_EXTERN") {
            declarations += 1;
            current.clear();
        } else if current.is_empty() {
            continue;
        }
        current.push_str(line);
        current.push(' ');
        if !current.contains(';') {
            continue;
        }
        let name = function_name(&current).expect("function name in NGHTTP2_EXTERN declaration");
        if !BLOCKLISTED_FUNCTIONS.contains(&name.as_str()) {
            assert!(
                bindings.contains(&format!("pub fn {name}(")),
                "nghttp2.h declares {name} but the FFI fragments do not"
            );
            checked += 1;
        }
        current.clear();
    }
    assert_eq!(
        checked + BLOCKLISTED_FUNCTIONS.len(),
        declarations,
        "parsed {} NGHTTP2_EXTERN declarations but checked {checked} functions",
        declarations
    );
}

fn verify_public_constants(source: &Path, bindings: &str) {
    for relative in [PUBLIC_HEADER, VERSION_HEADER] {
        let text = fs::read_to_string(source.join(relative))
            .unwrap_or_else(|err| panic!("read {relative}: {err}"));
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("#define NGHTTP2_") else {
                continue;
            };
            let Some(name) = rest.split_whitespace().next() else {
                continue;
            };
            if name == INCLUDE_GUARD_MACRO || GENERATED_VERSION_MACROS.contains(&name) {
                continue;
            }
            assert!(
                bindings.contains(&format!("pub const NGHTTP2_{name}")),
                "{relative} defines NGHTTP2_{name} but the FFI fragments do not"
            );
        }
    }
}

fn verify_public_types(source: &Path, bindings: &str) {
    let text = fs::read_to_string(source.join(PUBLIC_HEADER))
        .unwrap_or_else(|err| panic!("read {PUBLIC_HEADER}: {err}"));
    for line in text.lines() {
        let trimmed = line.trim();
        let name = if let Some(rest) = trimmed.strip_prefix("} nghttp2_") {
            rest.strip_suffix(';').map(|rest| format!("nghttp2_{rest}"))
        } else if let Some(rest) = trimmed.strip_prefix("typedef struct nghttp2_") {
            rest.strip_suffix(';')
                .and_then(|rest| rest.split_whitespace().last())
                .map(str::to_owned)
        } else {
            None
        };
        let Some(name) = name else {
            continue;
        };
        let declared = bindings.contains(&format!("pub struct {name} "))
            || bindings.contains(&format!("pub union {name} "))
            || bindings.contains(&format!("pub type {name} "));
        assert!(
            declared,
            "nghttp2.h declares type {name} but the FFI fragments do not"
        );
    }
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("typedef") || !trimmed.contains("(*nghttp2_") {
            continue;
        }
        let name = trimmed
            .split("(*")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
            .expect("callback typedef name");
        if BLOCKLISTED_TYPES.contains(&name) {
            continue;
        }
        assert!(
            bindings.contains(&format!("pub type {name} ")),
            "nghttp2.h declares callback {name} but the FFI fragments do not"
        );
    }
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("typedef ")
            || !trimmed.ends_with(';')
            || trimmed.contains("(*")
            || trimmed.starts_with("typedef struct ")
            || trimmed.starts_with("typedef union ")
            || trimmed.starts_with("typedef enum ")
        {
            continue;
        }
        let name = trimmed
            .trim_end_matches(';')
            .split_whitespace()
            .last()
            .expect("scalar typedef name");
        if !name.starts_with("nghttp2_") {
            continue;
        }
        assert!(
            bindings.contains(&format!("pub type {name} ")),
            "nghttp2.h declares scalar typedef {name} but the FFI fragments do not"
        );
    }
}

fn verify_public_enums(source: &Path, bindings: &str) {
    let text = fs::read_to_string(source.join(PUBLIC_HEADER))
        .unwrap_or_else(|err| panic!("read {PUBLIC_HEADER}: {err}"));
    let mut in_enum = false;
    let mut constants = 0_usize;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("typedef enum {") {
            in_enum = true;
            continue;
        }
        if in_enum && trimmed.starts_with('}') {
            in_enum = false;
            continue;
        }
        if !in_enum {
            continue;
        }
        let name = trimmed.split(['=', ',']).next().unwrap_or_default().trim();
        if !name.starts_with("NGHTTP2_") {
            continue;
        }
        assert!(
            bindings.contains(&format!("pub const {name}")),
            "nghttp2.h enum constant {name} is missing from the FFI fragments"
        );
        constants += 1;
    }
    assert!(constants > 0, "no enum constants found in nghttp2.h");
}

fn verify_bindings(source: &Path, bindings: &str) {
    let header_path = source.join(PUBLIC_HEADER);
    let header = fs::read_to_string(&header_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", header_path.display()));
    verify_public_functions(&header, bindings);
    verify_public_constants(source, bindings);
    verify_public_enums(source, bindings);
    verify_public_types(source, bindings);
}

fn main() {
    println!("cargo:rerun-if-env-changed=NGHTTP2_DIR");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=ffi");

    let pointer_width = env::var("CARGO_CFG_TARGET_POINTER_WIDTH").unwrap_or_default();
    assert_eq!(
        pointer_width, "64",
        "nghttp2-sys requires a 64-bit target (found pointer width: {pointer_width})"
    );

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let source = resolve_source(&manifest, &out_dir);
    println!(
        "cargo:rerun-if-changed={}",
        source.join(PUBLIC_HEADER).display()
    );

    let install_dir = build_static_library(&source);
    let (version, version_num) =
        parse_version_header(&install_dir.join("include/nghttp2/nghttp2ver.h"));
    let bindings = assemble_bindings(&manifest, &version, version_num);
    verify_bindings(&source, &bindings);
    verify_c_layout(&install_dir, &out_dir, &source, &bindings);
    fs::write(out_dir.join("bindings.rs"), bindings).expect("write bindings.rs");

    let mut linked = false;
    for subdir in ["lib", "lib64"] {
        let candidate = install_dir.join(subdir);
        if candidate.is_dir() {
            println!("cargo:rustc-link-search=native={}", candidate.display());
            linked |= candidate.join("libnghttp2.a").is_file();
        }
    }
    assert!(linked, "CMake did not install a static libnghttp2 archive");
    println!("cargo:rustc-link-lib=static=nghttp2");
}
