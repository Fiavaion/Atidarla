//! Smoke mode for CI and quick checks: `atidarla-app --smoke [frames]
//! [--screenshot out.png]` renders the shell for a number of frames, takes a
//! screenshot, prints a one-line JSON report and exits. A failure to start,
//! render or capture exits non-zero, so CI proves the shell runs on each OS
//! (M0 AC2).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::ColorImage;

/// Default number of frames rendered before the screenshot.
const DEFAULT_FRAMES: u32 = 120;
/// Give up if no screenshot has arrived by then.
const TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeArgs {
    pub frames: u32,
    pub screenshot: Option<PathBuf>,
}

impl SmokeArgs {
    /// Parses `--smoke [frames]` and `--screenshot <path>`; `None` when
    /// `--smoke` is absent.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = args.into_iter().peekable();
        let mut smoke = None;
        let mut screenshot = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--smoke" => {
                    let frames = match args.peek().and_then(|s| s.parse::<u32>().ok()) {
                        Some(n) => {
                            args.next();
                            n
                        }
                        None => DEFAULT_FRAMES,
                    };
                    if frames < 2 {
                        return Err("--smoke needs at least 2 frames".into());
                    }
                    smoke = Some(frames);
                }
                "--screenshot" => {
                    let path = args.next().ok_or("--screenshot needs a path")?;
                    screenshot = Some(PathBuf::from(path));
                }
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        if screenshot.is_some() && smoke.is_none() {
            return Err("--screenshot is only valid with --smoke".into());
        }
        Ok(smoke.map(|frames| SmokeArgs { frames, screenshot }))
    }
}

#[derive(Debug)]
pub struct Smoke {
    args: SmokeArgs,
    frame: u32,
    started: Instant,
    shot: Arc<Mutex<Option<Arc<ColorImage>>>>,
}

impl Smoke {
    pub fn new(args: SmokeArgs) -> Self {
        Self {
            args,
            frame: 0,
            started: Instant::now(),
            shot: Arc::default(),
        }
    }

    /// Called once per frame.
    pub fn tick(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        ctx.request_repaint();

        if self.frame == self.args.frames {
            let slot = Arc::clone(&self.shot);
            let repaint = ctx.clone();
            ctx.request_screenshot(move |image| {
                *slot.lock().expect("screenshot slot") = Some(image);
                repaint.request_repaint();
            });
        }

        let image = self.shot.lock().expect("screenshot slot").take();
        if let Some(image) = image {
            if let Some(path) = &self.args.screenshot
                && let Err(e) = write_png(path, &image)
            {
                fail(&format!("writing {}: {e}", path.display()));
            }
            println!(
                "{}",
                serde_json::json!({
                    "smoke": "ok",
                    "frames": self.frame,
                    "elapsed_ms": self.started.elapsed().as_millis() as u64,
                    "screenshot_size": image.size,
                })
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if self.started.elapsed() > TIMEOUT {
            fail(&format!(
                "no screenshot after {} frames in {TIMEOUT:?}",
                self.frame
            ));
        }
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("smoke: FAILED: {msg}");
    std::process::exit(1);
}

fn write_png(path: &Path, image: &ColorImage) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let [w, h] = image.size;
    let mut encoder = png::Encoder::new(file, w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(image.as_raw())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<SmokeArgs>, String> {
        SmokeArgs::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_args_means_interactive() {
        assert_eq!(parse(&[]), Ok(None));
    }

    #[test]
    fn smoke_with_and_without_frames() {
        assert_eq!(
            parse(&["--smoke"]),
            Ok(Some(SmokeArgs {
                frames: DEFAULT_FRAMES,
                screenshot: None
            }))
        );
        assert_eq!(
            parse(&["--smoke", "30", "--screenshot", "a.png"]),
            Ok(Some(SmokeArgs {
                frames: 30,
                screenshot: Some("a.png".into())
            }))
        );
        assert_eq!(
            parse(&["--screenshot", "a.png", "--smoke"]),
            Ok(Some(SmokeArgs {
                frames: DEFAULT_FRAMES,
                screenshot: Some("a.png".into())
            }))
        );
    }

    #[test]
    fn rejects_bad_args() {
        assert!(parse(&["--smoke", "1"]).is_err());
        assert!(parse(&["--screenshot"]).is_err());
        assert!(parse(&["--screenshot", "a.png"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
