//! Syphon's directory, which hears about servers through distributed
//! notifications delivered on the main thread's run loop. libtest runs
//! tests off the main thread, so this binary has its own `main`
//! (`harness = false`): the check runs on a thread while the main thread
//! runs its run loop, as the app's event loop does.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    let check = std::thread::spawn(macos::directory_sees_servers_come_and_go);
    while !check.is_finished() {
        lsr_interop::syphon::run_main_loop(0.02);
    }
    match check.join() {
        Ok(()) => println!("test directory_sees_servers_come_and_go ... ok"),
        Err(_) => {
            eprintln!("test directory_sees_servers_come_and_go ... FAILED");
            std::process::exit(101);
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::time::{Duration, Instant};

    use lsr_interop::readback::{test_pattern, write_rgba8};
    use lsr_interop::syphon::{SyphonServer, directory_servers};

    fn until(what: &str, mut ok: impl FnMut() -> bool) {
        let start = Instant::now();
        while !ok() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "timed out: {what}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn directory_sees_servers_come_and_go() {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::METAL;
        let instance = wgpu::Instance::new(desc);
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            let required =
                std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "LSR_REQUIRE_GPU is set but there is no Metal adapter"
            );
            eprintln!("skipping: no Metal adapter");
            return;
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let name = format!("lsr-test-{}-directory", std::process::id());
        let src = lsr_interop::metal::SharedSurface::create(
            &device,
            16,
            16,
            wgpu::TextureFormat::Bgra8Unorm,
        )
        .unwrap();
        write_rgba8(&queue, &src.texture, &test_pattern(16, 16, 0));

        let mut server = SyphonServer::new(&device, &queue, &name).unwrap();
        server.publish(&src.texture).unwrap();
        let uuid = server.info().uuid.clone();
        until("the directory lists the server", || {
            directory_servers()
                .unwrap()
                .iter()
                .any(|s| s.uuid == uuid && s.name == name && !s.app_name.is_empty())
        });
        drop(server);
        until("the directory drops the server", || {
            !directory_servers().unwrap().iter().any(|s| s.uuid == uuid)
        });
    }
}
