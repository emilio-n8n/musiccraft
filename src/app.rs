//! MusicCraft user interface and project lifecycle.
//!
//! The shell, dialogs, history and background jobs come from CraftKit. This
//! module owns the arrangement panels, the animated piano-roll playhead and the
//! WAV mixdown workflow.

use std::path::PathBuf;
use std::time::Instant;

use craftkit_io::DocumentHost;
use craftkit_jobs::JobRunner;
use craftkit_ui::cli::CliOptions;
use craftkit_ui::eframe::{self, egui};
use craftkit_ui::egui::{
    Align, Align2, Color32, ComboBox, CornerRadius, DragValue, FontId, Layout, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, TextEdit, Vec2,
};
use craftkit_ui::{AppIdentity, AppShell, Tone, dialogs, theme, widgets};

use crate::audio::Player;
use crate::document::{Document, Effect, Instrument, Note, Track};
use crate::edit;
use crate::engine;
use crate::midi;

const FORMAT: &str = "musiccraft";
const SCHEMA_VERSION: u32 = 1;
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Lowest and highest MIDI pitches drawn on the piano roll.
const PITCH_LOW: u8 = 48;
const PITCH_HIGH: u8 = 84;
const ROW_HEIGHT: f32 = 12.0;
const BEAT_WIDTH: f32 = 34.0;
const LABEL_WIDTH: f32 = 34.0;
const RULER_HEIGHT: f32 = 18.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    New,
    Open,
    Save,
    SaveAs,
    ImportMidi,
    ExportMidi,
    Undo,
    Redo,
    Mixdown,
    Duplicate,
    Quantize,
    PlayPause,
    Stop,
    About,
    Settings,
}

