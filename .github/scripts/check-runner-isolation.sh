#!/usr/bin/env bash
# Verifies that this runner owns its workspace and isolates temporary state so concurrent jobs on
# the same host cannot collide.
set -euo pipefail

workspace="${GITHUB_WORKSPACE:-$(git rev-parse --show-toplevel)}"
workspace="$(cd "$workspace" && pwd -P)"

if [ -n "${RUNNER_WORKSPACE:-}" ]; then
  if [ ! -d "$RUNNER_WORKSPACE" ]; then
    echo "::error::$(hostname): RUNNER_WORKSPACE=$RUNNER_WORKSPACE does not exist"
    exit 1
  fi
  runner_workspace="$(cd "$RUNNER_WORKSPACE" && pwd -P)"
  case "$workspace/" in
    "$runner_workspace/"*) ;;
    *)
      echo "::error::$(hostname): workspace $workspace is outside this runner's $runner_workspace"
      exit 1
      ;;
  esac
fi

if [ -n "${CARGO_TARGET_DIR:-}" ]; then
  case "$CARGO_TARGET_DIR" in
    /*) target="$CARGO_TARGET_DIR" ;;
    *) target="$workspace/$CARGO_TARGET_DIR" ;;
  esac
  target="$(realpath -m "$target")"
  case "$target/" in
    "$workspace/"*) ;;
    *)
      echo "::error::$(hostname): CARGO_TARGET_DIR=$target is outside the workspace $workspace"
      exit 1
      ;;
  esac
fi

origin="$(git -C "$workspace" remote get-url origin 2>/dev/null || true)"
case "$origin" in
  *nghttp2-rs*) ;;
  *)
    echo "::error::$(hostname): workspace $workspace is not an nghttp2-rs checkout (origin: ${origin:-none})"
    exit 1
    ;;
esac

if [ -n "${RUNNER_WORKSPACE:-}" ] && [ -n "${GITHUB_ENV:-}" ]; then
  if [ ! -w "$runner_workspace" ]; then
    echo "::error::$(hostname): $runner_workspace is not writable by $(id -un)"
    exit 1
  fi
  tmp="$runner_workspace/.tmp"
  rm -rf "$tmp"
  mkdir -p "$tmp"
  echo "TMPDIR=$tmp" >> "$GITHUB_ENV"
  echo "$(hostname): TMPDIR=$tmp"
fi

if [ -e /tmp/.nghttp2-rs-ci-private ] || [ -e /tmp/.questra-ci-private ]; then
  host_shm="$(awk '$5 == "/dev/shm" { print $3 }' /proc/1/mountinfo)"
  own_shm="$(stat -c '%Hd:%Ld' /dev/shm)"
  if [ -z "$host_shm" ] || [ "$host_shm" = "$own_shm" ]; then
    echo "::error::$(hostname): /dev/shm ($own_shm) is the host's ($host_shm); the runner service lacks the isolation drop-in"
    exit 1
  fi
  rm -rf /tmp/nghttp2-rs*
  find /dev/shm -mindepth 1 -delete
  echo "$(hostname): private /tmp, private /dev/shm ($own_shm, host $host_shm)"
else
  echo "$(hostname): /tmp is shared with other runners; using runner-scoped TMPDIR"
fi
