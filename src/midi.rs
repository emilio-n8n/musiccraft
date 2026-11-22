//! Standard MIDI File import and export.
//!
//! Uses `midly` to read/write SMF type-1 files. Notes are paired by channel
//! and key; tempo meta events and track names are preserved on round-trip.
//! No plugin, synth or external asset is involved.

use std::collections::HashMap;
use std::fmt;

use midly::{
    Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEventKind,
    num::{u15, u24, u28},
};

use crate::document::{Document, Instrument, Note, Track};

/// Default ticks per quarter note used by exported files.
pub const TICKS_PER_BEAT: u32 = 480;

/// Failure modes of MIDI import/export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiError {
    /// Input bytes are not a valid SMF file.
    Invalid(String),
    /// The file uses a timing scheme this importer cannot map.
    UnsupportedTiming,
    /// The file could not be written.
    Write(String),
}

impl fmt::Display for MidiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => write!(formatter, "invalid MIDI file: {error}"),
            Self::UnsupportedTiming => formatter.write_str("unsupported MIDI timing"),
            Self::Write(error) => write!(formatter, "could not write MIDI file: {error}"),
        }
    }
}

impl std::error::Error for MidiError {}

/// Import an SMF byte buffer into a self-sufficient MusicCraft document.
///
/// # Errors
///
/// Returns [`MidiError`] for malformed input or unsupported timing.
pub fn import_bytes(bytes: &[u8]) -> Result<Document, MidiError> {
    let smf = Smf::parse(bytes).map_err(|error| MidiError::Invalid(error.to_string()))?;
    let ticks_per_beat = match smf.header.timing {
        Timing::Metrical(ticks) => u32::from(ticks.as_int()).max(1),
        Timing::Timecode(_, _) => return Err(MidiError::UnsupportedTiming),
    };

    let mut document = Document {
        title: "Imported MIDI".into(),
        bpm: crate::document::DEFAULT_BPM,
        ..Document::default()
    };

    let mut track_id = 1_u64;
    for midi_track in &smf.tracks {
        let mut name = None;
        let mut active: HashMap<(u8, u8), (u32, f32)> = HashMap::new();
        let mut notes: Vec<Note> = Vec::new();
        let mut tick = 0_u32;

        for event in midi_track {
            tick = tick.saturating_add(event.delta.as_int());
            match event.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(micros)) => {
                    let micros = micros.as_int();
                    if micros > 0 {
                        document.bpm = (60_000_000.0 / f64::from(micros)).clamp(20.0, 300.0);
                    }
                }
                TrackEventKind::Meta(MetaMessage::TrackName(bytes)) => {
                    name = Some(String::from_utf8_lossy(bytes).into_owned());
                }
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOn { key, vel },
                } => {
                    let key = key.as_int();
                    let velocity = f32::from(vel.as_int()) / 127.0;
                    if velocity == 0.0 {
                        if let Some((start_tick, start_velocity)) =
                            active.remove(&(u8::from(channel), key))
                        {
                            push_note(
                                &mut notes,
                                &mut document,
                                key,
                                start_tick,
                                tick,
                                start_velocity,
                                ticks_per_beat,
                            );
                        }
                    } else {
                        active.insert((u8::from(channel), key), (tick, velocity.clamp(0.0, 1.0)));
                    }
                }
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOff { key, .. },
                } => {
                    let key = key.as_int();
                    if let Some((start_tick, start_velocity)) =
                        active.remove(&(u8::from(channel), key))
                    {
                        push_note(
                            &mut notes,
                            &mut document,
                            key,
                            start_tick,
                            tick,
                            start_velocity,
                            ticks_per_beat,
                        );
                    }
                }
                _ => {}
            }
        }

        if notes.is_empty() {
            continue;
        }
        notes.sort_by(|a, b| a.start_beat.total_cmp(&b.start_beat));
        let track = Track {
            id: track_id,
            name: name
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| document.tracks.len().saturating_add(1).to_string()),
            instrument: Instrument::from_index((document.tracks.len() % 5) as u8),
            volume: 0.8,
            mute: false,
            solo: false,
            effect: crate::document::Effect::None,
            notes,
        };
        track_id = track_id.saturating_add(1);
        document.tracks.push(track);
    }

    Ok(document)
}

fn push_note(
    notes: &mut Vec<Note>,
    document: &mut Document,
    pitch: u8,
    start_tick: u32,
    end_tick: u32,
    velocity: f32,
    ticks_per_beat: u32,
) {
    let start_beat = f64::from(start_tick) / f64::from(ticks_per_beat);
    let length_beats = f64::from(end_tick.saturating_sub(start_tick)) / f64::from(ticks_per_beat);
    let id = document.allocate_id();
    notes.push(Note {
        id,
        pitch,
        start_beat,
        length_beats: length_beats.max(crate::edit::MIN_LENGTH),
        velocity,
    });
}

