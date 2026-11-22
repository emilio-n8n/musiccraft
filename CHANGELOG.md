# Changelog — MusicCraft

All notable changes to MusicCraft are documented here.

## [Unreleased] — productisation (future v0.2.0)

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
