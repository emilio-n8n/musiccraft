//! Offline synthesis engine.
//!
//! Rendering is deterministic and UI-free: beats are converted to seconds at
//! the document tempo, each audible track is rendered into its own mono buffer,
//! its insert effect is applied, and the tracks are summed at 44.1 kHz before
//! the master gain and the final `[-1, 1]` clamp. No audio device is used here.

use std::fmt;
use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};

use crate::document::{Document, Effect, Instrument, Note, Track};

/// Frames per second of every rendered buffer and exported WAV file.
pub const SAMPLE_RATE: u32 = 44_100;
/// Tail kept after the last note so release envelopes are not truncated.
pub const RELEASE_SECONDS: f64 = 0.18;
/// Longest arrangement accepted by the offline renderer, including its tail.
pub const MAX_RENDER_SECONDS: f64 = 600.0;
/// Maximum aggregate mono-buffer memory used by a render.
pub const MAX_RENDER_BUFFER_BYTES: usize = 512 * 1024 * 1024;
/// Frames rendered between two progress/cancellation checks.
const CHUNK_FRAMES: usize = 4096;
/// Upper bound of roughly ten minutes, guarding against absurd durations.
#[cfg(test)]
const MAX_FRAMES: usize = 26_460_000;

/// Failure modes of the offline renderer and WAV writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// The caller asked the render to stop.
    Cancelled,
    /// The arrangement exceeds the bounded render duration.
    TooLong,
    /// The audible track buffers exceed the bounded render memory.
    ResourceLimit,
    /// The WAV file could not be written.
    Wav(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("render cancelled"),
            Self::TooLong => formatter.write_str(
                "arrangement exceeds the 10 minute render limit; shorten it or increase the tempo",
            ),
            Self::ResourceLimit => formatter.write_str(
                "arrangement exceeds the 512 MiB render buffer limit; shorten it or reduce audible tracks",
            ),
            Self::Wav(error) => write!(formatter, "WAV export failed: {error}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// Equal-temperament frequency of a MIDI pitch (A4 = 440 Hz).
#[must_use]
pub fn note_frequency(pitch: u8) -> f64 {
    440.0 * 2.0_f64.powf((f64::from(pitch) - 69.0) / 12.0)
}

/// Oscillator value for a phase expressed in cycles.
#[must_use]
pub fn oscillator(instrument: Instrument, phase_cycles: f64) -> f64 {
    if !phase_cycles.is_finite() {
        return 0.0;
    }
    let phase = phase_cycles.rem_euclid(1.0);
    match instrument {
        Instrument::Sine => (std::f64::consts::TAU * phase).sin(),
        Instrument::Square => {
            if phase < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        Instrument::Saw => 2.0 * phase - 1.0,
        Instrument::Triangle => 4.0 * (phase - 0.5).abs() - 1.0,
        Instrument::Noise => {
            // Deterministic hash of the phase: reproducible noise, no RNG state.
            let value = (phase * 12_989.0 + 0.5).sin() * 43_758.545_3;
            value.rem_euclid(1.0).mul_add(2.0, -1.0)
        }
    }
}

/// Simple four-stage amplitude envelope, in seconds except `sustain`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Adsr {
    /// Attack time.
    pub attack: f64,
    /// Decay time.
    pub decay: f64,
    /// Sustain level in `0.0..=1.0`.
    pub sustain: f64,
    /// Release time.
    pub release: f64,
}

impl Default for Adsr {
    fn default() -> Self {
        Self {
            attack: 0.008,
            decay: 0.07,
            sustain: 0.7,
            release: RELEASE_SECONDS,
        }
    }
}

impl Adsr {
    /// Envelope amplitude at `elapsed` seconds into a note held for `held_seconds`.
    #[must_use]
    pub fn level(&self, elapsed: f64, held_seconds: f64) -> f64 {
        if !elapsed.is_finite() || elapsed < 0.0 || !held_seconds.is_finite() || held_seconds < 0.0
        {
            return 0.0;
        }
        if elapsed < held_seconds {
            return self.held_level(elapsed);
        }
        let release = if self.release.is_finite() {
            self.release.max(0.0)
        } else {
            0.0
        };
        if release == 0.0 {
            return 0.0;
        }
        let progress = (elapsed - held_seconds) / release;
        if progress >= 1.0 - f64::EPSILON {
            return 0.0;
        }
        // Short notes may release before attack or decay has finished. Start
        // from the actual hold boundary level instead of jumping to sustain.
        self.held_level(held_seconds) * (1.0 - progress)
    }

    fn held_level(&self, elapsed: f64) -> f64 {
        let attack = if self.attack.is_finite() {
            self.attack.max(0.0)
        } else {
            0.0
        };
        let decay = if self.decay.is_finite() {
            self.decay.max(0.0)
        } else {
            0.0
        };
        let sustain = if self.sustain.is_finite() {
            self.sustain.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if elapsed < attack {
            return if attack > 0.0 { elapsed / attack } else { 1.0 };
        }
        let after_attack = elapsed - attack;
        if after_attack < decay {
            return if decay > 0.0 {
                1.0 - (1.0 - sustain) * (after_attack / decay)
            } else {
                sustain
            };
        }
        sustain
    }
}

/// Number of mono frames needed for the arrangement plus its release tail.
///
/// The result is capped at [`MAX_RENDER_SECONDS`]. Rendering reports
/// [`RenderError::TooLong`] instead of silently exporting a truncated file.
#[must_use]
#[cfg(test)]
pub fn frame_count(document: &Document) -> usize {
    checked_frame_count(document).unwrap_or(MAX_FRAMES)
}

fn checked_frame_count(document: &Document) -> Result<usize, RenderError> {
    let beats = document.duration_beats();
    if !beats.is_finite() || beats <= 0.0 {
        return Ok(0);
    }
    let seconds = document.duration_seconds() + RELEASE_SECONDS;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Ok(0);
    }
    if seconds > MAX_RENDER_SECONDS {
        return Err(RenderError::TooLong);
    }
    Ok((seconds * f64::from(SAMPLE_RATE)).ceil() as usize)
}

fn validate_render_resources(
    total_frames: usize,
    audible_tracks: usize,
) -> Result<(), RenderError> {
    // One buffer per audible track plus the output buffer. Delay/reverb scratch
    // space replaces, rather than adds to, the final output allocation.
    let buffers = audible_tracks.saturating_add(1);
    let bytes = total_frames
        .checked_mul(buffers)
        .and_then(|samples| samples.checked_mul(std::mem::size_of::<f32>()))
        .ok_or(RenderError::ResourceLimit)?;
    if bytes > MAX_RENDER_BUFFER_BYTES {
        return Err(RenderError::ResourceLimit);
    }
    Ok(())
}

fn track_gain(document: &Document) -> f32 {
    // Mute and solo select tracks; they must not change the gain of the tracks
    // that remain audible. Per-track volume and master output stay independent.
    Track::clamp_volume(document.master_volume)
}

fn clamp_effect_level(value: f32, maximum: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, maximum)
    } else {
        0.0
    }
}

