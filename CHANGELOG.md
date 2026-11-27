# Changelog — MusicCraft

All notable changes to MusicCraft are documented here.

## [Unreleased]

### Fixed

- Piano-roll hit testing now selects the topmost overlapping note and no longer
  draws out-of-range MIDI pitches on a false visible row.
- Continuous title, tempo, note, mixer and effect edits record one undo step per
  gesture; delay-time changes now reach the document.
- MIDI import preserves repeated note-ons on one key, closes hanging notes,
  imports GM programs and empty named/programmed tracks, and allocates unique
  track/note identifiers. Export orders note-offs before same-tick note-ons.
- Invalid note positions and SMF tick overflow fail explicitly instead of
  wrapping; MIDI import has file, track and note limits.
- Short-note ADSR release is continuous, corrupt effect parameters cannot create
  NaN samples, and mute/solo no longer changes remaining track gain.
- CPAL pause/resume/stop now controls the stream, failed starts clear playback
  state, and audio callbacks sanitize non-finite samples.
- Real-time playback renders in a cancellable background job; stopping or
  editing invalidates pending playback instead of starting stale audio later.
- WAV writing sanitizes non-finite samples. Renders over ten minutes or over
  512 MiB of aggregate mono buffers fail instead of truncating or exhausting
  memory.

### Added

- Selectable quantization grid and strength, with grid-snapped piano-roll drags.
- Regression coverage for MIDI polyphony/orphans/programs, UI hit testing,
  quantization strength, mixer gain, ADSR, cancellation and WAV/MIDI bounds.

## [0.2.0] - 2026-11-22

### Added

- Piano-roll note editing: drag to move, drag the right edge to resize,
  duplicate (`D`) and quantize (`Q`) on a selectable grid. One drag gesture
  records exactly one undo step.
- SMF MIDI import/export with `midly`: tempo, track names, programs and note
  on/off pairing. The existing WAV mixdown is unchanged.
- Two extra instruments (triangle, deterministic noise) for five waveforms.
- Simple per-track insert effects (feedback delay, four-comb reverb) and a mixer
  with mute, solo, per-track volume and a master output level.
- Optional real-time playback through CPAL when an output device exists;
  otherwise an explicitly-labelled offline visual preview.
- Domain tests for note editing, quantization, MIDI round-trip, mixer/effects,
  gesture undo and the audio callback.

### Changed

- Rendering applies insert effects per track before the master mix.

## [0.1.0] - 2026-11-22

### Added

- Versioned document model with tracks, MIDI notes, instruments, BPM and a
  generated three-track sample arrangement.
- Deterministic offline renderer at 44.1 kHz (sine / square / saw oscillators,
  ADSR envelope, track mixing and output clamping).
- Mono float32 WAV export through `hound`.
- Piano-roll timeline with animated playhead, track and note editing, transport
  controls and tempo editing.
- Cancellable "Mixdown WAV" background job with progress reporting.
- Domain tests for duration, oscillator frequencies, render sanity, note
  validation, JSON round-trip, undo/redo and job progress.
- Initial CraftKit-based application shell with versioned project files,
  atomic saves, bounded undo/redo and background jobs.