#[derive(Debug, Clone)]
enum PendingAction {
    New,
    Open(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingChoice {
    Save,
    Discard,
    Cancel,
}

#[derive(Debug, Clone)]
enum TrackEdit {
    ToggleMute,
    ToggleSolo,
    SetInstrument(Instrument),
    SetVolume(f32),
    SetEffect(Effect),
    Rename(String),
    Remove,
}

#[derive(Debug, Clone, Copy)]
enum NoteEdit {
    SetPitch(u8),
    SetStart(f64),
    SetLength(f64),
    SetVelocity(f32),
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    Move,
    Resize,
}

type TrackRow = (usize, String, Instrument, f32, bool, bool, Effect, f32);

#[derive(Debug, Clone, Copy)]
struct NoteDrag {
    mode: DragMode,
    note_id: u64,
    start_x: f32,
    start_y: f32,
    original: Note,
}

/// The MusicCraft application state.
pub struct MusicCraftApp {
    shell: AppShell,
    host: DocumentHost<Document>,
    selected_track: usize,
    selected_note: Option<u64>,
    jobs: JobRunner<PathBuf>,
    pending: Option<PendingAction>,
    player: Player,
    realtime: bool,
    playing: bool,
    playhead_beats: f64,
    last_tick: Option<Instant>,
    drag: Option<NoteDrag>,
    quantize_grid: f64,
}

impl MusicCraftApp {
    /// Build the application, opening the startup project when requested.
    pub fn new(context: &eframe::CreationContext<'_>, options: CliOptions) -> Self {
        let identity = AppIdentity {
            id: "musiccraft",
            name: "MusicCraft",
            version: APP_VERSION,
            tagline: "MIDI arrangement, built-in synth and offline WAV mixdown",
            repository: Some("https://github.com/emilio-n8n/musiccraft"),
        };
        let shell = AppShell::new(identity);
        let document = if options.sample {
            Document::sample()
        } else {
            Document::default()
        };
        let mut app = Self {
            shell,
            host: DocumentHost::new(document, FORMAT, SCHEMA_VERSION, APP_VERSION),
            selected_track: 0,
            selected_note: None,
            jobs: JobRunner::new(),
            pending: None,
            player: Player::new(),
            realtime: false,
            playing: false,
            playhead_beats: 0.0,
            last_tick: None,
            drag: None,
            quantize_grid: edit::DEFAULT_GRID,
        };
        app.shell.install_theme(&context.egui_ctx);
        if app.player.is_available() {
            app.shell.set_status(format!(
                "Real-time audio: {}",
                app.player.device_name().unwrap_or("default")
            ));
        } else {
            app.shell.set_status(format!(
                "Offline preview — {}",
                app.player.unavailable_reason().unwrap_or("no audio device")
            ));
        }
        if let Some(path) = options.open {
            app.load_path(path);
        }
        app
    }

    fn load_path(&mut self, path: PathBuf) {
        match DocumentHost::from_file(path.clone(), FORMAT, SCHEMA_VERSION, APP_VERSION) {
            Ok(host) => {
                self.host = host;
                self.selected_track = 0;
                self.selected_note = None;
                self.stop();
                self.shell.set_status(format!("Opened {}", path.display()));
            }
            Err(error) => {
                self.shell
                    .notify(Tone::Error, "Could not open project", error.to_string())
            }
        }
    }

    fn new_document(&mut self) {
        self.host.reset(Document::default());
        self.selected_track = 0;
        self.selected_note = None;
        self.stop();
        self.shell.set_status("New document");
    }

    fn save(&mut self) -> bool {
        if self.host.path().is_none() {
            return self.save_as();
        }
        match self.host.save() {
            Ok(path) => {
                self.shell.set_status(format!("Saved {}", path.display()));
                true
            }
            Err(error) => {
                self.shell
                    .notify(Tone::Error, "Could not save project", error.to_string());
                false
            }
        }
    }

    fn save_as(&mut self) -> bool {
        let default_name = format!("{}.json", slugify(&self.host.document().title));
        let Some(path) = dialogs::save_project("Save project", &default_name, "json") else {
            return false;
        };
        match self.host.save_as(path) {
            Ok(path) => {
                self.shell.set_status(format!("Saved {}", path.display()));
                true
            }
            Err(error) => {
                self.shell
                    .notify(Tone::Error, "Could not save project", error.to_string());
                false
            }
        }
    }

    fn request_new(&mut self) {
        if self.host.is_dirty() {
            self.pending = Some(PendingAction::New);
        } else {
            self.new_document();
        }
    }

    fn request_open(&mut self) {
        let Some(path) = dialogs::open_project("Open project", &["json"]) else {
            return;
        };
        if self.host.is_dirty() {
            self.pending = Some(PendingAction::Open(path));
        } else {
            self.load_path(path);
        }
    }

    fn perform(&mut self, action: PendingAction) {
        match action {
            PendingAction::New => self.new_document(),
            PendingAction::Open(path) => self.load_path(path),
        }
    }

    fn resolve_pending(&mut self, choice: PendingChoice) {
        let Some(action) = self.pending.take() else {
            return;
        };
        match choice {
            PendingChoice::Cancel => {}
            PendingChoice::Discard => self.perform(action),
            PendingChoice::Save => {
                if self.save() {
                    self.perform(action);
                } else {
                    self.pending = Some(action);
                }
            }
        }
    }

    fn clamp_selection(&mut self) {
        let count = self.host.document().tracks.len();
        if count == 0 {
            self.selected_track = 0;
            self.selected_note = None;
        } else {
            self.selected_track = self.selected_track.min(count - 1);
            let has_note = self
                .host
                .document()
                .tracks
                .get(self.selected_track)
                .is_some_and(|track| {
                    track
                        .notes
                        .iter()
                        .any(|note| Some(note.id) == self.selected_note)
                });
            if !has_note {
                self.selected_note = None;
            }
        }
        if self.playhead_beats > self.host.document().duration_beats() {
            self.playhead_beats = 0.0;
            self.last_tick = None;
        }
    }

    fn toggle_play(&mut self) {
        if self.host.document().duration_beats() <= 0.0 {
            self.shell.set_status("Add a note before starting playback");
            return;
        }
        if self.playing {
            self.playing = false;
            self.player.pause();
            self.shell.set_status("Playback paused");
            return;
        }
        if self.realtime && self.player.resume() {
            self.playing = true;
            self.last_tick = None;
            self.shell.set_status("Real-time playback resumed");
            return;
        }
        self.playing = true;
        self.last_tick = None;
        if self.player.is_available() {
            let samples = engine::render(self.host.document());
            match self.player.play(samples, true) {
                Ok(()) => {
                    self.realtime = true;
                    self.shell.set_status(format!(
                        "Real-time playback — {}",
                        self.player.device_name().unwrap_or("default output")
                    ));
                }
                Err(error) => {
                    self.realtime = false;
                    self.shell.set_status(format!("Offline preview — {error}"));
                }
            }
        } else {
            self.realtime = false;
            self.shell
                .set_status("Offline preview (no audio device) — visual playhead only, no sound");
        }
    }

    fn stop(&mut self) {
        self.playing = false;
        self.realtime = false;
        self.player.stop();
        self.playhead_beats = 0.0;
        self.last_tick = None;
    }

    fn update_playhead(&mut self, ui: &egui::Ui) {
        if !self.playing {
            self.last_tick = None;
            return;
        }
        let document = self.host.document();
        let duration = document.duration_beats();
        if duration <= 0.0 {
            self.stop();
            return;
        }
        if self.realtime && self.player.is_playing() {
            let seconds = self.player.position_frames() as f64 / f64::from(engine::SAMPLE_RATE);
            self.playhead_beats = seconds * document.effective_bpm() / 60.0;
            if self.playhead_beats > duration {
                self.playhead_beats = 0.0;
            }
            ui.ctx().request_repaint();
            return;
        }
        let now = Instant::now();
        let delta = self
            .last_tick
            .map_or(0.0, |previous| now.duration_since(previous).as_secs_f64());
        self.last_tick = Some(now);
        self.playhead_beats += delta * document.effective_bpm() / 60.0;
        if self.playhead_beats > duration {
            self.playhead_beats = 0.0;
        }
        ui.ctx().request_repaint();
    }

    fn add_track(&mut self) {
        self.host.apply(|document| {
            let id = document.allocate_id();
            let number = document.tracks.len() + 1;
            document.tracks.push(Track {
                id,
                name: format!("Track {number}"),
                instrument: Instrument::Sine,
                volume: 0.7,
                mute: false,
                solo: false,
                effect: crate::document::Effect::None,
                notes: Vec::new(),
            });
        });
        self.selected_track = self.host.document().tracks.len().saturating_sub(1);
        self.selected_note = None;
    }

    fn add_note(&mut self, track_index: usize, pitch: u8, start_beat: f64) {
        self.host.apply(|document| {
            if track_index >= document.tracks.len() {
                return;
            }
            let id = document.allocate_id();
            let note = Note {
                id,
                pitch: Note::clamp_pitch(pitch),
                start_beat: start_beat.max(0.0),
                length_beats: 1.0,
                velocity: 0.8,
            };
            if note.is_valid() {
                document.tracks[track_index].notes.push(note);
            }
        });
        self.selected_track = track_index;
        self.selected_note = self
            .host
            .document()
            .tracks
            .get(track_index)
            .and_then(|track| track.notes.last())
            .map(|note| note.id);
        self.clamp_selection();
    }

    fn apply_track_edit(&mut self, index: usize, edit: TrackEdit) {
        match edit {
            TrackEdit::ToggleMute => {
                self.host.apply(|document| {
                    if let Some(track) = document.tracks.get_mut(index) {
                        track.mute = !track.mute;
                    }
                });
            }
            TrackEdit::ToggleSolo => {
                self.host.apply(|document| {
                    if let Some(track) = document.tracks.get_mut(index) {
                        track.solo = !track.solo;
                    }
                });
            }
            TrackEdit::SetInstrument(instrument) => {
                self.host.apply(|document| {
                    if let Some(track) = document.tracks.get_mut(index) {
                        track.instrument = instrument;
                    }
                });
            }
            TrackEdit::SetEffect(effect) => {
                self.host.apply(|document| {
                    if let Some(track) = document.tracks.get_mut(index) {
                        track.effect = effect;
                    }
                });
            }
            TrackEdit::SetVolume(volume) => {
                self.host.apply(|document| {
                    if let Some(track) = document.tracks.get_mut(index) {
                        track.volume = Track::clamp_volume(volume);
                    }
                });
            }
            TrackEdit::Rename(name) => {
                self.host.apply(|document| {
                    if let Some(track) = document.tracks.get_mut(index) {
                        track.name = name;
                    }
                });
            }
            TrackEdit::Remove => {
                self.host.apply(|document| {
                    if index < document.tracks.len() {
                        document.tracks.remove(index);
                    }
                });
            }
        }
        self.clamp_selection();
    }

    fn apply_note_edit(&mut self, track_index: usize, note_id: u64, edit: NoteEdit) {
        self.host.apply(|document| {
            let Some(track) = document.tracks.get_mut(track_index) else {
                return;
            };
            let Some(note) = track.notes.iter_mut().find(|note| note.id == note_id) else {
                return;
            };
            match edit {
                NoteEdit::SetPitch(pitch) => note.pitch = Note::clamp_pitch(pitch),
                NoteEdit::SetStart(start) if start.is_finite() && start >= 0.0 => {
                    note.start_beat = start;
                }
                NoteEdit::SetLength(length) if length.is_finite() && length > 0.0 => {
                    note.length_beats = length;
                }
                NoteEdit::SetVelocity(velocity) => {
                    note.velocity = Note::clamp_velocity(velocity);
                }
                NoteEdit::Remove => {}
                NoteEdit::SetStart(_) | NoteEdit::SetLength(_) => {}
            }
        });
        if matches!(edit, NoteEdit::Remove) {
            self.selected_note = None;
        }
        self.clamp_selection();
    }

    fn remove_note(&mut self, track_index: usize, note_id: u64) {
        self.host.apply(|document| {
            if let Some(track) = document.tracks.get_mut(track_index) {
                track.notes.retain(|note| note.id != note_id);
            }
        });
        self.selected_note = None;
        self.clamp_selection();
    }

    fn import_midi(&mut self) {
        let Some(path) = dialogs::open_project("Import MIDI", &["mid", "midi"]) else {
            return;
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.shell
                    .notify(Tone::Error, "Could not read MIDI", error.to_string());
                return;
            }
        };
        match midi::import_bytes(&bytes) {
            Ok(document) => {
                self.stop();
                self.host.reset(document);
                self.selected_track = 0;
                self.selected_note = None;
                self.shell
                    .set_status(format!("Imported MIDI from {}", path.display()));
            }
            Err(error) => {
                self.shell
                    .notify(Tone::Error, "Could not import MIDI", error.to_string())
            }
        }
    }

    fn export_midi(&mut self) {
        let document = self.host.document();
        let bytes = match midi::export_bytes(document) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.shell
                    .notify(Tone::Error, "Could not export MIDI", error.to_string());
                return;
            }
        };
        let default_name = format!("{}.mid", slugify(&document.title));
        let Some(path) = dialogs::save_project("Export MIDI", &default_name, "mid") else {
            return;
        };
        match std::fs::write(&path, bytes) {
            Ok(()) => self
                .shell
                .set_status(format!("Exported MIDI to {}", path.display())),
            Err(error) => self
                .shell
                .notify(Tone::Error, "Could not write MIDI", error.to_string()),
        }
    }

    fn duplicate_selected_note(&mut self) {
        let (track, note) = (self.selected_track, self.selected_note);
        let Some(note_id) = note else {
            self.shell.set_status("Select a note to duplicate");
            return;
        };
        let mut new_id = None;
        self.host.apply(|document| {
            new_id = edit::duplicate_note(document, track, note_id);
        });
        if let Some(id) = new_id {
            self.selected_note = Some(id);
            self.shell.set_status("Note duplicated");
        } else {
            self.shell.set_status("Could not duplicate note");
        }
    }

    fn quantize_selected(&mut self) {
        let grid = edit::clamp_grid(self.quantize_grid);
        let track = self.selected_track;
        let note_id = self.selected_note;
        let mut changed = 0;
        self.host.apply(|document| {
            if let Some(id) = note_id {
                if let Some(track) = document.tracks.get_mut(track)
                    && let Some(note) = track.notes.iter_mut().find(|note| note.id == id)
                {
                    *note = edit::quantize_note(note, grid);
                    changed = 1;
                }
            } else {
                changed = edit::quantize_document(document, grid);
            }
        });
        self.shell
            .set_status(format!("Quantized {changed} note(s) to {grid} beat(s)"));
    }

    fn start_mixdown(&mut self) {
        let document = self.host.document().clone();
        let default_name = format!("{}.wav", slugify(&document.title));
        let Some(path) = dialogs::save_project("Mixdown WAV", &default_name, "wav") else {
            return;
        };
        let result = self.jobs.spawn("Mixdown WAV", move |context| {
            context.set_progress_message(0.0, "Preparing");
            let samples = engine::render_with_progress(
                &document,
                |fraction| {
                    context.set_progress_message(fraction, "Rendering audio");
                },
                || context.is_cancelled(),
            )
            .map_err(|error| error.to_string())?;
            context.set_progress_message(1.0, "Writing WAV");
            engine::write_wav(&path, &samples).map_err(|error| error.to_string())?;
            Ok(path)
        });
        match result {
            Ok(_) => self.shell.set_status("Mixdown started"),
            Err(error) => {
                self.shell
                    .notify(Tone::Error, "Could not start mixdown", error.to_string())
            }
        }
    }

    fn poll_jobs(&mut self) {
        for (_, result) in self.jobs.poll() {
            match result {
                Ok(path) => self.shell.notify(
                    Tone::Success,
                    "Mixdown ready",
                    format!("Wrote {}", path.display()),
                ),
                Err(error) if error.contains("cancel") => {
                    self.shell.set_status("Mixdown cancelled");
                }
                Err(error) => self.shell.notify(Tone::Error, "Mixdown failed", error),
            }
        }
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::New => self.request_new(),
            Action::Open => self.request_open(),
            Action::Save => {
                self.save();
            }
            Action::SaveAs => {
                self.save_as();
            }
            Action::ImportMidi => self.import_midi(),
            Action::ExportMidi => self.export_midi(),
            Action::Undo => {
                if self.host.undo() {
                    self.clamp_selection();
                    self.shell.set_status("Undo");
                }
            }
            Action::Redo => {
                if self.host.redo() {
                    self.clamp_selection();
                    self.shell.set_status("Redo");
                }
            }
            Action::Mixdown => self.start_mixdown(),
            Action::Duplicate => self.duplicate_selected_note(),
            Action::Quantize => self.quantize_selected(),
            Action::PlayPause => self.toggle_play(),
            Action::Stop => self.stop(),
            Action::About => self.shell.open_about(),
            Action::Settings => self.shell.open_settings(),
        }
    }

    fn handle_shortcuts(&mut self, ui: &egui::Ui) {
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let action = ui.input_mut(|input| {
            if input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::S,
            ) {
                Some(Action::SaveAs)
            } else if input.consume_key(egui::Modifiers::COMMAND, egui::Key::S) {
                Some(Action::Save)
            } else if input.consume_key(egui::Modifiers::COMMAND, egui::Key::O) {
                Some(Action::Open)
            } else if input.consume_key(egui::Modifiers::COMMAND, egui::Key::N) {
                Some(Action::New)
            } else if input.consume_key(egui::Modifiers::COMMAND, egui::Key::Z) {
                Some(Action::Undo)
            } else if input.consume_key(egui::Modifiers::COMMAND, egui::Key::Y) {
                Some(Action::Redo)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::Space) {
                Some(Action::PlayPause)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::D) {
                Some(Action::Duplicate)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::Q) {
                Some(Action::Quantize)
            } else {
                None
            }
        });
        if let Some(action) = action {
            self.handle_action(action);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let title = self.host.display_name();
        let dirty = self.host.is_dirty();
        let can_undo = self.host.can_undo();
        let can_redo = self.host.can_redo();
        let mut action = None;
        self.shell.top_bar(ui, &title, dirty, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New").clicked() {
                    action = Some(Action::New);
                    ui.close();
                }
                if ui.button("Open...").clicked() {
                    action = Some(Action::Open);
                    ui.close();
                }
                if ui.button("Save").clicked() {
                    action = Some(Action::Save);
                    ui.close();
                }
                if ui.button("Save As...").clicked() {
                    action = Some(Action::SaveAs);
                    ui.close();
                }
                if ui.button("Import MIDI...").clicked() {
                    action = Some(Action::ImportMidi);
                    ui.close();
                }
                if ui.button("Export MIDI...").clicked() {
                    action = Some(Action::ExportMidi);
                    ui.close();
                }
                if ui.button("Mixdown WAV...").clicked() {
                    action = Some(Action::Mixdown);
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(can_undo, egui::Button::new("Undo"))
                    .clicked()
                {
                    action = Some(Action::Undo);
                    ui.close();
                }
                if ui
                    .add_enabled(can_redo, egui::Button::new("Redo"))
                    .clicked()
                {
                    action = Some(Action::Redo);
                    ui.close();
                }
                ui.separator();
                if ui.button("Duplicate note (D)").clicked() {
                    action = Some(Action::Duplicate);
                    ui.close();
                }
                if ui.button("Quantize (Q)").clicked() {
                    action = Some(Action::Quantize);
                    ui.close();
                }
            });
            ui.menu_button("Transport", |ui| {
                if ui.button("Play / Pause").clicked() {
                    action = Some(Action::PlayPause);
                    ui.close();
                }
                if ui.button("Stop").clicked() {
                    action = Some(Action::Stop);
                    ui.close();
                }
            });
            if ui.button("About").clicked() {
                action = Some(Action::About);
            }
            if ui.button("Settings").clicked() {
                action = Some(Action::Settings);
            }
        });
        if let Some(action) = action {
            self.handle_action(action);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let running = self.jobs.jobs();
        self.shell.status_bar_with(ui, |ui| {
            for job in running {
                widgets::job_row(ui, job);
            }
        });
    }

    fn track_panel(&mut self, ui: &mut egui::Ui) {
        let tracks: Vec<TrackRow> = self
            .host
            .document()
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| {
                (
                    index,
                    track.name.clone(),
                    track.instrument,
                    track.volume,
                    track.mute,
                    track.solo,
                    track.effect,
                    self.host.document().master_volume,
                )
            })
            .collect();
        let selected = self.selected_track;
        let mut actions: Vec<(usize, TrackEdit)> = Vec::new();
        let mut add_track = false;

        egui::Panel::left("tracks-panel")
            .resizable(true)
            .default_size(230.0)
            .min_size(180.0)
            .show(ui, |ui| {
                widgets::section_label(ui, "Tracks");
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (index, name, instrument, volume, mute, solo, effect, _) in &tracks {
                            ui.group(|ui| {
                                let label = if *mute {
                                    format!("{name}  (muted)")
                                } else if *solo {
                                    format!("{name}  (solo)")
                                } else {
                                    name.clone()
                                };
                                if ui.selectable_label(*index == selected, label).clicked() {
                                    self.selected_track = *index;
                                }
                                let mut renamed = name.clone();
                                ui.add(TextEdit::singleline(&mut renamed).hint_text("Track name"));
                                if renamed != *name && !renamed.trim().is_empty() {
                                    actions.push((*index, TrackEdit::Rename(renamed)));
                                }
                                ComboBox::from_id_salt(format!("instrument-{index}"))
                                    .selected_text(instrument.label())
                                    .show_ui(ui, |ui| {
                                        for option in Instrument::all() {
                                            if ui
                                                .selectable_label(
                                                    *instrument == option,
                                                    option.label(),
                                                )
                                                .clicked()
                                            {
                                                actions.push((
                                                    *index,
                                                    TrackEdit::SetInstrument(option),
                                                ));
                                            }
                                        }
                                    });
                                effect_selector(ui, effect, *index, &mut actions);
                                effect_params_ui(ui, effect, *index, &mut actions);
                                let mut mixer_volume = *volume;
                                if ui
                                    .add(
                                        DragValue::new(&mut mixer_volume)
                                            .range(0.0..=1.0)
                                            .speed(0.01)
                                            .prefix("Vol "),
                                    )
                                    .changed()
                                {
                                    actions.push((*index, TrackEdit::SetVolume(mixer_volume)));
                                }
                                ui.horizontal(|ui| {
                                    let mute_label = if *mute { "Unmute" } else { "Mute" };
                                    if ui.button(mute_label).clicked() {
                                        actions.push((*index, TrackEdit::ToggleMute));
                                    }
                                    let solo_label = if *solo { "Unsolo" } else { "Solo" };
                                    if ui.button(solo_label).clicked() {
                                        actions.push((*index, TrackEdit::ToggleSolo));
                                    }
                                    if ui.small_button("Remove").clicked() {
                                        actions.push((*index, TrackEdit::Remove));
                                    }
                                });
                            });
                        }
                    });
                ui.separator();
                widgets::section_label(ui, "Master");
                let mut master = self.host.document().master_volume;
                if ui
                    .add(
                        DragValue::new(&mut master)
                            .range(0.0..=1.0)
                            .speed(0.01)
                            .prefix("Out "),
                    )
                    .changed()
                {
                    self.host.apply(|document| {
                        document.master_volume = Track::clamp_volume(master);
                    });
                }
                if ui.button("Add track").clicked() {
                    add_track = true;
                }
            });

        for (index, edit) in actions {
            self.apply_track_edit(index, edit);
        }
        if add_track {
            self.add_track();
        }
    }

    fn note_panel(&mut self, ui: &mut egui::Ui) {
        let track_index = self.selected_track;
        let Some(track) = self.host.document().tracks.get(track_index).cloned() else {
            egui::Panel::right("notes-panel")
                .resizable(true)
                .default_size(260.0)
                .min_size(200.0)
                .show(ui, |ui| {
                    widgets::section_label(ui, "Notes");
                    widgets::empty_state(ui, "No track", "Add a track to place notes.");
                });
            return;
        };

        let mut select: Option<u64> = None;
        let mut add = false;
        let mut edits: Vec<NoteEdit> = Vec::new();

        egui::Panel::right("notes-panel")
            .resizable(true)
            .default_size(260.0)
            .min_size(200.0)
            .show(ui, |ui| {
                widgets::section_label(ui, "Notes");
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for note in &track.notes {
                            let label = format!(
                                "#{} · {} · beat {:.1} · {:.1}b",
                                note.id, note.pitch, note.start_beat, note.length_beats
                            );
                            if ui
                                .selectable_label(self.selected_note == Some(note.id), label)
                                .clicked()
                            {
                                select = Some(note.id);
                            }
                        }
                    });
                ui.horizontal(|ui| {
                    if ui.button("Add note").clicked() {
                        add = true;
                    }
                    if ui
                        .add_enabled(self.selected_note.is_some(), egui::Button::new("Delete"))
                        .clicked()
                    {
                        edits.push(NoteEdit::Remove);
                    }
                });
                ui.separator();

                if let Some(id) = self.selected_note {
                    if let Some(note) = track.notes.iter().find(|note| note.id == id) {
                        ui.label(
                            RichText::new(format!("Selected note #{id}"))
                                .size(11.0)
                                .color(theme::MUTED),
                        );
                        let mut pitch = note.pitch;
                        let mut start = note.start_beat;
                        let mut length = note.length_beats;
                        let mut velocity = note.velocity;

                        ui.horizontal(|ui| {
                            ui.label("Pitch");
                            ui.add(DragValue::new(&mut pitch).range(PITCH_LOW..=PITCH_HIGH));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Start");
                            ui.add(
                                DragValue::new(&mut start)
                                    .range(0.0..=256.0)
                                    .speed(0.1)
                                    .suffix(" b"),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Length");
                            ui.add(
                                DragValue::new(&mut length)
                                    .range(0.125..=32.0)
                                    .speed(0.1)
                                    .suffix(" b"),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Velocity");
                            ui.add(DragValue::new(&mut velocity).range(0.0..=1.0).speed(0.02));
                        });

                        if pitch != note.pitch {
                            edits.push(NoteEdit::SetPitch(pitch));
                        }
                        if (start - note.start_beat).abs() > f64::EPSILON {
                            edits.push(NoteEdit::SetStart(start));
                        }
                        if (length - note.length_beats).abs() > f64::EPSILON {
                            edits.push(NoteEdit::SetLength(length));
                        }
                        if (velocity - note.velocity).abs() > f32::EPSILON {
                            edits.push(NoteEdit::SetVelocity(velocity));
                        }
                    }
                } else {
                    widgets::empty_state(
                        ui,
                        "No note selected",
                        "Click a note on the piano roll or add one.",
                    );
                }
            });

        if let Some(id) = select {
            self.selected_note = Some(id);
        }
        if add {
            let beat = self.playhead_beats.round();
            self.add_note(track_index, 60, beat);
        }
        for edit in edits {
            if matches!(edit, NoteEdit::Remove) {
                if let Some(id) = self.selected_note {
                    self.remove_note(track_index, id);
                }
            } else if let Some(id) = self.selected_note {
                self.apply_note_edit(track_index, id, edit);
            }
        }
    }

    fn central_panel(&mut self, ui: &mut egui::Ui) {
        let document = self.host.document().clone();
        let mut title = document.title.clone();
        let original_title = title.clone();
        let mut bpm = document.bpm;
        let original_bpm = bpm;
        let mut title_gained = false;
        let mut title_lost = false;
        let mut mixdown = false;
        let mut settings = false;
        let mut add_note: Option<(usize, u8, f64)> = None;
        let mut select: Option<(usize, u64)> = None;
        let mut drag_start: Option<(u64, DragMode, f32, f32, Note)> = None;
        let mut drag_delta: Option<(f32, f32)> = None;
        let mut drag_end = false;
        let mut quantize = false;

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Arrangement");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::toolbar_button(ui, "Quantize", "Snap notes to the grid (Q)")
                        .clicked()
                    {
                        quantize = true;
                    }
                    if widgets::toolbar_button(ui, "Mixdown WAV", "Export an offline WAV mix")
                        .clicked()
                    {
                        mixdown = true;
                    }
                    if widgets::toolbar_button(ui, "Settings", "Preferences").clicked() {
                        settings = true;
                    }
                });
            });
            ui.horizontal(|ui| {
                let label = if self.playing { "Pause" } else { "Play" };
                if ui.button(label).clicked() {
                    self.toggle_play();
                }
                if ui.button("Stop").clicked() {
                    self.stop();
                }
                ui.label(format!(
                    "{:.2} / {:.2} beats · {} notes · {}",
                    self.playhead_beats.min(document.duration_beats()),
                    document.duration_beats(),
                    document.note_count(),
                    if self.realtime {
                        "audio on"
                    } else {
                        "offline preview"
                    }
                ));
                let response = ui.add(
                    DragValue::new(&mut bpm)
                        .range(20.0..=300.0)
                        .speed(1.0)
                        .prefix("BPM "),
                );
                title_gained |= response.gained_focus();
                title_lost |= response.lost_focus();
            });
            ui.horizontal(|ui| {
                ui.label("Title");
                let response = ui.add(TextEdit::singleline(&mut title).desired_width(240.0));
                title_gained |= response.gained_focus();
                title_lost |= response.lost_focus();
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new("Click a note to select it, or click the grid to add one.")
                    .size(10.5)
                    .color(theme::FAINT),
            );

            let total_beats = (document.duration_beats() + 4.0).max(8.0);
            let content = Vec2::new(
                LABEL_WIDTH + total_beats as f32 * BEAT_WIDTH,
                RULER_HEIGHT + f32::from(PITCH_HIGH - PITCH_LOW + 1) * ROW_HEIGHT,
            );
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let (rect, response) = ui.allocate_exact_size(content, Sense::click_and_drag());
                    let painter = ui.painter_at(rect);
                    let origin = Pos2::new(rect.min.x + LABEL_WIDTH, rect.min.y + RULER_HEIGHT);

                    painter.rect_filled(rect, CornerRadius::same(4), theme::SUNKEN);
                    let last_beat = total_beats.ceil() as i32;
                    for beat in 0..=last_beat {
                        let x = origin.x + beat as f32 * BEAT_WIDTH;
                        painter.line_segment(
                            [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
                            Stroke::new(
                                1.0,
                                if beat % 4 == 0 {
                                    theme::BORDER_STRONG
                                } else {
                                    theme::BORDER
                                },
                            ),
                        );
                        if x < rect.max.x - 16.0 {
                            painter.text(
                                Pos2::new(x + 2.0, rect.min.y + 2.0),
                                Align2::LEFT_TOP,
                                beat.to_string(),
                                FontId::proportional(10.0),
                                theme::FAINT,
                            );
                        }
                    }
                    for pitch in PITCH_LOW..=PITCH_HIGH {
                        let y = origin.y + f32::from(PITCH_HIGH - pitch) * ROW_HEIGHT;
                        painter.line_segment(
                            [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
                            Stroke::new(1.0, theme::BORDER),
                        );
                        if pitch % 12 == 0 {
                            painter.text(
                                Pos2::new(rect.min.x + 4.0, y + 1.0),
                                Align2::LEFT_TOP,
                                format!("C{}", i32::from(pitch) / 12 - 1),
                                FontId::proportional(10.0),
                                theme::MUTED,
                            );
                        }
                    }
                    for (track_index, track) in document.tracks.iter().enumerate() {
                        let color = if track.mute {
                            theme::FAINT
                        } else {
                            instrument_color(track.instrument)
                        };
                        for note in &track.notes {
                            let note_rect = note_rect(note, origin);
                            let is_selected = track_index == self.selected_track
                                && self.selected_note == Some(note.id);
                            painter.rect_filled(
                                note_rect,
                                CornerRadius::same(3),
                                if is_selected {
                                    theme::ACCENT_LIGHT
                                } else {
                                    color
                                },
                            );
                            painter.rect_stroke(
                                note_rect,
                                CornerRadius::same(3),
                                Stroke::new(
                                    1.0,
                                    if is_selected {
                                        theme::TEXT
                                    } else {
                                        theme::BORDER_STRONG
                                    },
                                ),
                                StrokeKind::Inside,
                            );
                        }
                    }
                    if document.duration_beats() > 0.0 {
                        let x = origin.x + self.playhead_beats as f32 * BEAT_WIDTH;
                        painter.line_segment(
                            [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
                            Stroke::new(2.0, theme::ACCENT),
                        );
                    }

                    if response.clicked()
                        && let Some(position) = response.interact_pointer_pos()
                    {
                        let mut hit = None;
                        'search: for (track_index, track) in document.tracks.iter().enumerate() {
                            for note in &track.notes {
                                if note_rect(note, origin).contains(position) {
                                    hit = Some((track_index, note.id));
                                    break 'search;
                                }
                            }
                        }
                        if let Some((track_index, id)) = hit {
                            select = Some((track_index, id));
                        } else if position.x >= origin.x && position.y >= origin.y {
                            let beat = ((position.x - origin.x) / BEAT_WIDTH).max(0.0).round();
                            let pitch = (f32::from(PITCH_HIGH)
                                - (position.y - origin.y) / ROW_HEIGHT)
                                .round();
                            let pitch =
                                pitch.clamp(f32::from(PITCH_LOW), f32::from(PITCH_HIGH)) as u8;
                            add_note = Some((self.selected_track, pitch, f64::from(beat)));
                        }
                    }

                    if response.drag_started()
                        && let Some(position) = response.interact_pointer_pos()
                    {
                        let mut hit = None;
                        'drag_search: for (track_index, track) in document.tracks.iter().enumerate()
                        {
                            for note in &track.notes {
                                let rect = note_rect(note, origin);
                                if rect.contains(position) {
                                    let mode = if position.x >= rect.max.x - 6.0 {
                                        DragMode::Resize
                                    } else {
                                        DragMode::Move
                                    };
                                    hit = Some((track_index, *note, mode));
                                    break 'drag_search;
                                }
                            }
                        }
                        if let Some((track_index, note, mode)) = hit {
                            select = Some((track_index, note.id));
                            drag_start = Some((note.id, mode, position.x, position.y, note));
                        }
                    }
                    if let Some(position) = response.interact_pointer_pos()
                        && (response.dragged() || response.drag_stopped())
                        && self.drag.is_some()
                    {
                        drag_delta = Some((position.x, position.y));
                    }
                    if response.drag_stopped() {
                        drag_end = true;
                    }
                });
        });

        if title != original_title && !title.trim().is_empty() {
            self.host.apply(|document| document.title = title.clone());
        }
        if (bpm - original_bpm).abs() > f64::EPSILON {
            self.host.apply(|document| document.bpm = bpm);
        }
        if let Some((track_index, id)) = select {
            self.selected_track = track_index;
            self.selected_note = Some(id);
        }
        if let Some((note_id, mode, start_x, start_y, original)) = drag_start {
            self.drag = Some(NoteDrag {
                mode,
                note_id,
                start_x,
                start_y,
                original,
            });
            self.host.history_mut().begin_gesture();
        }
        if let Some((x, y)) = drag_delta
            && let Some(drag) = self.drag
        {
            let delta_beats = f64::from(x - drag.start_x) / f64::from(BEAT_WIDTH);
            let delta_pitch = ((drag.start_y - y) / ROW_HEIGHT).round() as i32;
            let updated = match drag.mode {
                DragMode::Move => edit::move_note(&drag.original, delta_beats, delta_pitch),
                DragMode::Resize => {
                    edit::resize_note(&drag.original, drag.original.length_beats + delta_beats)
                }
            };
            let track_index = self.selected_track;
            let note_id = drag.note_id;
            if let Some(note) = self
                .host
                .history_mut()
                .current_mut()
                .tracks
                .get_mut(track_index)
                .and_then(|track| track.notes.iter_mut().find(|note| note.id == note_id))
            {
                *note = updated;
            }
        }
        if drag_end && self.drag.is_some() {
            self.host.history_mut().end_gesture();
            self.drag = None;
            self.shell.set_status("Note edit recorded as one undo step");
        }
        if let Some((track_index, pitch, beat)) = add_note {
            self.add_note(track_index, pitch, beat);
        }
        if mixdown {
            self.start_mixdown();
        }
        if quantize {
            self.quantize_selected();
        }
        if settings {
            self.shell.open_settings();
        }
        self.clamp_selection();
    }

    fn unsaved_window(&mut self, ctx: &egui::Context) {
        if self.pending.is_none() {
            return;
        }
        let mut open = true;
        let mut choice = None;
        egui::Window::new("Unsaved changes")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.label("The current project has unsaved changes.");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Save and continue").clicked() {
                        choice = Some(PendingChoice::Save);
                    }
                    if ui.button("Discard").clicked() {
                        choice = Some(PendingChoice::Discard);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(PendingChoice::Cancel);
                    }
                });
            });
        if let Some(choice) = choice {
            self.resolve_pending(choice);
        } else if !open {
            self.pending = None;
        }
    }
}

