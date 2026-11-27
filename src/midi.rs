//! Standard MIDI File import and export.
//!
//! Uses `midly` to read/write SMF type-1 files. Notes are paired by channel
//! and key; repeated note-ons on one key are kept as a queue, hanging notes are
//! closed at the end of their track, and the initial tempo, track names and GM
//! programs are mapped into the self-sufficient document. No plugin, synth or
//! external asset is involved.

use std::collections::{HashMap, VecDeque};
use std::fmt;

use midly::{
    Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEventKind,
    num::{u15, u24, u28},
};

use crate::document::{Document, Instrument, Note, Track};

/// Default ticks per quarter note used by exported files.
pub const TICKS_PER_BEAT: u32 = 480;
/// Largest absolute tick representable by an SMF event delta.
pub const MAX_SMF_TICK: u32 = 0x0FFF_FFFF;
/// Maximum input size accepted by the importer.
pub const MAX_IMPORT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum number of SMF tracks accepted by the importer.
pub const MAX_IMPORT_TRACKS: usize = 256;
/// Maximum number of notes accepted by the importer.
pub const MAX_IMPORT_NOTES: usize = 100_000;

/// Failure modes of MIDI import/export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiError {
    /// Input bytes are not a valid SMF file.
    Invalid(String),
    /// The file uses a timing scheme this importer cannot map.
    UnsupportedTiming,
    /// A musical position or input size is outside the supported range.
    OutOfRange(String),
    /// The file could not be written.
    Write(String),
}

impl fmt::Display for MidiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => write!(formatter, "invalid MIDI file: {error}"),
            Self::UnsupportedTiming => formatter.write_str("unsupported MIDI timing"),
            Self::OutOfRange(error) => write!(formatter, "MIDI value out of range: {error}"),
            Self::Write(error) => write!(formatter, "could not write MIDI file: {error}"),
        }
    }
}

impl std::error::Error for MidiError {}

/// Import an SMF byte buffer into a self-sufficient MusicCraft document.
///
/// The document has one tempo and one oscillator per track. Therefore the
/// initial tempo and the last program change in each SMF track are used; MIDI
/// tempo maps and program changes within a track cannot be represented exactly.
///
/// # Errors
///
/// Returns [`MidiError`] for malformed, unsupported, oversized or
/// unrepresentable input.
pub fn import_bytes(bytes: &[u8]) -> Result<Document, MidiError> {
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(MidiError::OutOfRange(format!(
            "input is larger than {MAX_IMPORT_BYTES} bytes"
        )));
    }
    let smf = Smf::parse(bytes).map_err(|error| MidiError::Invalid(error.to_string()))?;
    if smf.tracks.len() > MAX_IMPORT_TRACKS {
        return Err(MidiError::OutOfRange(format!(
            "more than {MAX_IMPORT_TRACKS} tracks"
        )));
    }
    let ticks_per_beat = match smf.header.timing {
        Timing::Metrical(ticks) => u32::from(ticks.as_int()).max(1),
        Timing::Timecode(_, _) => return Err(MidiError::UnsupportedTiming),
    };

    let mut document = Document {
        title: "Imported MIDI".into(),
        bpm: crate::document::DEFAULT_BPM,
        ..Document::default()
    };
    let mut initial_tempo_micros = None;
    let mut note_count = 0_usize;

    for (track_number, midi_track) in smf.tracks.iter().enumerate() {
        let mut name = None;
        let mut program = None;
        let mut active: HashMap<(u8, u8), VecDeque<(u32, f32)>> = HashMap::new();
        let mut notes: Vec<Note> = Vec::new();
        let mut tick = 0_u32;

        for event in midi_track {
            tick = tick.checked_add(event.delta.as_int()).ok_or_else(|| {
                MidiError::OutOfRange(format!("tick overflow in track {track_number}"))
            })?;
            match event.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(micros)) => {
                    let micros = micros.as_int();
                    if initial_tempo_micros.is_none() && micros > 0 {
                        initial_tempo_micros = Some(micros);
                    }
                }
                TrackEventKind::Meta(MetaMessage::TrackName(bytes)) => {
                    name = Some(String::from_utf8_lossy(bytes).into_owned());
                }
                TrackEventKind::Midi {
                    channel: _,
                    message: MidiMessage::ProgramChange { program: value },
                } => {
                    program = Some(value.as_int());
                }
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOn { key, vel },
                } => {
                    let key = u8::from(key);
                    let velocity = f32::from(vel.as_int()) / 127.0;
                    if velocity == 0.0 {
                        close_note(
                            &mut active,
                            u8::from(channel),
                            key,
                            tick,
                            &mut notes,
                            &mut document,
                            &mut note_count,
                            ticks_per_beat,
                        )?;
                    } else {
                        active
                            .entry((u8::from(channel), key))
                            .or_default()
                            .push_back((tick, velocity.clamp(0.0, 1.0)));
                    }
                }
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOff { key, .. },
                } => {
                    close_note(
                        &mut active,
                        u8::from(channel),
                        u8::from(key),
                        tick,
                        &mut notes,
                        &mut document,
                        &mut note_count,
                        ticks_per_beat,
                    )?;
                }
                _ => {}
            }
        }

        // Preserve hanging notes instead of silently dropping them. Their held
        // duration ends at the final event in the SMF track.
        for ((_channel, key), queue) in active {
            for (start_tick, velocity) in queue {
                push_note(
                    &mut notes,
                    &mut document,
                    key,
                    start_tick,
                    tick,
                    velocity,
                    ticks_per_beat,
                    &mut note_count,
                )?;
            }
        }

        if notes.is_empty() && name.is_none() && program.is_none() {
            continue;
        }
        notes.sort_by(|a, b| {
            a.start_beat
                .total_cmp(&b.start_beat)
                .then_with(|| a.pitch.cmp(&b.pitch))
                .then_with(|| a.length_beats.total_cmp(&b.length_beats))
        });
        let fallback = Instrument::all()[document.tracks.len() % Instrument::all().len()];
        let track = Track {
            id: document.allocate_id(),
            name: name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| document.tracks.len().saturating_add(1).to_string()),
            instrument: program.map_or(fallback, Instrument::from_midi_program),
            volume: 0.8,
            mute: false,
            solo: false,
            effect: crate::document::Effect::None,
            notes,
        };
        document.tracks.push(track);
    }

    if let Some(micros) = initial_tempo_micros {
        document.bpm = (60_000_000.0 / f64::from(micros)).clamp(20.0, 300.0);
    }
    Ok(document)
}

