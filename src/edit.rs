//! Pure note-editing domain operations.
//!
//! Everything here works on plain [`Document`] values so it can be tested
//! without a UI. The application wraps one drag in a single `History` snapshot;
//! these helpers only change note fields.

use crate::document::{Document, Note, Track};

/// Smallest supported note length in beats.
pub const MIN_LENGTH: f64 = 0.125;
/// Largest supported note length in beats.
pub const MAX_LENGTH: f64 = 32.0;
/// Default quantization grid in beats (a sixteenth note at 4/4).
pub const DEFAULT_GRID: f64 = 0.25;

/// Clamp a quantization grid to a sane musical range.
#[must_use]
pub fn clamp_grid(grid: f64) -> f64 {
    if grid.is_finite() {
        grid.clamp(MIN_LENGTH, 4.0)
    } else {
        DEFAULT_GRID
    }
}

/// Move a note by a beat/pitch offset, clamping it to legal bounds.
#[must_use]
pub fn move_note(note: &Note, delta_beats: f64, delta_pitch: i32) -> Note {
    let mut moved = *note;
    if delta_beats.is_finite() {
        moved.start_beat = (note.start_beat + delta_beats).max(0.0);
    }
    let pitch = i32::from(note.pitch) + delta_pitch;
    moved.pitch = u8::try_from(pitch.clamp(0, i32::from(crate::document::MAX_MIDI_PITCH)))
        .unwrap_or(note.pitch);
    moved
}

/// Resize a note's right edge, keeping a legal positive length.
#[must_use]
pub fn resize_note(note: &Note, new_length_beats: f64) -> Note {
    let mut resized = *note;
    if new_length_beats.is_finite() {
        resized.length_beats = new_length_beats.clamp(MIN_LENGTH, MAX_LENGTH);
    }
    resized
}

/// Snap a beat value to the nearest grid line.
#[must_use]
pub fn snap(value: f64, grid: f64) -> f64 {
    let grid = clamp_grid(grid);
    if value.is_finite() {
        (value / grid).round() * grid
    } else {
        0.0
    }
}

/// Quantize a note's start and length to a grid, preserving a legal length.
#[must_use]
pub fn quantize_note(note: &Note, grid: f64) -> Note {
    let grid = clamp_grid(grid);
    let mut quantized = *note;
    let start = snap(note.start_beat, grid);
    let end = snap(note.end_beat(), grid);
    quantized.start_beat = start;
    quantized.length_beats = (end - start).clamp(MIN_LENGTH, MAX_LENGTH);
    quantized
}

/// Quantize every note of a track and return the number of changed notes.
pub fn quantize_track(track: &mut Track, grid: f64) -> usize {
    let mut changed = 0;
    for note in &mut track.notes {
        let quantized = quantize_note(note, grid);
        if quantized != *note {
            *note = quantized;
            changed += 1;
        }
    }
    changed
}

/// Quantize every note of a document and return the number of changed notes.
pub fn quantize_document(document: &mut Document, grid: f64) -> usize {
    let mut changed = 0;
    for track in &mut document.tracks {
        changed += quantize_track(track, grid);
    }
    changed
}

/// Duplicate a note onto the same track, immediately after its original.
///
/// Returns the identifier of the new note, or `None` if the note was not found.
pub fn duplicate_note(document: &mut Document, track_index: usize, note_id: u64) -> Option<u64> {
    let (pitch, start_beat, length_beats, velocity) = {
        let track = document.tracks.get(track_index)?;
        let note = track.notes.iter().find(|note| note.id == note_id)?;
        (
            note.pitch,
            note.start_beat,
            note.length_beats,
            note.velocity,
        )
    };
    let id = document.allocate_id();
    let track = document.tracks.get_mut(track_index)?;
    track.notes.push(Note {
        id,
        pitch,
        start_beat: start_beat + length_beats,
        length_beats,
        velocity,
    });
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Effect, Instrument};
    use craftkit_core::History;

    fn note() -> Note {
        Note {
            id: 1,
            pitch: 60,
            start_beat: 1.0,
            length_beats: 1.0,
            velocity: 0.8,
        }
    }

    fn document() -> Document {
        Document {
            title: "Edit".into(),
            bpm: 120.0,
            master_volume: 0.9,
            tracks: vec![Track {
                id: 2,
                name: "Lead".into(),
                instrument: Instrument::Sine,
                volume: 0.8,
                mute: false,
                solo: false,
                effect: Effect::None,
                notes: vec![note()],
            }],
            next_id: 3,
        }
    }

    #[test]
    fn moving_a_note_clamps_pitch_and_start() {
        let original = note();
        let moved = move_note(&original, -5.0, 10);
        assert_eq!(moved.pitch, 70);
        assert_eq!(moved.start_beat, 0.0);
        let high = move_note(&original, 0.0, 200);
        assert_eq!(high.pitch, i32::from(crate::document::MAX_MIDI_PITCH) as u8);
        let nan = move_note(&original, f64::NAN, 1);
        assert_eq!(nan.start_beat, original.start_beat);
    }

    #[test]
    fn resizing_a_note_keeps_a_legal_length() {
        assert_eq!(resize_note(&note(), 2.5).length_beats, 2.5);
        assert_eq!(resize_note(&note(), -1.0).length_beats, MIN_LENGTH);
        assert_eq!(resize_note(&note(), 100.0).length_beats, MAX_LENGTH);
        assert_eq!(resize_note(&note(), f64::NAN).length_beats, 1.0);
    }

    #[test]
    fn quantization_snaps_start_and_end_to_the_grid() {
        let note = Note {
            start_beat: 1.07,
            length_beats: 0.9,
            ..note()
        };
        let quantized = quantize_note(&note, 0.25);
        assert_eq!(quantized.start_beat, 1.0);
        assert_eq!(quantized.length_beats, 1.0);
        assert!(quantized.is_valid());
        assert_eq!(clamp_grid(0.0), MIN_LENGTH);
        assert_eq!(clamp_grid(f64::NAN), DEFAULT_GRID);
    }

    #[test]
    fn quantize_document_counts_only_changed_notes() {
        let mut document = document();
        assert_eq!(quantize_document(&mut document, 0.25), 0);
        document.tracks[0].notes[0].start_beat = 1.1;
        assert_eq!(quantize_document(&mut document, 0.25), 1);
        assert_eq!(document.tracks[0].notes[0].start_beat, 1.0);
    }

    #[test]
    fn duplicating_a_note_allocates_a_fresh_id() {
        let mut document = document();
        let id = duplicate_note(&mut document, 0, 1).expect("duplicate");
        assert_eq!(id, 3);
        assert_eq!(document.next_id, 4);
        assert_eq!(document.tracks[0].notes.len(), 2);
        assert_eq!(document.tracks[0].notes[1].start_beat, 2.0);
        assert_eq!(document.tracks[0].notes[1].pitch, 60);
        assert!(duplicate_note(&mut document, 5, 1).is_none());
        assert!(duplicate_note(&mut document, 0, 99).is_none());
    }

    #[test]
    fn a_gesture_is_a_single_undo_step() {
        let original = document();
        let mut history = History::new(original.clone());
        history.begin_gesture();
        let current = history.current_mut();
        current.tracks[0].notes[0] = move_note(&current.tracks[0].notes[0], 1.0, 0);
        current.tracks[0].notes[0] = resize_note(&current.tracks[0].notes[0], 2.0);
        history.end_gesture();

        assert_eq!(history.undo_depth(), 1);
        assert_ne!(history.current(), &original);
        assert!(history.undo());
        assert_eq!(history.current(), &original);
        assert!(history.redo());
    }
}