impl eframe::App for MusicCraftApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_jobs();
        self.update_playhead(ui);
        self.handle_shortcuts(ui);
        self.top_bar(ui);
        self.status_bar(ui);
        self.track_panel(ui);
        self.note_panel(ui);
        self.central_panel(ui);
        self.unsaved_window(ui.ctx());
        self.shell.show_windows(ui.ctx());
        self.shell
            .update_title(ui.ctx(), &self.host.display_name(), self.host.is_dirty());
    }
}

fn note_rect(note: &Note, origin: Pos2) -> Rect {
    let pitch = note.pitch.clamp(PITCH_LOW, PITCH_HIGH);
    let x = origin.x + note.start_beat as f32 * BEAT_WIDTH;
    let y = origin.y + f32::from(PITCH_HIGH - pitch) * ROW_HEIGHT;
    Rect::from_min_size(
        Pos2::new(x, y + 1.0),
        Vec2::new(
            (note.length_beats as f32 * BEAT_WIDTH).max(3.0),
            ROW_HEIGHT - 2.0,
        ),
    )
}

fn effect_selector(
    ui: &mut egui::Ui,
    effect: &Effect,
    index: usize,
    actions: &mut Vec<(usize, TrackEdit)>,
) {
    ComboBox::from_id_salt(format!("effect-{index}"))
        .selected_text(effect.label())
        .show_ui(ui, |ui| {
            for option in [
                Effect::None,
                Effect::Delay {
                    time_beats: 0.5,
                    feedback: 0.35,
                    mix: 0.25,
                },
                Effect::Reverb {
                    mix: 0.2,
                    decay: 0.65,
                },
            ] {
                if ui
                    .selectable_label(effect.label() == option.label(), option.label())
                    .clicked()
                {
                    actions.push((index, TrackEdit::SetEffect(option)));
                }
            }
        });
}

