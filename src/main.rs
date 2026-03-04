mod ai_agent;
mod app;
mod git_service;

use app::GitControlApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1380.0, 880.0])
            .with_min_inner_size([1080.0, 680.0])
            .with_title("Git Control")
            .with_decorations(true)
            .with_transparent(false),
        ..Default::default()
    };

    eframe::run_native(
        "Git Control",
        options,
        Box::new(|cc| Box::new(GitControlApp::new(cc))),
    )
}