/// Apply a simple insert effect to a mono track buffer in place.
#[cfg(test)]
pub fn apply_effect(samples: &mut [f32], effect: &Effect, seconds_per_beat: f64) {
    let _ = apply_effect_with_cancellation(samples, effect, seconds_per_beat, &mut || false);
}

fn apply_effect_with_cancellation<C>(
    samples: &mut [f32],
    effect: &Effect,
    seconds_per_beat: f64,
    cancelled: &mut C,
) -> Result<(), RenderError>
where
    C: FnMut() -> bool,
{
    match *effect {
        Effect::None => {}
        Effect::Delay {
            time_beats,
            feedback,
            mix,
        } => {
            if samples.is_empty()
                || !time_beats.is_finite()
                || time_beats <= 0.0
                || !seconds_per_beat.is_finite()
                || seconds_per_beat <= 0.0
            {
                return Ok(());
            }
            let delay_frames = ((time_beats * seconds_per_beat * f64::from(SAMPLE_RATE)).round()
                as usize)
                .clamp(1, samples.len());
            let feedback = clamp_effect_level(feedback, 0.9);
            let mix = clamp_effect_level(mix, 1.0);
            let mut line = vec![0.0_f32; delay_frames];
            for (index, sample) in samples.iter_mut().enumerate() {
                if index.is_multiple_of(CHUNK_FRAMES) && cancelled() {
                    return Err(RenderError::Cancelled);
                }
                let slot = index % delay_frames;
                let delayed = line[slot];
                let dry = *sample;
                line[slot] = dry + feedback * delayed;
                *sample = dry + mix * delayed;
            }
        }
        Effect::Reverb { decay, mix } => {
            if samples.is_empty() {
                return Ok(());
            }
            let decay = clamp_effect_level(decay, 0.95);
            let mix = clamp_effect_level(mix, 1.0);
            let bases = [0.0297_f64, 0.0371, 0.0411, 0.0437];
            let mut lines: Vec<Vec<f32>> = bases
                .iter()
                .map(|base| {
                    vec![0.0_f32; ((base * f64::from(SAMPLE_RATE)).round() as usize).max(1)]
                })
                .collect();
            let mut wet = vec![0.0_f32; samples.len()];
            for index in 0..samples.len() {
                if index.is_multiple_of(CHUNK_FRAMES) && cancelled() {
                    return Err(RenderError::Cancelled);
                }
                let mut sum = 0.0_f32;
                for line in &mut lines {
                    let slot = index % line.len();
                    let delayed = line[slot];
                    line[slot] = samples[index] + decay * delayed;
                    sum += delayed;
                }
                wet[index] = sum / lines.len() as f32;
            }
            for (index, (sample, echo)) in samples.iter_mut().zip(wet).enumerate() {
                if index.is_multiple_of(CHUNK_FRAMES) && cancelled() {
                    return Err(RenderError::Cancelled);
                }
                *sample += mix * echo;
            }
        }
    }
    Ok(())
}

