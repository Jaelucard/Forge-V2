#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}"
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --exclude forge-ui --all-targets --locked -- -D warnings
cargo clippy -p forge-ui --target wasm32-unknown-unknown --locked -- -D warnings
