# Decisions — MusicCraft

Short architecture decision records. Newest first.

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
WAV is exported in a cancellable CraftKit job with progress; the on-screen
playhead is silent and visual only.
