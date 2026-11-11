# Forge V2

A personal desktop workspace with GP Editorial Paper and Carbon themes, written in Rust. Tauri hosts the native app and Leptos renders its interface through WebAssembly. The packaged app does not run a Node server or bundle a separate Chromium browser.

Forge lives in this directory inside the [T3 Code fork](https://github.com/Jaelucard/Forge-V2). The original T3 application remains available at the repository root. This Rust implementation does not read T3's database or credentials. Upstream reference: `pingdotgg/t3code` at `e5b0718a8a297823920d1bf3a7d5cbacd57263ea`; preserve the root MIT licence.

## Use the local build

Open `target/release/bundle/macos/Forge V2.app`. Choose **Add workspace** and select a folder. Create a conversation to save a draft, brief, margin notes or prompt templates. Focus, Review and Explore provide conversation, Git change review and file navigation. Recap lists recorded activity and marked decisions; it does not generate a model summary.

Grok is optional for all these local features. To enable agent conversations later, install the official [Grok Build CLI](https://docs.x.ai/build/overview), sign in using its own `grok login`, and set its executable path in Forge's settings. Refresh the connection status, then explicitly send a message. Forge uses the CLI's saved account, strips API-key environment fallbacks, and never imports credentials from the separate Grok Bot app. A version check is not proof that sign-in works.

Grok runs with broad access under your OS account. The selected project is task context, not a filesystem boundary for the external agent. Forge admits at most four runs and only one writer per checkout; competing tasks queue. Grok's native subagents and detached processes may exceed that visible count. Stopping a run waits for a provider outcome. Interrupted or uncertain delivery keeps its workspace reserved until you confirm the external task has stopped. Forge never blindly resends uncertain work after restarting.

The six local tools never start model requests. Preparing a prompt only edits the composer. Usage displays provider-reported values; missing billing information is shown as unknown. Resource readings are requested on demand and distinguish the app process from direct provider processes; they do not claim to include every WebView helper or provider descendant.

## Build on Apple Silicon

Verified development toolchain: Rust/Cargo 1.99.1, Tauri 2.12.5, Leptos 0.8.23, Trunk 0.22.1 and Tauri CLI 2.12.4. Dependency resolution is recorded in `Cargo.lock`. Install the macOS Command Line Tools and add `wasm32-unknown-unknown` with rustup. The `forge` directory is an independent Cargo workspace; the upstream JavaScript dependencies are not needed.

For local build tools, use the official [Trunk 0.22.1 release](https://github.com/trunk-rs/trunk/releases/tag/v0.22.1) and [Tauri CLI 2.12.4 release](https://github.com/tauri-apps/tauri/releases/tag/tauri-cli-v2.12.4). Place the native `trunk` and `cargo-tauri` executables in `.tools/bin`. Alternatively build those exact versions with Cargo using `--root .tools --locked`.

```sh
cd forge
rustup target add wasm32-unknown-unknown
sh scripts/build-macos.sh
sh scripts/check.sh
```

The script builds the Rust/WASM assets before the desktop app, then creates a local `.app` bundle. It does not sign, notarise or publish a release. The application identifier is `com.jaelucard.forge-v2`; local data normally lives in the OS application-data directory for that identifier. Set `FORGE_DATA_DIR` to an isolated directory when running disposable verification fixtures. A file lock prevents two Forge processes from writing the same database.

## Current boundaries

This is the first macOS trial build. Windows and Linux require their own build and interaction checks before they are advertised as supported. Integrated terminal, browser and document viewers, additional providers and the Muse Personal Agent bridge belong to later phases. Files are currently read-only text previews; previews cannot execute HTML. Git inspection uses read-only commands and cannot commit or modify a checkout.

The Grok adapter has a separate subprocess protocol test suite. A fixture result is not a live account result. Real Grok installation, sign-in and connection testing are deliberately deferred at the owner's request. Muse's readable controls have been inspected, but no automated task round-trip has been implemented or sent.

History renders a bounded page of 80 messages. Full streamed text is saved in indexed SQLite chunks; long message previews are shortened, with full output available in pages. Source previews and diff output have visible size limits. Notes currently anchor to conversations and stable message IDs; file-revision annotations and the full external attachment store are still outstanding. The complete agreed programme and implementation evidence are maintained outside this source tree.

## Source layout

- `crates/domain`: shared typed requests and data.
- `crates/core`: SQLite persistence, local tools, receipts, queue and writer leases.
- `crates/grok`: Grok ACP transport, subprocess lifecycle and protocol fixtures.
- `crates/desktop`: native window, restricted command bridge and guarded workspace access.
- `crates/ui`: Rust/WASM interface and both GP Editorial themes.

Generated JavaScript is limited to the WebAssembly loader and platform bindings. No JavaScript widgets or application logic are maintained here.
