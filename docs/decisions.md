# Decisions — MusicCraft

Short architecture decision records. Newest first.

## ADR-004 — Bounded audio and lossless editing gestures

Date: 2026-11-27

Decision: keep schema 1 unchanged, group continuous UI edits into history
gestures, prepare real-time playback in a cancellable job, and reject audio or
MIDI values that exceed explicit representation/render limits. Quantization
strength and grid selection are session UI state, not persisted document fields.

Alternatives: store arbitrary MIDI program/tempo maps in the project; render on
the UI thread; silently clamp oversized arrangements; snapshot every widget
frame as an undo step.

Reasons: the existing document intentionally has one tempo and one oscillator
per track, the UI must stay responsive, and silent data loss or hundreds of
undo steps per drag are user-visible failures.

Consequences: SMF tempo maps and per-note program changes are approximated on
import, zero-velocity notes are omitted on export, renders over ten minutes
return an error, and MIDI imports are bounded. Regression tests cover these
limits and the gesture/editing behaviour.

## ADR-001 — Built on CraftKit

Date: 2026-11-22

Decision: use the pinned CraftKit crates for the native shell, project
lifecycle, history and background jobs.

Alternatives: build the desktop foundation from scratch; use another UI
framework.

Reasons: consistent behaviour across the portfolio, fewer duplicated bugs,
shared release and testing conventions.

Consequences: CraftKit upgrades are explicit tag changes; domain code stays in
this repository.

## ADR-002 — Beat-based document and offline rendering

Date: 2026-11-22

Decision: store note positions and lengths in beats, convert to seconds with a
clamped BPM only at render time, and render the whole arrangement into a mono
44.1 kHz float buffer that is clamped to `[-1, 1]` before WAV export. Identifiers
come from a single `next_id` counter on the document.

Alternatives: store absolute seconds; stream to an audio device; use a plugin
host.

Reasons: beats survive tempo edits, a pure renderer is easy to test, and the
desktop shell must never block on audio callbacks.

Consequences: `--sample` is generated in code (no proprietary assets); the
WAV is exported in a cancellable CraftKit job with progress; when no audio
device is present the playhead is a silent, clearly-labelled visual preview.

## ADR-003 — Productisation without changing the schema

Date: 2026-11-22

Decision: add productisation features (editing gestures, MIDI I/O, effects,
mixer, optional real-time playback) while keeping `format = musiccraft`,
`schema_version = 1`, the launcher manifest and the JSON envelope unchanged.
New document fields (`master_volume`, `solo`, `effect` and extra instrument
variants) are additive and deserialized with serde defaults, so v0.1.0 files
still open.

Alternatives: bump the schema; split into multiple crates; embed a plugin host.

Reasons: v0.1.0 is a published snapshot and the launcher rules must stay valid;
additive fields keep existing projects loadable and CI simple.

Consequences: no migration code is required; the piano roll records one
`History` gesture per drag; real-time playback is best-effort and falls back to
the offline preview on headless runners.