/// Export a document as an SMF type-1 byte buffer.
///
/// # Errors
///
/// Returns [`MidiError::Write`] if encoding fails.
pub fn export_bytes(document: &Document) -> Result<Vec<u8>, MidiError> {
    let header = Header::new(
        Format::Parallel,
        Timing::Metrical(u15::new(TICKS_PER_BEAT as u16)),
    );
    let mut smf = Smf::new(header);

    let micros = (60_000_000.0 / document.effective_bpm()).round().max(1.0) as u32;
    smf.tracks.push(vec![
        midly::TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(micros))),
        },
        midly::TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        },
    ]);

    for (index, track) in document.tracks.iter().enumerate() {
        let channel = u8::try_from(index % 16).unwrap_or(0).into();
        let mut events: Vec<(u32, u8, TrackEventKind<'_>)> = Vec::new();
        events.push((
            0,
            0,
            TrackEventKind::Meta(MetaMessage::TrackName(track.name.as_bytes())),
        ));
        events.push((
            0,
            1,
            TrackEventKind::Midi {
                channel,
                message: MidiMessage::ProgramChange {
                    program: track.instrument.index().into(),
                },
            },
        ));
        for note in &track.notes {
            if !note.is_valid() {
                continue;
            }
            let start = (note.start_beat * f64::from(TICKS_PER_BEAT))
                .round()
                .max(0.0) as u32;
            let end = (note.end_beat() * f64::from(TICKS_PER_BEAT))
                .round()
                .max(f64::from(start + 1)) as u32;
            let key = note.pitch.into();
            let vel = (note.velocity * 127.0).round().clamp(1.0, 127.0) as u8;
            events.push((
                start,
                2,
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOn {
                        key,
                        vel: vel.into(),
                    },
                },
            ));
            events.push((
                end,
                3,
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOff {
                        key,
                        vel: 0_u8.into(),
                    },
                },
            ));
        }
        events.sort_by_key(|event| (event.0, event.1));

        let mut out = Vec::with_capacity(events.len() + 1);
        let mut previous = 0_u32;
        for (tick, _, kind) in events {
            out.push(midly::TrackEvent {
                delta: u28::new(tick.saturating_sub(previous)),
                kind,
            });
            previous = tick;
        }
        out.push(midly::TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        smf.tracks.push(out);
    }

    let mut bytes = Vec::new();
    smf.write_std(&mut bytes)
        .map_err(|error| MidiError::Write(error.to_string()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_bytes_have_a_valid_header() {
        let bytes = export_bytes(&Document::sample()).expect("export");
        let smf = Smf::parse(&bytes).expect("reparse");
        assert_eq!(smf.header.format, Format::Parallel);
        assert_eq!(
            smf.header.timing,
            Timing::Metrical(u15::new(TICKS_PER_BEAT as u16))
        );
        // One tempo track plus the three arrangement tracks.
        assert_eq!(smf.tracks.len(), 4);
    }

    #[test]
    fn midi_round_trip_preserves_notes_and_tempo() {
        let original = Document::sample();
        let bytes = export_bytes(&original).expect("export");
        let imported = import_bytes(&bytes).expect("import");

        assert!((imported.bpm - original.bpm).abs() < 0.5);
        assert_eq!(imported.tracks.len(), original.tracks.len());
        for (before, after) in original.tracks.iter().zip(imported.tracks.iter()) {
            assert_eq!(before.name, after.name);
            assert_eq!(before.instrument, after.instrument);
            assert_eq!(before.notes.len(), after.notes.len());
            for (note_before, note_after) in before.notes.iter().zip(after.notes.iter()) {
                assert_eq!(note_before.pitch, note_after.pitch);
                assert!((note_before.start_beat - note_after.start_beat).abs() < 0.01);
                assert!((note_before.length_beats - note_after.length_beats).abs() < 0.01);
                assert!((note_before.velocity - note_after.velocity).abs() < 0.01);
                assert!(note_after.is_valid());
            }
        }
    }

    #[test]
    fn invalid_bytes_are_rejected_without_panicking() {
        assert!(import_bytes(b"not a midi file").is_err());
        assert!(import_bytes(&[]).is_err());
        assert!(import_bytes(&[0xFF, 0xFF]).is_err());
    }

    #[test]
    fn note_off_with_zero_velocity_closes_the_note() {
        let mut document = Document::sample();
        document.tracks.clear();
        document.tracks.push(Track {
            id: 1,
            name: "Solo".into(),
            instrument: Instrument::Sine,
            volume: 0.7,
            mute: false,
            solo: false,
            effect: crate::document::Effect::None,
            notes: vec![Note {
                id: 2,
                pitch: 64,
                start_beat: 0.0,
                length_beats: 1.0,
                velocity: 0.5,
            }],
        });
        let bytes = export_bytes(&document).expect("export");
        let imported = import_bytes(&bytes).expect("import");
        assert_eq!(imported.note_count(), 1);
        assert_eq!(imported.tracks[0].notes[0].pitch, 64);
    }
}
