//! Optional real-time playback through CPAL.
//!
//! If the host exposes an output device, a pre-rendered mono buffer is streamed
//! to it without blocking the UI thread. If no device exists (headless CI,
//! containers, muted desktops) the player reports itself unavailable and the
//! application falls back to the visual-only offline playhead. No unsafe code.

use std::fmt;
use std::sync::{Arc, Mutex};

use cpal::SampleFormat;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// State shared between the UI thread and the audio callback.
#[derive(Debug, Default)]
struct PlaybackState {
    samples: Vec<f32>,
    cursor: usize,
    playing: bool,
    loops: bool,
}

/// Reason real-time playback could not be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// No output device was found.
    NoDevice,
    /// The device configuration could not be read or built.
    Device(String),
    /// The sample format is not supported by this build.
    UnsupportedFormat(String),
    /// The stream could not be started.
    Stream(String),
    /// Playback was requested without any rendered samples.
    EmptyBuffer,
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice => formatter.write_str("no audio output device"),
            Self::Device(error) => write!(formatter, "audio device error: {error}"),
            Self::UnsupportedFormat(format) => {
                write!(formatter, "unsupported audio sample format: {format}")
            }
            Self::Stream(error) => write!(formatter, "audio stream error: {error}"),
            Self::EmptyBuffer => formatter.write_str("no rendered audio to play"),
        }
    }
}

impl std::error::Error for AudioError {}

/// A small real-time player for one pre-rendered mono buffer.
pub struct Player {
    stream: Option<cpal::Stream>,
    state: Arc<Mutex<PlaybackState>>,
    device_name: String,
    unavailable: Option<String>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Player")
            .field("device_name", &self.device_name)
            .field("available", &self.stream.is_some())
            .field("unavailable", &self.unavailable)
            .finish()
    }
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Player {
    /// Probe the default output device and build a paused stream when possible.
    #[must_use]
    pub fn new() -> Self {
        let state = Arc::new(Mutex::new(PlaybackState::default()));
        match Self::build_stream(&state) {
            Ok((stream, device_name)) => Self {
                stream: Some(stream),
                state,
                device_name,
                unavailable: None,
            },
            Err(error) => Self {
                stream: None,
                state,
                device_name: String::new(),
                unavailable: Some(error.to_string()),
            },
        }
    }

    fn build_stream(
        state: &Arc<Mutex<PlaybackState>>,
    ) -> Result<(cpal::Stream, String), AudioError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
        let name = device
            .description()
            .map(|description| description.name().to_owned())
            .unwrap_or_else(|_| "default output".to_owned());
        let supported = device
            .default_output_config()
            .map_err(|error| AudioError::Device(error.to_string()))?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.config();
        let channels = config.channels.max(1) as usize;

        let error_callback = |error| {
            // Never panic on the audio thread; surface failures in stderr only.
            eprintln!("MusicCraft audio stream error: {error}");
        };
        let callback_state = Arc::clone(state);
        let stream = match format {
            SampleFormat::F32 => device.build_output_stream(
                config,
                move |data: &mut [f32], _| write_samples(&callback_state, data, channels),
                error_callback,
                None,
            ),
            SampleFormat::F64 => device.build_output_stream(
                config,
                move |data: &mut [f64], _| write_samples(&callback_state, data, channels),
                error_callback,
                None,
            ),
            SampleFormat::I8 => device.build_output_stream(
                config,
                move |data: &mut [i8], _| write_samples(&callback_state, data, channels),
                error_callback,
                None,
            ),
            SampleFormat::U8 => device.build_output_stream(
                config,
                move |data: &mut [u8], _| write_samples(&callback_state, data, channels),
                error_callback,
                None,
            ),
            SampleFormat::I32 => device.build_output_stream(
                config,
                move |data: &mut [i32], _| write_samples(&callback_state, data, channels),
                error_callback,
                None,
            ),
            SampleFormat::U32 => device.build_output_stream(
                config,
                move |data: &mut [u32], _| write_samples(&callback_state, data, channels),
                error_callback,
                None,
            ),
            SampleFormat::I64 => device.build_output_stream(
                config,
                move |data: &mut [i64], _| write_samples(&callback_state, data, channels),
                error_callback,
                None,
            ),
            SampleFormat::U64 => device.build_output_stream(
                config,
                move |data: &mut [u64], _| write_samples(&callback_state, data, channels),
                error_callback,
                None,
            ),
            SampleFormat::I16 => device.build_output_stream(
                config,
                move |data: &mut [i16], _| write_samples(&callback_state, data, channels),
                error_callback,
                None,
            ),
            SampleFormat::U16 => device.build_output_stream(
                config,
                move |data: &mut [u16], _| write_samples(&callback_state, data, channels),
                error_callback,
                None,
            ),
            other => return Err(AudioError::UnsupportedFormat(other.to_string())),
        }
        .map_err(|error| AudioError::Stream(error.to_string()))?;
        Ok((stream, name))
    }

    /// Whether a real output device is available.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.stream.is_some()
    }

    /// Name of the selected output device, when available.
    #[must_use]
    pub fn device_name(&self) -> Option<&str> {
        if self.stream.is_some() {
            Some(&self.device_name)
        } else {
            None
        }
    }

    /// Why real-time playback is unavailable, when known.
    #[must_use]
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// Replace the playback buffer and start streaming from its first frame.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::NoDevice`] when no stream exists,
    /// [`AudioError::EmptyBuffer`] for empty input, or [`AudioError::Stream`]
    /// when the stream cannot start.
    pub fn play(&self, samples: Vec<f32>, loops: bool) -> Result<(), AudioError> {
        let Some(stream) = self.stream.as_ref() else {
            return Err(AudioError::NoDevice);
        };
        if samples.is_empty() {
            return Err(AudioError::EmptyBuffer);
        }
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.samples = samples;
            state.cursor = 0;
            state.loops = loops;
            state.playing = true;
        }
        stream.play().map_err(|error| {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.playing = false;
            state.cursor = 0;
            state.samples.clear();
            AudioError::Stream(error.to_string())
        })
    }

    /// Pause without discarding the current position.
    pub fn pause(&self) {
        if let Some(stream) = self.stream.as_ref() {
            let _ = stream.pause();
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.playing = false;
    }

    /// Resume from the current position when a buffer is loaded.
    pub fn resume(&self) -> bool {
        let Some(stream) = self.stream.as_ref() else {
            return false;
        };
        {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.samples.is_empty() {
                return false;
            }
        }
        if stream.play().is_err() {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.playing = false;
            return false;
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.playing = true;
        true
    }

    /// Stop playback, rewind and drop the buffer so the next play re-renders.
    pub fn stop(&self) {
        if let Some(stream) = self.stream.as_ref() {
            let _ = stream.pause();
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.playing = false;
        state.cursor = 0;
        state.samples.clear();
    }

    /// Current playback position in mono frames.
    #[must_use]
    pub fn position_frames(&self) -> usize {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.cursor
    }

    /// Whether samples are currently being streamed.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.playing
    }
}

fn write_samples<T>(state: &Arc<Mutex<PlaybackState>>, output: &mut [T], channels: usize)
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let Ok(mut state) = state.try_lock() else {
        fill_silence(output);
        return;
    };
    if !state.playing || state.samples.is_empty() {
        fill_silence(output);
        return;
    }

    let channels = channels.max(1);
    for frame in output.chunks_exact_mut(channels) {
        if !state.playing {
            fill_silence(frame);
            continue;
        }
        let sample = state
            .samples
            .get(state.cursor)
            .copied()
            .filter(|sample| sample.is_finite())
            .unwrap_or(0.0);
        state.cursor += 1;
        if state.cursor >= state.samples.len() {
            if state.loops {
                state.cursor = 0;
            } else {
                state.playing = false;
                state.cursor = state.samples.len();
            }
        }
        for channel in frame {
            *channel = T::from_sample(sample);
        }
    }
}