fn effect_params_ui(
    ui: &mut egui::Ui,
    effect: &Effect,
    index: usize,
    actions: &mut Vec<(usize, TrackEdit)>,
) {
    match *effect {
        Effect::None => {}
        Effect::Delay {
            time_beats,
            feedback,
            mix,
        } => {
            let mut time = time_beats;
            let mut feedback_value = feedback;
            let mut mix_value = mix;
            ui.horizontal(|ui| {
                ui.add(DragValue::new(&mut time).range(0.125..=8.0).prefix("T "));
                if ui
                    .add(
                        DragValue::new(&mut feedback_value)
                            .range(0.0..=0.9)
                            .prefix("FB "),
                    )
                    .changed()
                {
                    actions.push((
                        index,
                        TrackEdit::SetEffect(Effect::Delay {
                            time_beats: time,
                            feedback: feedback_value,
                            mix: mix_value,
                        }),
                    ));
                }
                if ui
                    .add(
                        DragValue::new(&mut mix_value)
                            .range(0.0..=1.0)
                            .prefix("Mix "),
                    )
                    .changed()
                {
                    actions.push((
                        index,
                        TrackEdit::SetEffect(Effect::Delay {
                            time_beats: time,
                            feedback: feedback_value,
                            mix: mix_value,
                        }),
                    ));
                }
            });
        }
        Effect::Reverb { decay, mix } => {
            let mut decay_value = decay;
            let mut mix_value = mix;
            ui.horizontal(|ui| {
                if ui
                    .add(
                        DragValue::new(&mut decay_value)
                            .range(0.0..=0.95)
                            .prefix("Decay "),
                    )
                    .changed()
                {
                    actions.push((
                        index,
                        TrackEdit::SetEffect(Effect::Reverb {
                            decay: decay_value,
                            mix: mix_value,
                        }),
                    ));
                }
                if ui
                    .add(
                        DragValue::new(&mut mix_value)
                            .range(0.0..=1.0)
                            .prefix("Mix "),
                    )
                    .changed()
                {
                    actions.push((
                        index,
                        TrackEdit::SetEffect(Effect::Reverb {
                            decay: decay_value,
                            mix: mix_value,
                        }),
                    ));
                }
            });
        }
    }
}

fn instrument_color(instrument: Instrument) -> Color32 {
    match instrument {
        Instrument::Sine => theme::INFO,
        Instrument::Square => theme::SUCCESS,
        Instrument::Saw => theme::WARNING,
        Instrument::Triangle => theme::ACCENT_LIGHT,
        Instrument::Noise => theme::MUTED,
    }
}

fn slugify(title: &str) -> String {
    let mut slug = String::new();
    for character in title.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "untitled".to_owned()
    } else {
        slug.to_owned()
    }
}
