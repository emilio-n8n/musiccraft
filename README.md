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
- Bounded undo/redo through the shared document history; one piano-roll drag,
  typed title or mixer-control gesture is one undo step.
- Piano-roll editing: drag to move, drag the right edge to resize, `D` to
  duplicate and `Q` to quantize. Grids from 1/32 to a whole note and a
  0–100% quantization strength are selectable; drags snap to the active grid.
- MIDI import/export (SMF type 1) with `midly`: initial tempo, track names,
  GM-program mapping, velocities, overlapping same-key notes and hanging notes
  are handled. Import is bounded to 16 MiB, 256 tracks and 100,000 notes.
- Five built-in oscillators (sine, square, saw, triangle, noise) with an ADSR
  envelope whose release remains continuous for short notes.
- Mixer with mute, solo, per-track volume, master output and two simple insert
  effects (delay and reverb). Mute/solo never changes the gain of other tracks.
- Optional real-time playback via CPAL when an output device exists; rendering
  runs in a cancellable background job. Otherwise the transport runs a
  clearly-labelled offline visual preview.
- Offline mono mixdown to 32-bit float WAV at 44.1 kHz, with a cancellable
  background job and progress reporting. Arrangements over ten minutes or
  requiring more than 512 MiB of aggregate mono buffers fail explicitly instead
  of producing a silently truncated WAV or exhausting memory.
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
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 tools/launcher_check.py . --expected-repo emilio-n8n/musiccraft
```

Linux windowing development packages are installed by the CI workflow.

## Project layout

```text
src/main.rs       CLI and window startup
src/app.rs        UI, piano-roll gestures, menus and lifecycle
src/document.rs   versioned document model and validation
src/edit.rs       pure note editing, duplication and quantization
src/engine.rs     offline synthesis, effects, mixer and WAV export
src/midi.rs       SMF import/export (midly)
src/audio.rs      optional CPAL real-time playback
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
holds a title, tempo, master volume, tracks (name, instrument, volume, mute,
solo, effect) and notes (pitch, start and length in beats, velocity) plus an
identifier counter. New fields are additive with serde defaults, so v0.1.0
project files remain readable at schema 1.

WAV mixdown and real-time buffer preparation run in cancellable background
jobs. Real-time playback is best-effort: when CPAL finds an output device the
rendered buffer is streamed to it; on headless systems the transport reports
"offline preview" and moves only the visual playhead. No plugin or VST is used.

The project schema and launcher manifest remain at version 1. MusicCraft stores
one tempo and one oscillator per track, so SMF import uses the initial tempo and
the last program change in each track. Zero-velocity project notes are silent
and are omitted from SMF export because MIDI represents them as note-offs.

## Licence

Licensed under either of:

- Apache License, Version 2.0 (`LICENSE-APACHE`)
- MIT license (`LICENSE-MIT`)

at your option.

## Third-party notices

See [THIRD_PARTY.md](THIRD_PARTY.md).
