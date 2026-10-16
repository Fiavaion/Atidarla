//! Atidarla application shell (PRD §5.2 `atd-app`).

mod app;
mod canvas;
mod channels;
mod dock;
mod params;
mod patch;
mod project;
mod smoke;
mod suggest;
mod viewer;

use std::process::ExitCode;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    // An optional project folder first, then smoke-mode flags.
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let open = (!args.is_empty() && !args[0].starts_with('-'))
        .then(|| std::path::PathBuf::from(args.remove(0)));
    let smoke = match smoke::SmokeArgs::parse(args) {
        Ok(smoke) => smoke,
        Err(e) => {
            eprintln!(
                "error: {e}\nusage: atidarla-app [project.atidarla] [--smoke [frames]] [--screenshot out.png]"
            );
            return ExitCode::from(2);
        }
    };
    // The default egui device plus the optional features the engine uses.
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.device_descriptor = std::sync::Arc::new(|adapter| {
        let base = if adapter.get_info().backend == wgpu::Backend::Gl {
            wgpu::Limits::downlevel_webgl2_defaults()
        } else {
            wgpu::Limits::default()
        };
        wgpu::DeviceDescriptor {
            label: Some("atidarla device"),
            required_features: atd_gpu::optional_features(adapter),
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: 8192,
                ..base
            },
            ..Default::default()
        }
    });
    let wgpu_options = eframe::egui_wgpu::WgpuConfiguration {
        wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    };
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        viewport: egui::ViewportBuilder::default()
            .with_title("Atidarla")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Atidarla",
        options,
        Box::new(|cc| Ok(Box::new(app::AtidarlaApp::new(cc, smoke, open)))),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
