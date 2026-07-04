// release 构建下隐藏控制台黑窗（仅 Windows）。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio;
mod config;
mod hotkeys;
mod platform;
mod profile;

fn main() -> eframe::Result<()> {
    env_logger::init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([440.0, 640.0])
            .with_min_inner_size([380.0, 460.0])
            .with_title("VociePlayer"),
        ..Default::default()
    };

    eframe::run_native(
        "VociePlayer",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)) as Box<dyn eframe::App>)),
    )
}
