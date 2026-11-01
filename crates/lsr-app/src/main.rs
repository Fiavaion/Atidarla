//! Lasair application shell (PRD §5.2 `lsr-app`).

mod accessibility;
mod ai_assist;
mod ai_badges;
mod ai_hosts;
mod app;
mod assistant;
mod canvas;
mod channels;
mod dat_view;
mod diff_view;
mod dock;
mod heartbeat;
mod learn;
mod mappings;
mod media_bin;
mod merge;
mod models_panel;
mod outputs;
mod params;
mod patch;
mod project;
mod record;
mod settings;
mod smoke;
mod stage;
mod storage_panel;
mod suggest;
mod textport;
mod timeline;
mod viewer;
mod wires;

use std::process::ExitCode;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    // An optional project folder first, then smoke-mode flags.
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let open = (!args.is_empty() && !args[0].starts_with('-'))
        .then(|| std::path::PathBuf::from(args.remove(0)));
    let oscquery = args
        .iter()
        .position(|a| a == "--oscquery")
        .map(|i| args.remove(i))
        .is_some();
    let kiosk = args
        .iter()
        .position(|a| a == "--kiosk")
        .map(|i| args.remove(i))
        .is_some();
    // Run the opened project's Python without asking (unattended shows,
    // `lasair run --allow-python-ops`; docs/python-ops.md).
    let allow_python = args
        .iter()
        .position(|a| a == "--allow-python-ops")
        .map(|i| args.remove(i))
        .is_some();
    let heartbeat = match args.iter().position(|a| a == "--heartbeat") {
        Some(i) if i + 1 < args.len() => {
            args.remove(i);
            Some(std::path::PathBuf::from(args.remove(i)))
        }
        Some(_) => {
            eprintln!("error: --heartbeat needs a file path");
            return ExitCode::from(2);
        }
        None => None,
    };
    let smoke = match smoke::SmokeArgs::parse(args) {
        Ok(smoke) => smoke,
        Err(e) => {
            eprintln!(
                "error: {e}\nusage: lasair-app [project.lasair] [--oscquery] [--kiosk] [--allow-python-ops] [--heartbeat file] [--smoke [frames]] [--screenshot out.png]"
            );
            return ExitCode::from(2);
        }
    };
    // The default egui device plus the optional features the engine uses.
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor = lsr_gpu::instance_descriptor();
    // Limits: the WebGPU defaults raised to what the adapter offers where
    // geometry needs it (storage buffers, buffer sizes, 8K textures).
    setup.device_descriptor = std::sync::Arc::new(|adapter| wgpu::DeviceDescriptor {
        label: Some("lasair device"),
        required_features: lsr_gpu::optional_features(adapter),
        required_limits: lsr_gpu::required_limits(adapter),
        ..Default::default()
    });
    let wgpu_options = eframe::egui_wgpu::WgpuConfiguration {
        wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    };
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        viewport: egui::ViewportBuilder::default()
            .with_title("Lasair")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([800.0, 500.0])
            .with_fullscreen(kiosk)
            .with_decorations(!kiosk),
        ..Default::default()
    };
    let heartbeat = heartbeat.and_then(|path| {
        heartbeat::Heartbeat::start(path)
            .inspect_err(|e| eprintln!("warning: heartbeat: {e}"))
            .ok()
    });
    let shell = app::Shell {
        kiosk,
        heartbeat,
        allow_python,
    };
    let result = eframe::run_native(
        "Lasair",
        options,
        Box::new(|cc| {
            Ok(Box::new(app::LasairApp::new(
                cc, smoke, open, oscquery, shell,
            )))
        }),
    );
    // Share workers stop Syphon/Spout and release the GPU on their own
    // threads; exiting under them can trap on macOS, so give them a moment.
    lsr_ops_img::share_wait_for_workers(std::time::Duration::from_secs(2));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
