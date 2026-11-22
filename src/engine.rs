//! Offline synthesis engine.
//!
//! Rendering is deterministic and UI-free: it converts beats to seconds at the
//! document tempo, sums every unmuted track into a mono buffer at 44.1 kHz and
//! clamps the result to `[-1, 1]`. No audio device or real-time thread is used.

use std::fmt;
use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};

use crate::document::{Document, Instrument, Note, Track};

/// Frames per second of every rendered buffer and exported WAV file.
pub const SAMPLE_RATE: u32 = 44_100;
/// Tail kept after the last note so release envelopes are not truncated.
pub const RELEASE_SECONDS: f64 = 0.18;
/// Frames rendered between two progress/cancellation checks.
const CHUNK_FRAMES: usize = 4096;
/// Upper bound of roughly ten minutes, guarding against absurd durations.
const MAX_FRAMES: usize = 26_460_000;

/// Failure modes of the offline renderer and WAV writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// The caller asked the render to stop.
    Cancelled,
    /// The WAV file could not be written.
    Wav(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("render cancelled"),
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
        if !elapsed.is_finite() || elapsed < 0.0 {
            return 0.0;
        }
        if elapsed < self.attack {
            return if self.attack > 0.0 {
                elapsed / self.attack
            } else {
                1.0
            };
        }
        let after_attack = elapsed - self.attack;
        if after_attack < self.decay {
            return if self.decay > 0.0 {
                1.0 - (1.0 - self.sustain) * (after_attack / self.decay)
            } else {
                self.sustain
            };
        }
        if elapsed < held_seconds {
            return self.sustain;
        }
        let into_release = elapsed - held_seconds;
        if into_release < self.release && self.release > 0.0 {
            return self.sustain * (1.0 - into_release / self.release);
        }
        0.0
    }
}

/// Number of mono frames needed for the arrangement plus its release tail.
#[must_use]
pub fn frame_count(document: &Document) -> usize {
    let beats = document.duration_beats();
    if !beats.is_finite() || beats <= 0.0 {
        return 0;
    }
    let seconds = document.duration_seconds() + RELEASE_SECONDS;
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    (seconds * f64::from(SAMPLE_RATE))
        .ceil()
        .min(MAX_FRAMES as f64) as usize
}

fn track_gain(document: &Document) -> f32 {
    let audible = document
        .tracks
        .iter()
        .filter(|track| !track.mute && Track::clamp_volume(track.volume) > 0.0)
        .count();
    0.9 / (audible.max(1) as f32)
}

/// Mix the notes that overlap the half-open frame range `[offset, end)`.
fn mix_range(output: &mut [f32], offset: usize, document: &Document, gain: f32) {
    let sample_rate = f64::from(SAMPLE_RATE);
    let bpm = document.effective_bpm();
    let seconds_per_beat = 60.0 / bpm;
    let envelope = Adsr::default();

    for track in &document.tracks {
        if track.mute {
            continue;
        }
        let volume = f64::from(Track::clamp_volume(track.volume));
        if volume <= 0.0 {
            continue;
        }
        for note in &track.notes {
            if !note.is_valid() {
                continue;
            }
            mix_note(
                output,
                offset,
                note,
                track.instrument,
                volume,
                f64::from(gain),
                seconds_per_beat,
                sample_rate,
                &envelope,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn mix_note(
    output: &mut [f32],
    offset: usize,
    note: &Note,
    instrument: Instrument,
    volume: f64,
    gain: f64,
    seconds_per_beat: f64,
    sample_rate: f64,
    envelope: &Adsr,
) {
    let frequency = note_frequency(note.pitch);
    let velocity = f64::from(Note::clamp_velocity(note.velocity));
    let onset_seconds = note.start_beat * seconds_per_beat;
    let held_seconds = note.length_beats * seconds_per_beat;
    let onset_frame = (onset_seconds * sample_rate).round() as usize;
    let release_end = onset_seconds + held_seconds + envelope.release;
    let end_frame = (release_end * sample_rate).ceil() as usize;

    let first = onset_frame.max(offset);
    let last = end_frame.min(offset.saturating_add(output.len()));
    if first >= last {
        return;
    }

    for frame in first..last {
        let elapsed = (frame - onset_frame) as f64 / sample_rate;
        let phase = frequency * elapsed;
        let amplitude = oscillator(instrument, phase) * envelope.level(elapsed, held_seconds);
        let value = amplitude * velocity * volume * gain;
        output[frame - offset] += value as f32;
    }
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
/// Returns [`RenderError::Cancelled`] when `cancelled` becomes true. The
/// progress callback receives fractions in `0.0..=1.0`.
pub fn render_with_progress<F, C>(
    document: &Document,
    mut progress: F,
    mut cancelled: C,
) -> Result<Vec<f32>, RenderError>
where
    F: FnMut(f32),
    C: FnMut() -> bool,
{
    let total = frame_count(document);
    if total == 0 {
        return Ok(Vec::new());
    }
    let mut output = vec![0.0_f32; total];
    let gain = track_gain(document);
    let mut offset = 0;
    while offset < total {
        if cancelled() {
            return Err(RenderError::Cancelled);
        }
        let end = (offset + CHUNK_FRAMES).min(total);
        mix_range(&mut output[offset..end], offset, document, gain);
        progress(end as f32 / total as f32);
        offset = end;
    }
    clamp_buffer(&mut output);
    Ok(output)
}

/// Export mono float32 samples as a 44.1 kHz WAV file.
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

    fn render(document: &Document) -> Vec<f32> {
        render_with_progress(document, |_| {}, || false).expect("render")
    }

    fn temp_wav_path() -> std::path::PathBuf {
        let count = WAV_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "musiccraft-mixdown-{}-{count}.wav",
            std::process::id()
        ))
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
        assert!((oscillator(Instrument::Sine, 0.25) - 1.0).abs() < 1e-9);
        assert!((oscillator(Instrument::Sine, 0.75) + 1.0).abs() < 1e-9);
        assert_eq!(oscillator(Instrument::Square, 0.25), 1.0);
        assert_eq!(oscillator(Instrument::Square, 0.75), -1.0);
        assert!((oscillator(Instrument::Saw, 0.5)).abs() < 1e-9);
        for instrument in [Instrument::Sine, Instrument::Square, Instrument::Saw] {
            for step in 0..100 {
                let value = oscillator(instrument, step as f64 / 100.0);
                assert!((-1.0..=1.0).contains(&value));
            }
        }
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
