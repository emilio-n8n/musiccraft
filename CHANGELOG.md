# Changelog — MusicCraft

All notable changes to MusicCraft are documented here.

## [Unreleased]

### Added

- Versioned document model with tracks, MIDI notes, instruments, BPM and a
  generated three-track sample arrangement.
- Deterministic offline renderer at 44.1 kHz (sine / square / saw oscillators,
  ADSR envelope, track mixing and output clamping).
- Mono float32 WAV export through `hound`.
- Piano-roll timeline with animated, silent playhead, track and note editing,
  transport controls and tempo editing.
- Cancellable "Mixdown WAV" background job with progress reporting.
- Domain tests for duration, oscillator frequencies, render sanity, note
  validation, JSON round-trip, undo/redo and job progress.

## [0.1.0] - 2026-11-22

### Added

- Initial CraftKit-based application shell.
- Versioned project files with atomic save.
- Undo/redo through the shared document history.
- Background jobs with progress and cancellation.
- Application-specific document model and UI.
