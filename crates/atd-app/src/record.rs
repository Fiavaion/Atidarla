//! Record (FR-S10): writes the viewed image (the live composition) to a
//! movie file while the app runs.
//!
//! Each frame the viewed texture is copied into one of a few readback
//! buffers, mapped asynchronously and read on a later frame; the pixels go
//! to an `atd_io` [`Recorder`], which encodes on its own thread. Nothing
//! here waits for the GPU, the encoder or the disk (rule 1): when every
//! readback buffer is in flight, or the encoder has no free frame, the
//! frame is skipped and counted. Frames are written at a fixed 60 per
//! second, so a recording plays at the speed it was made when the app
//! keeps up with 60 fps. The file dialog runs on its own thread, and so
//! does closing the file.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use atd_gpu::capture::Capture;
use atd_io::video::record::{Codec, ENABLED, Recorder, Settings, Stats};

/// Frames per second of a live recording.
const FPS: u32 = 60;
/// Readback buffers in flight.
const SLOTS: usize = 3;

enum State {
    Idle,
    /// The save dialog is open.
    Picking(Receiver<Option<PathBuf>>),
    /// A file is chosen; recording starts with the next frame (its size).
    Starting(PathBuf),
    Recording(Box<Live>),
    /// The file is being closed on a worker thread.
    Closing(Receiver<Result<Stats, String>>, PathBuf),
}

struct Live {
    recorder: Recorder,
    capture: Capture,
    path: PathBuf,
    /// The frame being filled from capture rows.
    filling: Option<Vec<u8>>,
}

/// The Record button and the recording behind it.
pub struct RecordPanel {
    state: State,
    /// Last result or error, and whether it is an error.
    pub message: Option<(String, bool)>,
}

impl Default for RecordPanel {
    fn default() -> Self {
        Self {
            state: State::Idle,
            message: None,
        }
    }
}

impl std::fmt::Debug for RecordPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordPanel")
            .field("recording", &self.is_recording())
            .finish_non_exhaustive()
    }
}

impl RecordPanel {
    pub fn is_recording(&self) -> bool {
        matches!(self.state, State::Starting(_) | State::Recording(_))
    }

    /// Starts recording to `path` (`.mov`: ProRes 422, `.mp4`: H.264).
    pub fn start(&mut self, path: PathBuf) {
        self.message = None;
        self.state = State::Starting(path);
    }

    /// Stops recording; the file is closed on a worker thread.
    pub fn stop(&mut self) {
        let state = std::mem::replace(&mut self.state, State::Idle);
        let State::Recording(live) = state else {
            return;
        };
        let Live {
            recorder,
            capture,
            path,
            ..
        } = *live;
        let skipped = capture.skipped;
        let (tx, rx) = channel();
        let spawned = std::thread::Builder::new()
            .name("atd-record-close".into())
            .spawn(move || {
                let _ = tx.send(recorder.finish().map(|mut s| {
                    s.dropped += skipped;
                    s
                }));
            });
        match spawned {
            Ok(_) => self.state = State::Closing(rx, path),
            Err(e) => self.message = Some((format!("cannot close the recording: {e}"), true)),
        }
    }

