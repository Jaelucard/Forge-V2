#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}"
if [ ! -x .tools/bin/trunk ] || [ ! -x .tools/bin/cargo-tauri ]; then
  printf '%s\n' 'Install the pinned project-local build tools described in forge/README.md.' >&2
  exit 1
fi
.tools/bin/trunk build --release --locked
cd crates/desktop
../../.tools/bin/cargo-tauri build --bundles app
