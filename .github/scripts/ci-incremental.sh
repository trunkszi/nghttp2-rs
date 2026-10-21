#!/usr/bin/env bash
# Incremental CI gate for nghttp2-rs.
#
# Inspects `git diff --name-only "$BASE_SHA...HEAD"`, verifies architectural invariants
# (<= 800 lines per .rs/.c/.cpp/.h/.hpp file, zero async runtime dependencies), checks
# formatting via `cargo fmt --all -- --check`, and runs `cargo clippy` + `cargo nextest`
# (or `cargo test`) strictly on the modified workspace packages and their reverse-dependents.
set -euo pipefail

BASE_SHA="${1:-}"
if [ -z "$BASE_SHA" ] || ! git cat-file -e "${BASE_SHA}^{commit}" 2>/dev/null; then
  if git cat-file -e "HEAD~1^{commit}" 2>/dev/null; then
    BASE_SHA="$(git rev-parse HEAD~1)"
  else
    BASE_SHA="$(git rev-parse HEAD)"
  fi
fi

echo "==> Incremental CI diff base: $BASE_SHA -> $(git rev-parse HEAD)"

if [ "$BASE_SHA" = "$(git rev-parse HEAD)" ]; then
  changed_files="$(git ls-files)"
else
  changed_files="$(git diff --name-only "$BASE_SHA...HEAD")"
fi

if [ -z "$changed_files" ]; then
  echo "==> No files changed between $BASE_SHA and HEAD."
  exit 0
fi

echo "==> Changed files:"
echo "$changed_files" | sed 's/^/  - /'

# 1. Enforce <= 800 line limit on all tracked .rs, .c, .cpp, .h, .hpp files.
echo "==> Checking <= 800 line limit on source files (.rs, .c, .cpp, .h, .hpp)..."
max_lines=800
line_violations=0
while IFS= read -r file; do
  [ -z "$file" ] && continue
  [ -f "$file" ] || continue
  case "$file" in
    *.rs|*.c|*.cpp|*.h|*.hpp)
      lines="$(awk 'END{print NR}' "$file")"
      if [ "$lines" -gt "$max_lines" ]; then
        echo "::error file=$file::$file has $lines lines (limit is $max_lines)"
        line_violations=$((line_violations + 1))
      fi
      ;;
  esac
done < <(git ls-files '*.rs' '*.c' '*.cpp' '*.h' '*.hpp')

if [ "$line_violations" -gt 0 ]; then
  echo "::error::$line_violations source file(s) exceeded the $max_lines-line limit."
  exit 1
fi

# 2. Enforce zero async runtime dependencies across all Cargo.toml manifests.
async_names='(tokio|futures|futures-core|futures-util|async-std|smol)'
dependency_key="[\"']?${async_names}[\"']?"
while IFS= read -r manifest; do
  [ -z "$manifest" ] && continue
  [ -f "$manifest" ] || continue
  if grep -Eq "^[[:space:]]*(${dependency_key}[[:space:]]*(=|\.)|[^=#]*dependencies[[:space:]]*\.[[:space:]]*${dependency_key}[[:space:]]*(=|\.)|\[[^]#]*dependencies[[:space:]]*\.[[:space:]]*${dependency_key}[[:space:]]*(\.|\]))" "$manifest"; then
    echo "::error file=$manifest::Forbidden async runtime dependency detected in $manifest"
    exit 1
  fi
done < <(git ls-files '*Cargo.toml')

# 3. If the root Cargo.toml does not exist yet, exit cleanly.
if [ ! -f Cargo.toml ]; then
  echo "==> No root Cargo.toml present on this branch yet; skipping Cargo checks."
  exit 0
fi

# 4. Determine affected workspace crates from changed files.
run_all_crates=0
declare -A affected_map=()

while IFS= read -r file; do
  [ -z "$file" ] && continue
  case "$file" in
    README.md)
      affected_map["nghttp2"]=1
      ;;
    *.md|.scratch/*|docs/*|GLOSSARY.md|AGENTS.md|.gitignore)
      ;;
    Cargo.toml|Cargo.lock|rust-toolchain.toml|rustfmt.toml|.github/*)
      run_all_crates=1
      ;;
    crates/nghttp2-sys/*)
      affected_map["nghttp2-sys"]=1
      affected_map["nghttp2"]=1
      ;;
    crates/nghttp2-macros/*)
      affected_map["nghttp2-macros"]=1
      affected_map["nghttp2"]=1
      ;;
    crates/nghttp2/*)
      affected_map["nghttp2"]=1
      ;;
    *)
      run_all_crates=1
      ;;
  esac
done <<< "$changed_files"

workspace_pkgs="$(python3 -c '
import json, subprocess
meta = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"], text=True))
for pkg in meta.get("packages", []):
    print(pkg["name"])
')"

if [ "$run_all_crates" -eq 1 ]; then
  for pkg in $workspace_pkgs; do
    affected_map["$pkg"]=1
  done
fi

if [ "${#affected_map[@]}" -eq 0 ]; then
  echo "==> Only documentation / scratch files changed; skipping Cargo fmt, clippy, and test."
  exit 0
fi

# 5. Always check formatting when Rust/Cargo files are touched.
echo "==> Running cargo fmt --all -- --check..."
cargo fmt --all -- --check

ordered_candidates=("nghttp2-sys" "nghttp2-macros" "nghttp2")
selected_pkgs=()
for candidate in "${ordered_candidates[@]}"; do
  if [ -n "${affected_map[$candidate]:-}" ] && echo "$workspace_pkgs" | grep -qx "$candidate"; then
    selected_pkgs+=("$candidate")
  fi
done
for pkg in $workspace_pkgs; do
  if [ -n "${affected_map[$pkg]:-}" ]; then
    already=0
    for s in "${selected_pkgs[@]:-}"; do
      [ "$s" = "$pkg" ] && already=1
    done
    [ "$already" -eq 0 ] && selected_pkgs+=("$pkg")
  fi
done

echo "==> Affected workspace packages: ${selected_pkgs[*]}"

for pkg in "${selected_pkgs[@]}"; do
  echo "==> [clippy] package: $pkg"
  cargo clippy -p "$pkg" --all-targets -- -D warnings
done

for pkg in "${selected_pkgs[@]}"; do
  echo "==> [test] package: $pkg"
  if command -v cargo-nextest >/dev/null 2>&1; then
    cargo nextest run -p "$pkg" --no-tests=pass
  else
    cargo test -p "$pkg"
  fi
  echo "==> [doctest] package: $pkg"
  cargo test --doc -p "$pkg"
done

echo "==> Incremental CI succeeded for: ${selected_pkgs[*]}"