#[allow(clippy::too_many_arguments)]
fn close_note(
    active: &mut HashMap<(u8, u8), VecDeque<(u32, f32)>>,
    channel: u8,
    key: u8,
    end_tick: u32,
    notes: &mut Vec<Note>,
    document: &mut Document,
    note_count: &mut usize,
    ticks_per_beat: u32,
) -> Result<(), MidiError> {
    let note_key = (channel, key);
    let Some(queue) = active.get_mut(&note_key) else {
        return Ok(());
    };
    let Some((start_tick, velocity)) = queue.pop_front() else {
        return Ok(());
    };
    if queue.is_empty() {
        active.remove(&note_key);
    }
    push_note(
        notes,
        document,
        key,
        start_tick,
        end_tick,
        velocity,
        ticks_per_beat,
        note_count,
    )
}

#[allow(clippy::too_many_arguments)]
fn push_note(
    notes: &mut Vec<Note>,
    document: &mut Document,
    pitch: u8,
    start_tick: u32,
    end_tick: u32,
    velocity: f32,
    ticks_per_beat: u32,
    note_count: &mut usize,
) -> Result<(), MidiError> {
    if *note_count >= MAX_IMPORT_NOTES {
        return Err(MidiError::OutOfRange(format!(
            "more than {MAX_IMPORT_NOTES} notes"
        )));
    }
    let start_beat = f64::from(start_tick) / f64::from(ticks_per_beat);
    let length_beats = f64::from(end_tick.saturating_sub(start_tick)) / f64::from(ticks_per_beat);
    let note = Note {
        id: document.allocate_id(),
        pitch,
        start_beat,
        length_beats: length_beats.max(crate::edit::MIN_LENGTH),
        velocity,
    };
    if !note.is_valid() {
        return Err(MidiError::OutOfRange(format!(
            "note at tick {start_tick} exceeds the supported beat range"
        )));
    }
    *note_count += 1;
    notes.push(note);
    Ok(())
}