/// Render one track into a full-length buffer (velocity included, no volume).
fn render_track<C>(
    buffer: &mut [f32],
    track: &Track,
    seconds_per_beat: f64,
    cancelled: &mut C,
) -> Result<(), RenderError>
where
    C: FnMut() -> bool,
{
    let sample_rate = f64::from(SAMPLE_RATE);
    let envelope = Adsr::default();
    for note in &track.notes {
        if !note.is_valid() {
            continue;
        }
        mix_note(
            buffer,
            0,
            note,
            track.instrument,
            f64::from(Note::clamp_velocity(note.velocity)),
            seconds_per_beat,
            sample_rate,
            &envelope,
            cancelled,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn mix_note<C>(
    output: &mut [f32],
    offset: usize,
    note: &Note,
    instrument: Instrument,
    velocity: f64,
    seconds_per_beat: f64,
    sample_rate: f64,
    envelope: &Adsr,
    cancelled: &mut C,
) -> Result<(), RenderError>
where
    C: FnMut() -> bool,
{
    let frequency = note_frequency(note.pitch);
    let onset_seconds = note.start_beat * seconds_per_beat;
    let held_seconds = note.length_beats * seconds_per_beat;
    let onset_frame = (onset_seconds * sample_rate).round() as usize;
    let release_end = onset_seconds + held_seconds + envelope.release;
    let end_frame = (release_end * sample_rate).ceil() as usize;

    let first = onset_frame.max(offset);
    let last = end_frame.min(offset.saturating_add(output.len()));
    if first >= last {
        return Ok(());
    }

    for frame in first..last {
        if (frame - first).is_multiple_of(CHUNK_FRAMES) && cancelled() {
            return Err(RenderError::Cancelled);
        }
        let elapsed = (frame - onset_frame) as f64 / sample_rate;
        let phase = frequency * elapsed;
        let amplitude = oscillator(instrument, phase) * envelope.level(elapsed, held_seconds);
        output[frame - offset] += (amplitude * velocity) as f32;
    }
    Ok(())
}

fn clamp_buffer(samples: &mut [f32]) {
    for sample in samples {
        if sample.is_finite() {
            *sample = sample.clamp(-1.0, 1.0);
        } else {
            *sample = 0.0;
        }
    }
}

/// Render with per-chunk progress and cooperative cancellation.
///
/// # Errors
///
/// Returns [`RenderError::Cancelled`] when `cancelled` becomes true, including
/// while rendering notes or effects. [`RenderError::TooLong`] and
/// [`RenderError::ResourceLimit`] are returned before allocation when the
/// arrangement exceeds the duration or aggregate buffer limit. The progress
/// callback receives fractions in `0.0..=1.0`.
pub fn render_with_progress<F, C>(
    document: &Document,
    mut progress: F,
    mut cancelled: C,
) -> Result<Vec<f32>, RenderError>
where
    F: FnMut(f32),
    C: FnMut() -> bool,
{
    let total = checked_frame_count(document)?;
    if total == 0 {
        return Ok(Vec::new());
    }
    let seconds_per_beat = 60.0 / document.effective_bpm();
    let any_solo = document.has_solo();
    let gain = track_gain(document);
    let audible: Vec<&Track> = document
        .tracks
        .iter()
        .filter(|track| track.is_audible_with(any_solo) && Track::clamp_volume(track.volume) > 0.0)
        .collect();
    validate_render_resources(total, audible.len())?;
    let track_total = audible.len().max(1) as f32;

    // Insert effects need the whole track before it joins the mix.
    let mut tracks: Vec<(Vec<f32>, f32)> = Vec::new();
    for (index, track) in audible.iter().enumerate() {
        if cancelled() {
            return Err(RenderError::Cancelled);
        }
        let volume = Track::clamp_volume(track.volume);
        let mut buffer = vec![0.0_f32; total];
        render_track(&mut buffer, track, seconds_per_beat, &mut cancelled)?;
        progress((index as f32 + 0.5) / track_total * 0.8);
        apply_effect_with_cancellation(
            &mut buffer,
            &track.effect,
            seconds_per_beat,
            &mut cancelled,
        )?;
        progress((index as f32 + 1.0) / track_total * 0.8);
        tracks.push((buffer, volume));
    }

    let mut output = vec![0.0_f32; total];
    let mut offset = 0;
    while offset < total {
        if cancelled() {
            return Err(RenderError::Cancelled);
        }
        let end = (offset + CHUNK_FRAMES).min(total);
        for (buffer, volume) in &tracks {
            for (destination, source) in output[offset..end]
                .iter_mut()
                .zip(buffer[offset..end].iter())
            {
                *destination += source * volume * gain;
            }
        }
        progress(0.8 + end as f32 / total as f32 * 0.2);
        offset = end;
    }
    clamp_buffer(&mut output);
    progress(1.0);
    Ok(output)
}

/// Render the whole arrangement without progress reporting.
///
/// Returns an empty buffer when rendering is cancelled or the arrangement is
/// too long. Use [`render_with_progress`] when the failure must be surfaced.
#[must_use]
#[cfg(test)]
pub fn render(document: &Document) -> Vec<f32> {
    render_with_progress(document, |_| {}, || false).unwrap_or_default()
}

/// Export mono float32 samples as a 44.1 kHz WAV file.
///
/// Non-finite samples are written as silence and finite samples are clamped to
/// `[-1, 1]`, so even a caller-supplied buffer cannot create an invalid WAV.
///
/// # Errors
///
/// Returns [`RenderError::Wav`] when the file cannot be created, written or
/// finalized.
pub fn write_wav(path: &Path, samples: &[f32]) -> Result<(), RenderError> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut writer =
        WavWriter::create(path, spec).map_err(|error| RenderError::Wav(error.to_string()))?;
    for &sample in samples {
        let sample = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        writer
            .write_sample(sample)
            .map_err(|error| RenderError::Wav(error.to_string()))?;
    }
    writer
        .finalize()
        .map_err(|error| RenderError::Wav(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use craftkit_jobs::JobRunner;
    use hound::WavReader;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static WAV_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_wav_path() -> std::path::PathBuf {
        let count = WAV_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "musiccraft-mixdown-{}-{count}.wav",
            std::process::id()
        ))
    }

    fn one_note_document() -> Document {
        let mut document = Document::sample();
        document.tracks.truncate(1);
        document.tracks[0].notes.truncate(1);
        document
    }

    #[test]
    fn oscillator_frequency_follows_equal_temperament() {
        assert!((note_frequency(69) - 440.0).abs() < 1e-9);
        assert!((note_frequency(81) - 880.0).abs() < 1e-9);
        assert!((note_frequency(57) - 220.0).abs() < 1e-9);
        assert!((note_frequency(60) - 261.6255653).abs() < 1e-6);
    }

    #[test]
    fn oscillator_shapes_are_bounded() {
        for instrument in Instrument::all() {
            for step in 0..200 {
                let value = oscillator(instrument, step as f64 / 200.0);
                assert!((-1.0..=1.0).contains(&value), "{instrument:?}");
            }
            assert_eq!(oscillator(instrument, f64::NAN), 0.0);
            assert_eq!(oscillator(instrument, f64::INFINITY), 0.0);
        }
        assert!((oscillator(Instrument::Sine, 0.25) - 1.0).abs() < 1e-9);
        assert_eq!(oscillator(Instrument::Square, 0.25), 1.0);
        assert!((oscillator(Instrument::Saw, 0.5)).abs() < 1e-9);
    }

    #[test]
    fn adsr_starts_and_ends_at_zero() {
        let envelope = Adsr::default();
        assert_eq!(envelope.level(0.0, 1.0), 0.0);
        assert!(envelope.level(0.004, 1.0) > 0.0);
        assert!(envelope.level(0.5, 1.0) > 0.0);
        assert_eq!(envelope.level(1.5, 1.0), 0.0);
    }

    #[test]
    fn adsr_release_is_continuous_for_short_notes() {
        let envelope = Adsr {
            attack: 0.1,
            decay: 0.2,
            sustain: 0.5,
            release: 0.2,
        };
        let held = 0.15;
        let before = envelope.level(held - 1e-6, held);
        let boundary = envelope.level(held, held);
        let after = envelope.level(held + 1e-6, held);
        assert!(
            boundary > 0.5,
            "release must start at the actual held level"
        );
        assert!((before - boundary).abs() < 1e-5);
        assert!((boundary - after).abs() < 1e-5);
        assert_eq!(envelope.level(held + envelope.release, held), 0.0);
    }

    #[test]
    fn render_is_not_silent_and_does_not_clip() {
        let document = Document::sample();
        let samples = render(&document);
        assert_eq!(samples.len(), frame_count(&document));
        assert!(!samples.is_empty());
        assert!(samples.iter().all(|sample| sample.is_finite()));
        let peak = samples
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 0.0, "the demo arrangement must produce sound");
        assert!(peak <= 1.0, "the mix must never clip");
        assert!(render(&Document::default()).is_empty());
    }

    #[test]
    fn muted_tracks_do_not_contribute() {
        let mut document = Document::sample();
        let before = render(&document);
        for track in &mut document.tracks {
            track.mute = true;
        }
        let after = render(&document);
        assert!(before.iter().any(|sample| *sample != 0.0));
        assert!(after.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn muting_one_track_does_not_change_another_tracks_gain() {
        let mut document = Document::sample();
        document.tracks.truncate(2);
        document.tracks[0].notes.truncate(1);
        document.tracks[1].notes = vec![Note {
            id: 100,
            pitch: 64,
            start_beat: 2.0,
            length_beats: 1.0,
            velocity: 0.8,
        }];
        let all = render(&document);
        document.tracks[0].mute = true;
        let muted = render(&document);
        let frame = (1.25 * f64::from(SAMPLE_RATE)) as usize;
        assert_eq!(all[frame], muted[frame]);
    }

    #[test]
    fn solo_isolates_the_soloed_track() {
        let mut document = Document::sample();
        let all = render(&document);
        document.tracks[1].solo = true;
        let soloed = render(&document);
        assert!(soloed.iter().any(|sample| *sample != 0.0));
        assert_ne!(all, soloed);
        let peak = soloed
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 0.0 && peak <= 1.0);
    }

    #[test]
    fn master_volume_scales_the_output() {
        let mut document = Document::sample();
        let full = render(&document);
        document.master_volume = 0.0;
        let silent = render(&document);
        assert!(full.iter().any(|sample| *sample != 0.0));
        assert!(silent.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn delay_and_reverb_change_the_mix_without_clipping() {
        let document = Document::sample();
        let dry = render(&document);
        let mut delayed = document.clone();
        delayed.tracks[0].effect = Effect::Delay {
            time_beats: 0.25,
            feedback: 0.4,
            mix: 0.5,
        };
        let with_delay = render(&delayed);
        assert_ne!(dry, with_delay);

        let mut reverbed = document;
        reverbed.tracks[2].effect = Effect::Reverb {
            decay: 0.7,
            mix: 0.5,
        };
        let with_reverb = render(&reverbed);
        assert_ne!(dry, with_reverb);
        for sample in with_delay.iter().chain(with_reverb.iter()) {
            assert!(sample.is_finite());
            assert!(sample.abs() <= 1.0);
        }
    }

    #[test]
    fn corrupt_effect_parameters_cannot_create_non_finite_samples() {
        let mut samples = vec![0.5_f32; 10_000];
        apply_effect(
            &mut samples,
            &Effect::Delay {
                time_beats: 0.25,
                feedback: f32::NAN,
                mix: f32::INFINITY,
            },
            0.5,
        );
        assert!(samples.iter().all(|sample| sample.is_finite()));
        apply_effect(
            &mut samples,
            &Effect::Reverb {
                decay: f32::NAN,
                mix: f32::NEG_INFINITY,
            },
            0.5,
        );
        assert!(samples.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn render_reports_progress_and_honours_cancellation() {
        let document = Document::sample();
        let mut fractions = Vec::new();
        let samples =
            render_with_progress(&document, |fraction| fractions.push(fraction), || false)
                .expect("render");
        assert!(!samples.is_empty());
        assert!(fractions.windows(2).all(|pair| pair[1] >= pair[0]));
        assert!((fractions.last().copied().unwrap_or(0.0) - 1.0).abs() < 1e-6);

        let mut checks = 0;
        let cancelled = render_with_progress(
            &document,
            |_| {},
            || {
                checks += 1;
                checks > 1
            },
        );
        assert!(matches!(cancelled, Err(RenderError::Cancelled)));
    }

    #[test]
    fn cancellation_is_checked_inside_long_note_and_effect_rendering() {
        let mut document = one_note_document();
        document.bpm = 300.0;
        document.tracks[0].notes[0].length_beats = 300.0;
        document.tracks[0].effect = Effect::Reverb {
            decay: 0.7,
            mix: 0.5,
        };
        let mut checks = 0;
        let result = render_with_progress(
            &document,
            |_| {},
            || {
                checks += 1;
                checks > 20
            },
        );
        assert!(matches!(result, Err(RenderError::Cancelled)));
        assert!(checks > 20);
    }

    #[test]
    fn oversized_arrangements_fail_instead_of_exporting_a_truncated_mix() {
        let mut document = one_note_document();
        document.bpm = 20.0;
        document.tracks[0].notes[0].length_beats = 300.0;
        assert_eq!(frame_count(&document), MAX_FRAMES);
        assert!(matches!(
            render_with_progress(&document, |_| {}, || false),
            Err(RenderError::TooLong)
        ));
    }

    #[test]
    fn render_buffer_memory_is_bounded_before_allocation() {
        assert!(validate_render_resources(MAX_FRAMES, 3).is_ok());
        assert!(matches!(
            validate_render_resources(MAX_FRAMES, 5),
            Err(RenderError::ResourceLimit)
        ));
        assert!(matches!(
            validate_render_resources(usize::MAX, 1),
            Err(RenderError::ResourceLimit)
        ));
    }

    #[test]
    fn wav_export_is_valid_float32_mono() {
        let document = Document::sample();
        let samples = render(&document);
        let path = temp_wav_path();
        write_wav(&path, &samples).expect("write wav");

        let mut reader = WavReader::open(&path).expect("open wav");
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, SAMPLE_RATE);
        assert_eq!(spec.bits_per_sample, 32);
        assert_eq!(spec.sample_format, SampleFormat::Float);
        let exported: Vec<f32> = reader
            .samples::<f32>()
            .map(|sample| sample.expect("sample"))
            .collect();
        assert_eq!(exported.len(), samples.len());
        assert!(exported.iter().zip(samples.iter()).all(|(a, b)| a == b));
        drop(reader);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wav_export_sanitizes_non_finite_and_out_of_range_samples() {
        let path = temp_wav_path();
        write_wav(&path, &[f32::NAN, f32::INFINITY, 2.0, -2.0]).expect("write wav");
        let mut reader = WavReader::open(&path).expect("open wav");
        let exported: Vec<f32> = reader
            .samples::<f32>()
            .map(|sample| sample.expect("sample"))
            .collect();
        assert_eq!(exported, [0.0, 0.0, 1.0, -1.0]);
    }

    #[test]
    fn mixdown_job_reports_progress_and_completes() {
        let document = Document::sample();
        let mut runner = JobRunner::<usize>::new();
        let id = runner
            .spawn("mixdown", move |context| {
                let samples = render_with_progress(
                    &document,
                    |fraction| {
                        context.set_progress_message(fraction, "Rendering");
                        std::thread::sleep(Duration::from_millis(2));
                    },
                    || context.is_cancelled(),
                )
                .map_err(|error| error.to_string())?;
                Ok(samples.len())
            })
            .expect("spawn");

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut result = None;
        let mut saw_progress = false;
        while result.is_none() {
            if let Some(job) = runner.jobs().iter().find(|job| job.id == id)
                && job.fraction() > 0.0
                && job.message() == "Rendering"
            {
                saw_progress = true;
            }
            for (job_id, outcome) in runner.poll() {
                if job_id == id {
                    result = Some(outcome);
                }
            }
            assert!(Instant::now() < deadline, "mixdown job timed out");
            std::thread::sleep(Duration::from_millis(3));
        }

        assert!(saw_progress, "the job must expose progress to the UI");
        let frames = result.expect("job result").expect("render");
        assert_eq!(frames, frame_count(&Document::sample()));
    }
}
