# MusicCraft

MIDI arrangement, built-in synth and offline WAV mixdown

**MusicCraft is an independent, community open-source project.** It is not an
official product of, endorsed by, or affiliated with any third-party vendor it
may interoperate with.

Built on [CraftKit](https://github.com/emilio-n8n/craftkit), a small native Rust
desktop foundation.

## Features

- Native Rust desktop UI (egui/eframe), no web runtime.
- Versioned JSON project files with atomic saves (`musiccraft`, schema 1).
- Bounded undo/redo through the shared document history.
- Piano-roll arrangement with an animated, silent preview playhead.
- Three built-in oscillators (sine, square, saw) with a simple ADSR envelope.
- Offline mono mixdown to 32-bit float WAV at 44.1 kHz, with a cancellable
  background job and progress reporting.
- Recent files and persisted preferences.
- Offline-first: no account, no telemetry, no network required.

## Run

```bash
cargo run --release -- --sample
cargo run --release -- --open project.json
```

Without arguments the application opens an empty document.

- `--sample` opens the built-in demonstration document.
- `--open <file>` opens a project.
- `--version` prints the version.
- `--help` prints usage.

## Build and test

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Linux windowing development packages are installed by the CI workflow.

## Project layout

```text
src/main.rs       CLI and window startup
src/app.rs        UI, piano roll, menus and lifecycle
src/document.rs   versioned document model and validation
src/engine.rs     offline synthesis and WAV export
```

The application depends on pinned CraftKit crates from Git. Keep the tag in
`Cargo.toml` and `Cargo.lock` consistent when upgrading the foundation.

## File format

Projects are JSON envelopes:

```json
{
  "format": "musiccraft",
  "schema_version": 1,
  "app_version": "0.1.0",
  "document": { }
}
```

The application owns the `document` schema and its migrations. A newer
`schema_version` is rejected instead of being silently misread. The document
holds a title, a tempo in BPM, tracks (name, instrument, volume, mute) and notes
(pitch, start and length in beats, velocity) plus an identifier counter.

Audio is rendered offline in a background job: no audio device, real-time
thread or plugin is involved. The playhead is a visual guide only.

## Licence

Licensed under either of:

- Apache License, Version 2.0 (`LICENSE-APACHE`)
- MIT license (`LICENSE-MIT`)

at your option.

## Third-party notices

See [THIRD_PARTY.md](THIRD_PARTY.md).