/// Export a document as an SMF type-1 byte buffer.
///
/// Silent zero-velocity notes are omitted because SMF uses zero-velocity
/// note-ons as note-offs. Invalid or unrepresentable notes fail the export
/// instead of being silently truncated.
///
/// # Errors
///
/// Returns [`MidiError::OutOfRange`] for unsupported note positions or
/// [`MidiError::Write`] if encoding fails.
pub fn export_bytes(document: &Document) -> Result<Vec<u8>, MidiError> {
    let header = Header::new(
        Format::Parallel,
        Timing::Metrical(u15::new(TICKS_PER_BEAT as u16)),
    );
    let mut smf = Smf::new(header);

    let micros = (60_000_000.0 / document.effective_bpm())
        .round()
        .clamp(1.0, f64::from(u24::max_value().as_int())) as u32;
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
                    program: track.instrument.midi_program().into(),
                },
            },
        ));
        for note in &track.notes {
            if !note.is_valid() {
                return Err(MidiError::OutOfRange(format!(
                    "note {} is invalid: {}",
                    note.id,
                    note.validate().err().map_or_else(
                        || "unknown validation error".to_owned(),
                        |error| error.to_string()
                    )
                )));
            }
            if note.velocity <= 0.0 {
                continue;
            }
            let start = note_tick(note.start_beat, note.id)?;
            let end = note_tick(note.end_beat(), note.id)?.max(start.checked_add(1).ok_or_else(
                || MidiError::OutOfRange(format!("note {} ends at the SMF tick limit", note.id)),
            )?);
            let key = note.pitch.into();
            let vel = (note.velocity * 127.0).round().clamp(1.0, 127.0) as u8;
            // At a shared boundary, close the old note before opening the new
            // one. Otherwise same-key notes overwrite each other on import.
            events.push((
                end,
                2,
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOff {
                        key,
                        vel: 0_u8.into(),
                    },
                },
            ));
            events.push((
                start,
                3,
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOn {
                        key,
                        vel: vel.into(),
                    },
                },
            ));
        }
        events.sort_by_key(|event| (event.0, event.1));

        let mut out = Vec::with_capacity(events.len() + 1);
        let mut previous = 0_u32;
        for (tick, _, kind) in events {
            out.push(midly::TrackEvent {
                delta: u28::new(tick.checked_sub(previous).ok_or_else(|| {
                    MidiError::OutOfRange("MIDI events are not in tick order".to_owned())
                })?),
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

fn note_tick(beat: f64, note_id: u64) -> Result<u32, MidiError> {
    let tick = (beat * f64::from(TICKS_PER_BEAT)).round();
    if !beat.is_finite() || beat < 0.0 || tick > f64::from(MAX_SMF_TICK) {
        return Err(MidiError::OutOfRange(format!(
            "note {note_id} is outside the SMF tick range"
        )));
    }
    Ok(tick as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Effect;
    use std::collections::HashSet;

    fn event(delta: u32, kind: TrackEventKind<'_>) -> midly::TrackEvent<'_> {
        midly::TrackEvent {
            delta: u28::new(delta),
            kind,
        }
    }

    fn import_events(tracks: Vec<Vec<midly::TrackEvent<'_>>>) -> Document {
        let mut smf = Smf::new(Header::new(
            Format::Parallel,
            Timing::Metrical(u15::new(TICKS_PER_BEAT as u16)),
        ));
        smf.tracks = tracks;
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).expect("write SMF");
        import_bytes(&bytes).expect("import SMF")
    }

    fn note_on(key: u8, velocity: u8) -> TrackEventKind<'static> {
        TrackEventKind::Midi {
            channel: 0_u8.into(),
            message: MidiMessage::NoteOn {
                key: key.into(),
                vel: velocity.into(),
            },
        }
    }

    fn note_off(key: u8) -> TrackEventKind<'static> {
        TrackEventKind::Midi {
            channel: 0_u8.into(),
            message: MidiMessage::NoteOff {
                key: key.into(),
                vel: 0_u8.into(),
            },
        }
    }

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
    fn every_builtin_oscillator_round_trips_through_its_gm_program() {
        for instrument in Instrument::all() {
            let mut document = Document::sample();
            document.tracks.truncate(1);
            document.tracks[0].instrument = instrument;
            let bytes = export_bytes(&document).expect("export");
            let imported = import_bytes(&bytes).expect("import");
            assert_eq!(imported.tracks[0].instrument, instrument);
        }
    }

    #[test]
    fn invalid_bytes_are_rejected_without_panicking() {
        assert!(import_bytes(b"not a midi file").is_err());
        assert!(import_bytes(&[]).is_err());
        assert!(import_bytes(&[0xFF, 0xFF]).is_err());
        assert!(matches!(
            import_bytes(&vec![0_u8; MAX_IMPORT_BYTES + 1]),
            Err(MidiError::OutOfRange(_))
        ));
    }

    #[test]
    fn note_off_with_zero_velocity_closes_the_note() {
        let imported = import_events(vec![vec![
            event(0, note_on(64, 100)),
            event(TICKS_PER_BEAT, note_on(64, 0)),
            event(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ]]);
        assert_eq!(imported.note_count(), 1);
        assert_eq!(imported.tracks[0].notes[0].pitch, 64);
        assert!((imported.tracks[0].notes[0].length_beats - 1.0).abs() < 1e-9);
    }

    #[test]
    fn repeated_note_ons_on_one_key_are_paired_fifo() {
        let imported = import_events(vec![vec![
            event(0, note_on(60, 100)),
            event(120, note_on(60, 80)),
            event(360, note_off(60)),
            event(480, note_off(60)),
            event(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ]]);
        let notes = &imported.tracks[0].notes;
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].start_beat, 0.0);
        assert_eq!(notes[0].length_beats, 1.0);
        assert_eq!(notes[1].start_beat, 0.25);
        assert_eq!(notes[1].length_beats, 1.75);
    }

    #[test]
    fn hanging_notes_are_closed_at_the_track_end() {
        let imported = import_events(vec![vec![
            event(0, note_on(67, 90)),
            event(
                2 * TICKS_PER_BEAT,
                TrackEventKind::Meta(MetaMessage::EndOfTrack),
            ),
        ]]);
        assert_eq!(imported.note_count(), 1);
        assert_eq!(imported.tracks[0].notes[0].length_beats, 2.0);
    }

    #[test]
    fn adjacent_same_key_notes_survive_export_and_import() {
        let mut document = Document::sample();
        document.tracks.truncate(1);
        document.tracks[0].notes = vec![
            Note {
                id: 10,
                pitch: 60,
                start_beat: 0.0,
                length_beats: 1.0,
                velocity: 0.8,
            },
            Note {
                id: 11,
                pitch: 60,
                start_beat: 1.0,
                length_beats: 1.0,
                velocity: 0.7,
            },
        ];
        let bytes = export_bytes(&document).expect("export");
        let imported = import_bytes(&bytes).expect("import");
        assert_eq!(imported.note_count(), 2);
        assert_eq!(imported.tracks[0].notes[0].start_beat, 0.0);
        assert_eq!(imported.tracks[0].notes[0].length_beats, 1.0);
        assert_eq!(imported.tracks[0].notes[1].start_beat, 1.0);
        assert_eq!(imported.tracks[0].notes[1].length_beats, 1.0);
    }

    #[test]
    fn programs_names_empty_tracks_and_unique_ids_are_preserved() {
        let imported = import_events(vec![
            vec![event(0, TrackEventKind::Meta(MetaMessage::EndOfTrack))],
            vec![
                event(0, TrackEventKind::Meta(MetaMessage::TrackName(b"Empty"))),
                event(
                    0,
                    TrackEventKind::Midi {
                        channel: 0_u8.into(),
                        message: MidiMessage::ProgramChange {
                            program: 40_u8.into(),
                        },
                    },
                ),
                event(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
            ],
            vec![
                event(0, TrackEventKind::Meta(MetaMessage::TrackName(b"Lead"))),
                event(
                    0,
                    TrackEventKind::Midi {
                        channel: 0_u8.into(),
                        message: MidiMessage::ProgramChange {
                            program: 80_u8.into(),
                        },
                    },
                ),
                event(0, note_on(64, 100)),
                event(TICKS_PER_BEAT, note_off(64)),
                event(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
            ],
        ]);
        assert_eq!(imported.tracks.len(), 2);
        assert_eq!(imported.tracks[0].name, "Empty");
        assert_eq!(imported.tracks[0].instrument, Instrument::Saw);
        assert_eq!(imported.tracks[1].name, "Lead");
        assert_eq!(imported.tracks[1].instrument, Instrument::Square);
        let ids: HashSet<u64> = imported
            .tracks
            .iter()
            .flat_map(|track| std::iter::once(track.id).chain(track.notes.iter().map(|n| n.id)))
            .collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn initial_tempo_wins_over_later_tempo_events() {
        let imported = import_events(vec![vec![
            event(
                0,
                TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000))),
            ),
            event(
                TICKS_PER_BEAT,
                TrackEventKind::Meta(MetaMessage::Tempo(u24::new(250_000))),
            ),
            event(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ]]);
        assert_eq!(imported.bpm, 120.0);
    }

    #[test]
    fn zero_velocity_notes_do_not_become_audible_midi_notes() {
        let mut document = Document::sample();
        document.tracks.truncate(1);
        document.tracks[0].notes = vec![Note {
            id: 2,
            pitch: 60,
            start_beat: 0.0,
            length_beats: 1.0,
            velocity: 0.0,
        }];
        document.tracks[0].effect = Effect::None;
        let bytes = export_bytes(&document).expect("export");
        let smf = Smf::parse(&bytes).expect("parse");
        assert!(!smf.tracks[1].iter().any(|event| {
            matches!(
                event.kind,
                TrackEventKind::Midi {
                    message: MidiMessage::NoteOn { .. } | MidiMessage::NoteOff { .. },
                    ..
                }
            )
        }));
        assert_eq!(import_bytes(&bytes).expect("import").note_count(), 0);
    }

    #[test]
    fn unrepresentable_notes_fail_export_instead_of_wrapping() {
        let mut document = Document::sample();
        document.tracks.truncate(1);
        document.tracks[0].notes = vec![Note {
            id: 2,
            pitch: 60,
            start_beat: f64::from(MAX_SMF_TICK) / f64::from(TICKS_PER_BEAT),
            length_beats: 1.0,
            velocity: 0.8,
        }];
        assert!(matches!(
            export_bytes(&document),
            Err(MidiError::OutOfRange(_))
        ));
    }
}
