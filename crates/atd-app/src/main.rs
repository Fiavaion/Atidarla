//! Atidarla application shell (PRD §5.2 `atd-app`).

mod app;
mod canvas;
mod dock;
mod smoke;

use std::process::ExitCode;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let smoke = match smoke::SmokeArgs::parse(std::env::args().skip(1)) {
        Ok(smoke) => smoke,
        Err(e) => {
            eprintln!("error: {e}\nusage: atidarla-app [--smoke [frames]] [--screenshot out.png]");
            return ExitCode::from(2);
        }
    };
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("Atidarla")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Atidarla",
        options,
        Box::new(|cc| Ok(Box::new(app::AtidarlaApp::new(cc, smoke)))),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