fn fill_silence<T: cpal::Sample>(output: &mut [T]) {
    for sample in output {
        *sample = T::EQUILIBRIUM;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::engine;

    #[test]
    fn a_player_can_always_be_constructed_without_panicking() {
        let player = Player::new();
        assert!(!player.is_playing());
        if !player.is_available() {
            assert!(player.unavailable_reason().is_some());
        }
    }

    #[test]
    fn stop_and_position_work_without_a_device() {
        let player = Player::new();
        player.stop();
        assert_eq!(player.position_frames(), 0);
        player.pause();
        assert!(!player.resume()); // no buffer loaded
    }

    #[test]
    fn write_samples_fills_and_advances_one_frame_per_sample() {
        let state = Arc::new(Mutex::new(PlaybackState {
            samples: vec![0.5, -0.5],
            cursor: 0,
            playing: true,
            loops: false,
        }));
        let mut output = [0.0_f32; 6];
        write_samples(&state, &mut output, 2);
        assert_eq!(output, [0.5, 0.5, -0.5, -0.5, 0.0, 0.0]);
        assert_eq!(state.lock().map(|s| s.cursor).unwrap_or(0), 2);
        assert!(!state.lock().map(|s| s.playing).unwrap_or(true));
    }

    #[test]
    fn write_samples_loops_and_sanitizes_non_finite_samples() {
        let state = Arc::new(Mutex::new(PlaybackState {
            samples: vec![f32::NAN, 0.5],
            cursor: 0,
            playing: true,
            loops: true,
        }));
        let mut output = [1.0_f32; 6];
        write_samples(&state, &mut output, 2);
        assert_eq!(output, [0.0, 0.0, 0.5, 0.5, 0.0, 0.0]);
        assert_eq!(state.lock().map(|s| s.cursor).unwrap_or(0), 1);
        assert!(state.lock().map(|s| s.playing).unwrap_or(false));
    }

    #[test]
    fn render_output_is_usable_as_a_playback_buffer() {
        let document = Document::sample();
        let buffer = engine::render(&document);
        assert_eq!(buffer.len(), engine::frame_count(&document));
        assert!(buffer.iter().all(|sample| sample.is_finite()));
    }
}
