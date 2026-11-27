//! MusicCraft document model.
//!
//! Every musical position is stored in beats; tempo converts beats to seconds
//! at render time. Notes use MIDI pitch numbers (`0..=127`) and a normalized
//! velocity (`0.0..=1.0`). A document is fully self-sufficient: it embeds its
//! tracks, notes and the identifier sequence needed to create new ones.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Highest valid MIDI pitch number.
pub const MAX_MIDI_PITCH: u8 = 127;
/// Tempo used when a stored BPM value is missing or nonsensical.
pub const DEFAULT_BPM: f64 = 120.0;
/// Largest supported note onset or end position, in beats.
///
/// This keeps corrupt projects and duplicated notes away from floating-point
/// overflow and from times that cannot be represented by an SMF file.
pub const MAX_BEAT_POSITION: f64 = 1_000_000.0;

/// Oscillator waveform assigned to a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Instrument {
    /// Smooth sine wave.
    Sine,
    /// Bright square wave.
    Square,
    /// Ramp-like sawtooth wave.
    Saw,
    /// Soft triangular wave.
    Triangle,
    /// Deterministic white-noise texture.
    Noise,
}

impl Instrument {
    /// Short label used by the track editor.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sine => "Sine",
            Self::Square => "Square",
            Self::Saw => "Saw",
            Self::Triangle => "Triangle",
            Self::Noise => "Noise",
        }
    }

    /// General MIDI program used when exporting this oscillator.
    #[must_use]
    pub const fn midi_program(self) -> u8 {
        match self {
            Self::Sine => 72,
            Self::Square => 80,
            Self::Saw => 81,
            Self::Triangle => 82,
            Self::Noise => 126,
        }
    }

    /// Map a General MIDI program to the closest built-in oscillator.
    ///
    /// MusicCraft projects store an oscillator rather than an arbitrary GM
    /// program. The five programs emitted by [`Self::midi_program`] round-trip
    /// exactly; other programs are grouped by their GM instrument family.
    #[must_use]
    pub fn from_midi_program(program: u8) -> Self {
        match program {
            72 => Self::Sine,
            80 => Self::Square,
            81 => Self::Saw,
            82 => Self::Triangle,
            126 => Self::Noise,
            8..=15 | 96..=103 | 120..=125 | 127 => Self::Noise,
            0..=7 | 16..=23 | 73..=79 => Self::Sine,
            56..=63 | 83..=87 => Self::Square,
            24..=55 | 64..=71 | 88..=95 | 104..=119 => Self::Saw,
            _ => Self::Triangle,
        }
    }

    /// All instruments, in editor display order.
    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Sine,
            Self::Square,
            Self::Saw,
            Self::Triangle,
            Self::Noise,
        ]
    }
}

/// Simple per-track effect, applied before the track joins the master mix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    /// No effect.
    #[default]
    None,
    /// Feedback echo; times in beats, levels in `0.0..=1.0`.
    Delay {
        /// Delay time in beats.
        time_beats: f64,
        /// Feedback amount.
        feedback: f32,
        /// Wet level.
        mix: f32,
    },
    /// Short Schroeder-style reverb; levels in `0.0..=1.0`.
    Reverb {
        /// Wet level.
        mix: f32,
        /// Decay of the comb filters.
        decay: f32,
    },
}

impl Effect {
    /// Short label used by the mixer.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Delay { .. } => "Delay",
            Self::Reverb { .. } => "Reverb",
        }
    }
}

/// A single placed note on a track.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// Stable identifier inside the document.
    pub id: u64,
    /// MIDI pitch number (`0..=127`).
    pub pitch: u8,
    /// Position of the note onset, in beats from the start.
    pub start_beat: f64,
    /// Note length in beats; must be positive.
    pub length_beats: f64,
    /// Normalized velocity in `0.0..=1.0`.
    pub velocity: f32,
}

/// Reason a note failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteError {
    /// Pitch is above the MIDI range.
    PitchOutOfRange,
    /// Onset is negative or not a finite number.
    InvalidStart,
    /// Length is zero, negative or not a finite number.
    InvalidLength,
    /// Velocity is outside `0.0..=1.0` or not finite.
    InvalidVelocity,
    /// The note ends beyond the supported beat range or its end is not finite.
    PositionOutOfRange,
}

