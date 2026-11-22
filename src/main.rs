//! MusicCraft application entry point.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod document;
mod edit;
mod engine;
mod midi;

use craftkit_ui::cli::{self, CliAction};
use craftkit_ui::{WindowOptions, run_native};

fn main() -> craftkit_ui::eframe::Result<()> {
    let action = match cli::parse_args(std::env::args().skip(1)) {
        Ok(action) => action,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };

    let options = match action {
        CliAction::Help => {
            print!(
                "{}",
                cli::help_text(
                    "musiccraft",
                    "MusicCraft — MIDI arrangement, built-in synth and offline WAV mixdown"
                )
            );
            return Ok(());
        }
        CliAction::Version => {
            println!("MusicCraft {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        CliAction::Run(options) => options,
    };

    run_native(WindowOptions::new("MusicCraft"), move |context| {
        let app = app::MusicCraftApp::new(context, options);
        Ok(Box::new(app) as Box<dyn craftkit_ui::eframe::App>)
    })
}