    /// The Record / Stop button and the recording's status, for a toolbar.
    pub fn toolbar(&mut self, ui: &mut egui::Ui) {
        self.poll_dialogs();
        match &self.state {
            State::Idle => {
                let button = ui.add_enabled(ENABLED, egui::Button::new("Record"));
                let button = if ENABLED {
                    button.on_hover_text(
                        "Record the viewed image to a movie: .mov is ProRes 422, .mp4 is H.264 \
                         (hardware encoder). 60 frames a second.",
                    )
                } else {
                    button.on_disabled_hover_text(
                        "This build has no FFmpeg: rebuild with `--features atd-io/ffmpeg` to \
                         record (docs/render.md).",
                    )
                };
                if button.clicked() {
                    let pick = rfd::AsyncFileDialog::new()
                        .set_title("Record to")
                        .add_filter("ProRes (.mov)", &["mov"])
                        .add_filter("H.264 (.mp4)", &["mp4"])
                        .set_file_name("recording.mov")
                        .save_file();
                    let (tx, rx) = channel();
                    let spawned = std::thread::Builder::new()
                        .name("atd-record-dialog".into())
                        .spawn(move || {
                            let picked = pollster::block_on(pick).map(|h| h.path().to_owned());
                            let _ = tx.send(picked);
                        });
                    if spawned.is_ok() {
                        self.state = State::Picking(rx);
                    }
                }
            }
            State::Picking(_) => {
                ui.add_enabled(false, egui::Button::new("Record"));
            }
            State::Starting(_) | State::Recording(_) => {
                if ui
                    .button(egui::RichText::new("Stop").color(egui::Color32::from_rgb(230, 80, 80)))
                    .on_hover_text("Stop recording and close the file")
                    .clicked()
                {
                    self.stop();
                }
                if let State::Recording(live) = &self.state {
                    let frames = live.recorder.submitted();
                    let dropped = live.recorder.dropped() + live.capture.skipped;
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 80, 80),
                        format!(
                            "REC {:.1} s, {frames} frames{}",
                            frames as f64 / f64::from(FPS),
                            if dropped > 0 {
                                format!(", {dropped} dropped")
                            } else {
                                String::new()
                            }
                        ),
                    );
                }
            }
            State::Closing(..) => {
                ui.weak("Closing the file…");
            }
        }
        if let Some((m, error)) = &self.message {
            if *error {
                ui.colored_label(egui::Color32::from_rgb(230, 80, 80), m);
            } else {
                ui.weak(m);
            }
        }
    }

    fn poll_dialogs(&mut self) {
        if let State::Picking(rx) = &self.state
            && let Ok(picked) = rx.try_recv()
        {
            match picked {
                Some(mut path) => {
                    if Codec::for_path(&path).is_none() {
                        path.set_extension("mov");
                    }
                    self.start(path);
                }
                None => self.state = State::Idle,
            }
        }
        if let State::Closing(rx, path) = &self.state
            && let Ok(result) = rx.try_recv()
        {
            self.message = Some(match result {
                Ok(s) => (
                    format!(
                        "Recorded {} frames to {}{}",
                        s.frames,
                        path.display(),
                        if s.dropped > 0 {
                            format!(" ({} dropped)", s.dropped)
                        } else {
                            String::new()
                        }
                    ),
                    false,
                ),
                Err(e) => (e, true),
            });
            self.state = State::Idle;
        }
    }

    /// Records `texture` (the viewed image) for this frame. Call once per
    /// app frame while [`Self::is_recording`].
    pub fn frame(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) {
        if let State::Starting(path) = &self.state {
            let path = path.clone();
            if !matches!(
                texture.format(),
                wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb
            ) {
                self.fail(format!(
                    "Record needs an 8-bit RGBA image; this one is {:?}",
                    texture.format()
                ));
                return;
            }
            let codec = Codec::for_path(&path).unwrap_or(Codec::ProRes422);
            match Recorder::start(Settings {
                path: path.clone(),
                width: texture.width(),
                height: texture.height(),
                fps: (FPS, 1),
                codec,
                bitrate: None,
            }) {
                Ok(recorder) => {
                    self.state = State::Recording(Box::new(Live {
                        recorder,
                        capture: Capture::new(device, texture.width(), texture.height(), SLOTS),
                        path,
                        filling: None,
                    }));
                }
                Err(e) => {
                    self.fail(e);
                    return;
                }
            }
        }
        let State::Recording(live) = &mut self.state else {
            return;
        };
        if live.recorder.failed() {
            // The encoder stopped (could not open the file, a full disk):
            // closing reports why.
            self.stop();
            return;
        }
        if (texture.width(), texture.height()) != live.capture.size() {
            self.stop();
            self.message = Some((
                "Recording stopped: the viewed image changed size".into(),
                true,
            ));
            return;
        }
        let Live {
            recorder,
            capture,
            filling,
            ..
        } = &mut **live;
        let (cw, ch) = capture.size();
        let row = cw as usize * 4;
        let last = ch.saturating_sub(1);
        capture.poll(device, |pixels, y| {
            if y == 0 {
                *filling = recorder.try_frame();
            }
            if let Some(frame) = filling {
                let at = y as usize * row;
                frame[at..at + row].copy_from_slice(pixels);
            }
            if y == last
                && let Some(frame) = filling.take()
            {
                let _ = recorder.submit(frame);
            }
        });
        capture.copy(device, queue, texture);
    }

    fn fail(&mut self, e: String) {
        self.state = State::Idle;
        self.message = Some((e, true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = atd_gpu::primary_instance();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    fn texture(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// The app's Record hook end to end (with FFmpeg): start, frames, stop,
    /// the file closed on its worker and reported.
    #[test]
    fn record_panel_writes_a_movie() {
        if !ENABLED {
            // The default build: the button is disabled, nothing to record.
            let mut panel = RecordPanel::default();
            panel.start("x.mov".into());
            assert!(panel.is_recording());
            return;
        }
        let Some((device, queue)) = device() else {
            return;
        };
        let (w, h) = (64, 32);
        let tex = texture(&device, w, h);
        let path = std::env::temp_dir().join(format!("atd-app-record-{}.mov", std::process::id()));
        let mut panel = RecordPanel::default();
        panel.start(path.clone());
        for seed in 0..40u8 {
            atd_interop::readback::write_rgba8(
                &queue,
                &tex,
                &atd_interop::readback::test_pattern(w, h, seed),
            );
            panel.frame(&device, &queue, &tex);
            // An app frame later.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panel.stop();
        let start = std::time::Instant::now();
        while panel.message.is_none() {
            panel.poll_dialogs();
            assert!(start.elapsed().as_secs() < 30, "the file closes");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let (msg, error) = panel.message.clone().unwrap();
        assert!(!error, "{msg}");
        assert!(msg.starts_with("Recorded "), "{msg}");
        let frames: u64 = msg["Recorded ".len()..]
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(frames >= 30, "most of 40 frames arrive: {msg}");
        assert!(std::fs::metadata(&path).unwrap().len() > 0);
        let _ = std::fs::remove_file(&path);
    }

    /// Frames come back in order, tightly packed, without the caller ever
    /// waiting; the test waits only to finish.
    #[test]
    fn capture_reads_frames_back_in_order() {
        let Some((device, queue)) = device() else {
            assert!(
                !std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0"),
                "ATD_REQUIRE_GPU is set but there is no GPU adapter"
            );
            return;
        };
        let (w, h) = (70, 9);
        let texture = texture(&device, w, h);
        let mut capture = Capture::new(&device, w, h, SLOTS);
        let mut got: Vec<Vec<u8>> = Vec::new();
        let mut current = Vec::new();
        let mut sink = |row: &[u8], y: u32| {
            if y == 0 {
                current.clear();
            }
            current.extend_from_slice(row);
            if y == h - 1 {
                got.push(current.clone());
            }
        };
        for seed in 0..5u8 {
            atd_interop::readback::write_rgba8(
                &queue,
                &texture,
                &atd_interop::readback::test_pattern(w, h, seed),
            );
            capture.copy(&device, &queue, &texture);
            capture.poll(&device, &mut sink);
        }
        let start = std::time::Instant::now();
        while capture.busy() {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            capture.poll(&device, &mut sink);
            assert!(start.elapsed().as_secs() < 30);
        }
        let want: Vec<Vec<u8>> = (0..5u8)
            .map(|s| atd_interop::readback::test_pattern(w, h, s))
            .collect();
        // Frames skipped while every buffer was busy are counted, the rest
        // arrive in order.
        assert_eq!(got.len() as u64 + capture.skipped, 5);
        let mut it = want.iter();
        for frame in &got {
            assert!(it.any(|w| w == frame), "frames arrive in order, intact");
        }
    }
}
