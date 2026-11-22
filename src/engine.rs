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
    let any_solo = document.has_solo();
    let audible = document
        .tracks
        .iter()
        .filter(|track| track.is_audible_with(any_solo) && Track::clamp_volume(track.volume) > 0.0)
        .count();
    Track::clamp_volume(document.master_volume) / (audible.max(1) as f32)
}

/// Apply a simple insert effect to a mono track buffer in place.
pub fn apply_effect(samples: &mut [f32], effect: &Effect, seconds_per_beat: f64) {
    match *effect {
        Effect::None => {}
        Effect::Delay {
            time_beats,
            feedback,
            mix,
        } => {
            if samples.is_empty() || !time_beats.is_finite() || time_beats <= 0.0 {
                return;
            }
            let delay_frames = ((time_beats * seconds_per_beat * f64::from(SAMPLE_RATE)).round()
                as usize)
                .clamp(1, samples.len());
            let feedback = feedback.clamp(0.0, 0.9);
            let mix = mix.clamp(0.0, 1.0);
            let mut line = vec![0.0_f32; delay_frames];
            for (index, sample) in samples.iter_mut().enumerate() {
                let slot = index % delay_frames;
                let delayed = line[slot];
                let dry = *sample;
                line[slot] = dry + feedback * delayed;
                *sample = dry + mix * delayed;
            }
        }
        Effect::Reverb { decay, mix } => {
            if samples.is_empty() {
                return;
            }
            let decay = decay.clamp(0.0, 0.95);
            let mix = mix.clamp(0.0, 1.0);
            let bases = [0.0297_f64, 0.0371, 0.0411, 0.0437];
            let mut lines: Vec<Vec<f32>> = bases
                .iter()
                .map(|base| {
                    vec![0.0_f32; ((base * f64::from(SAMPLE_RATE)).round() as usize).max(1)]
                })
                .collect();
            let mut wet = vec![0.0_f32; samples.len()];
            for index in 0..samples.len() {
                let mut sum = 0.0_f32;
                for line in &mut lines {
                    let slot = index % line.len();
                    let delayed = line[slot];
                    line[slot] = samples[index] + decay * delayed;
                    sum += delayed;
                }
                wet[index] = sum / lines.len() as f32;
            }
            for (sample, echo) in samples.iter_mut().zip(wet) {
                *sample += mix * echo;
            }
        }
    }
}

/// Render one track into a full-length buffer (velocity included, no volume).
fn render_track(buffer: &mut [f32], track: &Track, seconds_per_beat: f64) {
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
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn mix_note(
    output: &mut [f32],
    offset: usize,
    note: &Note,
    instrument: Instrument,
    velocity: f64,
    seconds_per_beat: f64,
    sample_rate: f64,
    envelope: &Adsr,
) {
    let frequency = note_frequency(note.pitch);
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
        output[frame - offset] += (amplitude * velocity) as f32;
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
    let seconds_per_beat = 60.0 / document.effective_bpm();
    let any_solo = document.has_solo();
    let gain = track_gain(document);

    // Insert effects need the whole track before it joins the mix.
    let mut tracks: Vec<(Vec<f32>, f32)> = Vec::new();
    for track in &document.tracks {
        if cancelled() {
            return Err(RenderError::Cancelled);
        }
        if !track.is_audible_with(any_solo) {
            continue;
        }
        let volume = Track::clamp_volume(track.volume);
        if volume <= 0.0 {
            continue;
        }
        let mut buffer = vec![0.0_f32; total];
        render_track(&mut buffer, track, seconds_per_beat);
        apply_effect(&mut buffer, &track.effect, seconds_per_beat);
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
        progress(end as f32 / total as f32);
        offset = end;
    }
    clamp_buffer(&mut output);
    Ok(output)
}

/// Render the whole arrangement without progress reporting.
#[must_use]
pub fn render(document: &Document) -> Vec<f32> {
    render_with_progress(document, |_| {}, || false).unwrap_or_default()
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
        for instrument in Instrument::all() {
            for step in 0..200 {
                let value = oscillator(instrument, step as f64 / 200.0);
                assert!((-1.0..=1.0).contains(&value), "{instrument:?}");
            }
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
    fn solo_isolates_the_soloed_track() {
        let mut document = Document::sample();
        let all = render(&document);
        document.tracks[1].solo = true;
        let soloed = render(&document);
        assert!(soloed.iter().any(|sample| *sample != 0.0));
        assert_ne!(all, soloed);
        // Only one audible track: its gain is now the full master level.
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
