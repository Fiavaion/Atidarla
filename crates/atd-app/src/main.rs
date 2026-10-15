//! Atidarla application shell (PRD §5.2 `atd-app`).

mod app;
mod canvas;
mod dock;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("Atidarla")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Atidarla",
        options,
        Box::new(|cc| Ok(Box::new(app::AtidarlaApp::new(cc)))),
    )
}
