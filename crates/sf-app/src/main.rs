//! SeqForge desktop app. Usage: `seqforge-gui [rom] [sequence]`.

mod app;
mod audio;
mod package;
mod pianoroll;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SeqForge")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([900.0, 560.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "SeqForge",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