impl fmt::Display for NoteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::PitchOutOfRange => "pitch must be between 0 and 127",
            Self::InvalidStart => "start beat must be finite and non-negative",
            Self::InvalidLength => "length in beats must be finite and positive",
            Self::InvalidVelocity => "velocity must be a finite value between 0 and 1",
            Self::PositionOutOfRange => {
                "note end must be finite and within the supported beat range"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for NoteError {}

impl Note {
    /// Validate a note before persisting or rendering it.
    ///
    /// # Errors
    ///
    /// Returns the first violated bound; no external input is trusted.
    pub fn validate(&self) -> Result<(), NoteError> {
        if self.pitch > MAX_MIDI_PITCH {
            return Err(NoteError::PitchOutOfRange);
        }
        if !self.start_beat.is_finite() || self.start_beat < 0.0 {
            return Err(NoteError::InvalidStart);
        }
        if !self.length_beats.is_finite() || self.length_beats <= 0.0 {
            return Err(NoteError::InvalidLength);
        }
        if !self.velocity.is_finite() || !(0.0..=1.0).contains(&self.velocity) {
            return Err(NoteError::InvalidVelocity);
        }
        let end = self.end_beat();
        if !end.is_finite() || end > MAX_BEAT_POSITION {
            return Err(NoteError::PositionOutOfRange);
        }
        Ok(())
    }

    /// Whether all note fields are within their supported bounds.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.validate().is_ok()
    }

    /// Beat at which the note ends.
    #[must_use]
    pub fn end_beat(&self) -> f64 {
        self.start_beat + self.length_beats
    }

    /// Clamp an incoming pitch to the MIDI range.
    #[must_use]
    pub const fn clamp_pitch(pitch: u8) -> u8 {
        if pitch > MAX_MIDI_PITCH {
            MAX_MIDI_PITCH
        } else {
            pitch
        }
    }

    /// Clamp an incoming velocity to `0.0..=1.0`, mapping NaNs to silence.
    #[must_use]
    pub fn clamp_velocity(velocity: f32) -> f32 {
        if velocity.is_finite() {
            velocity.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// A track: an instrument, a mixer state and its ordered notes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// Stable identifier inside the document.
    pub id: u64,
    /// User-facing track name.
    pub name: String,
    /// Oscillator used for every note on this track.
    pub instrument: Instrument,
    /// Linear mixer volume in `0.0..=1.0`.
    pub volume: f32,
    /// When muted, the track is skipped by the renderer.
    pub mute: bool,
    /// When any track is soloed, only soloed tracks are rendered.
    #[serde(default)]
    pub solo: bool,
    /// Simple insert effect. Defaults to none for schema-1 files written by
    /// earlier versions.
    #[serde(default)]
    pub effect: Effect,
    /// Notes belonging to this track.
    pub notes: Vec<Note>,
}

impl Track {
    /// Clamp an incoming volume to `0.0..=1.0`, mapping NaNs to silence.
    #[must_use]
    pub fn clamp_volume(volume: f32) -> f32 {
        if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Whether this track should sound given the global solo state.
    #[must_use]
    pub fn is_audible_with(&self, any_solo: bool) -> bool {
        !self.mute && (!any_solo || self.solo)
    }
}

/// A complete, versionable arrangement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Arrangement title.
    pub title: String,
    /// Tempo in beats per minute.
    pub bpm: f64,
    /// Master output level in `0.0..=1.0`.
    #[serde(default = "default_master_volume")]
    pub master_volume: f32,
    /// Tracks in display order.
    pub tracks: Vec<Track>,
    /// Next identifier handed out by [`Document::allocate_id`].
    pub next_id: u64,
}

fn default_master_volume() -> f32 {
    0.9
}

impl Default for Document {
    fn default() -> Self {
        Self {
            title: "Untitled".into(),
            bpm: DEFAULT_BPM,
            master_volume: default_master_volume(),
            tracks: Vec::new(),
            next_id: 1,
        }
    }
}

impl Document {
    /// Reserve a fresh document-wide identifier.
    pub fn allocate_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    /// Tempo guarded against corrupt or user-entered values.
    #[must_use]
    pub fn effective_bpm(&self) -> f64 {
        if self.bpm.is_finite() && self.bpm > 0.0 {
            self.bpm.clamp(20.0, 300.0)
        } else {
            DEFAULT_BPM
        }
    }

    /// Length of the arrangement in beats: the latest valid note end.
    #[must_use]
    pub fn duration_beats(&self) -> f64 {
        self.tracks
            .iter()
            .flat_map(|track| track.notes.iter())
            .filter(|note| note.is_valid())
            .fold(0.0_f64, |latest, note| latest.max(note.end_beat()))
    }

    /// Whether at least one track is soloed.
    #[must_use]
    pub fn has_solo(&self) -> bool {
        self.tracks.iter().any(|track| track.solo)
    }

    /// Length of the arrangement in seconds at the effective tempo.
    #[must_use]
    pub fn duration_seconds(&self) -> f64 {
        self.duration_beats() * 60.0 / self.effective_bpm()
    }

    /// Total number of placed notes, including muted tracks.
    #[must_use]
    pub fn note_count(&self) -> usize {
        self.tracks.iter().map(|track| track.notes.len()).sum()
    }

    /// Generated three-track demonstration arrangement; no external assets.
    #[must_use]
    pub fn sample() -> Self {
        let mut document = Self {
            title: "MusicCraft demo".into(),
            bpm: DEFAULT_BPM,
            master_volume: default_master_volume(),
            tracks: Vec::with_capacity(3),
            next_id: 1,
        };

        add_track(
            &mut document,
            "Lead",
            Instrument::Sine,
            0.85,
            &[
                (60, 0.0, 1.0, 0.9),
                (64, 1.0, 1.0, 0.8),
                (67, 2.0, 1.0, 0.8),
                (72, 3.0, 2.0, 0.9),
                (67, 5.0, 1.0, 0.75),
                (64, 6.0, 2.0, 0.7),
            ],
        );
        add_track(
            &mut document,
            "Bass",
            Instrument::Square,
            0.5,
            &[
                (48, 0.0, 2.0, 0.8),
                (55, 2.0, 2.0, 0.75),
                (57, 4.0, 2.0, 0.75),
                (53, 6.0, 2.0, 0.8),
            ],
        );
        add_track(
            &mut document,
            "Pad",
            Instrument::Saw,
            0.25,
            &[(60, 0.0, 4.0, 0.5), (67, 4.0, 4.0, 0.5)],
        );
        document
    }
}

fn add_track(
    document: &mut Document,
    name: &str,
    instrument: Instrument,
    volume: f32,
    entries: &[(u8, f64, f64, f32)],
) {
    let id = document.allocate_id();
    let mut notes = Vec::with_capacity(entries.len());
    for &(pitch, start_beat, length_beats, velocity) in entries {
        let note_id = document.allocate_id();
        notes.push(Note {
            id: note_id,
            pitch,
            start_beat,
            length_beats,
            velocity,
        });
    }
    document.tracks.push(Track {
        id,
        name: name.into(),
        instrument,
        volume,
        mute: false,
        solo: false,
        effect: Effect::None,
        notes,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use craftkit_core::ProjectEnvelope;
    use craftkit_io::DocumentHost;

    fn simple_document() -> Document {
        Document {
            title: "Test".into(),
            bpm: 120.0,
            master_volume: 0.9,
            tracks: vec![Track {
                id: 1,
                name: "Lead".into(),
                instrument: Instrument::Sine,
                volume: 0.8,
                mute: false,
                solo: false,
                effect: Effect::None,
                notes: vec![
                    Note {
                        id: 2,
                        pitch: 60,
                        start_beat: 0.0,
                        length_beats: 1.0,
                        velocity: 0.8,
                    },
                    Note {
                        id: 3,
                        pitch: 64,
                        start_beat: 2.0,
                        length_beats: 3.0,
                        velocity: 0.7,
                    },
                ],
            }],
            next_id: 4,
        }
    }

    #[test]
    fn duration_is_latest_note_end_in_beats_and_seconds() {
        let document = simple_document();
        assert_eq!(document.duration_beats(), 5.0);
        assert_eq!(document.duration_seconds(), 2.5);
        assert_eq!(document.note_count(), 2);
        assert_eq!(Document::default().duration_beats(), 0.0);
        assert_eq!(Document::default().duration_seconds(), 0.0);

        let sample = Document::sample();
        assert_eq!(sample.tracks.len(), 3);
        assert_eq!(sample.duration_beats(), 8.0);
        assert_eq!(sample.duration_seconds(), 4.0);
    }

    #[test]
    fn effective_bpm_rejects_corrupt_values() {
        let mut document = simple_document();
        document.bpm = f64::NAN;
        assert_eq!(document.effective_bpm(), DEFAULT_BPM);
        document.bpm = -5.0;
        assert_eq!(document.effective_bpm(), DEFAULT_BPM);
        document.bpm = 1000.0;
        assert_eq!(document.effective_bpm(), 300.0);
    }

    #[test]
    fn midi_program_mapping_round_trips_builtin_oscillators() {
        for instrument in Instrument::all() {
            assert_eq!(
                Instrument::from_midi_program(instrument.midi_program()),
                instrument
            );
        }
        assert_eq!(Instrument::from_midi_program(40), Instrument::Saw);
        assert_eq!(Instrument::from_midi_program(8), Instrument::Noise);
        assert_eq!(Instrument::from_midi_program(9), Instrument::Noise);
    }

    #[test]
    fn out_of_bounds_notes_are_refused() {
        let valid = Note {
            id: 1,
            pitch: 60,
            start_beat: 0.0,
            length_beats: 1.0,
            velocity: 0.8,
        };
        assert!(valid.validate().is_ok());
        assert!(valid.is_valid());

        let invalid = [
            Note {
                pitch: 128,
                ..valid
            },
            Note {
                start_beat: -0.5,
                ..valid
            },
            Note {
                start_beat: f64::NAN,
                ..valid
            },
            Note {
                length_beats: 0.0,
                ..valid
            },
            Note {
                length_beats: f64::INFINITY,
                ..valid
            },
            Note {
                velocity: 1.01,
                ..valid
            },
            Note {
                velocity: f32::NAN,
                ..valid
            },
            Note {
                start_beat: MAX_BEAT_POSITION,
                length_beats: 1.0,
                ..valid
            },
            Note {
                start_beat: f64::MAX,
                length_beats: f64::MAX,
                ..valid
            },
        ];
        for note in invalid {
            assert!(note.validate().is_err(), "{note:?} should be rejected");
            assert!(!note.is_valid());
        }
        assert!(Note::clamp_velocity(-2.0) == 0.0);
        assert!(Note::clamp_velocity(f32::NAN) == 0.0);
        assert_eq!(Note::clamp_pitch(200), MAX_MIDI_PITCH);
    }

    #[test]
    fn json_round_trip_preserves_document_and_envelope() {
        let document = Document::sample();
        let json = serde_json::to_string(&document).expect("serialize document");
        let parsed: Document = serde_json::from_str(&json).expect("parse document");
        assert_eq!(parsed, document);

        let envelope = ProjectEnvelope::new("musiccraft", 1, "0.1.0", document.clone());
        let json = serde_json::to_string(&envelope).expect("serialize envelope");
        let parsed: ProjectEnvelope<Document> =
            serde_json::from_str(&json).expect("parse envelope");
        assert_eq!(parsed, envelope);
        assert_eq!(parsed.format, "musiccraft");
        assert_eq!(parsed.schema_version, 1);
        assert!(parsed.validate("musiccraft", 1).is_ok());
        assert!(parsed.validate("other", 1).is_err());
    }

    #[test]
    fn undo_and_redo_a_note() {
        let mut host = DocumentHost::new(simple_document(), "musiccraft", 1, "0.1.0");
        assert_eq!(host.document().tracks[0].notes.len(), 2);

        host.apply(|document| {
            let id = document.allocate_id();
            document.tracks[0].notes.push(Note {
                id,
                pitch: 67,
                start_beat: 4.0,
                length_beats: 1.0,
                velocity: 0.6,
            });
        });
        assert_eq!(host.document().tracks[0].notes.len(), 3);
        assert_eq!(host.document().next_id, 5);
        assert!(host.is_dirty());

        assert!(host.undo());
        assert_eq!(host.document().tracks[0].notes.len(), 2);
        assert_eq!(host.document().next_id, 4);
        assert!(host.redo());
        assert_eq!(host.document().tracks[0].notes.len(), 3);
        assert!(host.document().tracks[0].notes[2].id == 4);
    }
}
