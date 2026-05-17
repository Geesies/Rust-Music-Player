mod app;
mod database;
mod eq;
mod library;
mod player;

use app::MusicPlayerApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();

    eframe::run_native(
        "Rust Music Player",
        options,
        Box::new(|_cc| Ok(Box::new(MusicPlayerApp::new()))),
    )
}
