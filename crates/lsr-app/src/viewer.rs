//! Texture viewer (FR-P3): the viewed node's output drawn through a display
//! pass. Modes: RGBA over a checkerboard, RGB (alpha ignored), a single
//! channel (R, G, B or alpha) as grey. Exposure is in stops. Hovering shows
//! the pixel under the pointer.

use egui::Ui;
use lsr_gpu::fullscreen::FullscreenPass;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Rgba,
    Rgb,
    R,
    G,
    B,
    A,
}

impl Mode {
    const ALL: [(Mode, &'static str, &'static str); 6] = [
        (
            Mode::Rgba,
            "RGBA",
            "Colour over a checkerboard where transparent",
        ),
        (Mode::Rgb, "RGB", "Colour, ignoring alpha"),
        (Mode::R, "R", "Red channel as grey"),
        (Mode::G, "G", "Green channel as grey"),
        (Mode::B, "B", "Blue channel as grey"),
        (Mode::A, "A", "Alpha channel as grey"),
    ];

    fn index(self) -> f32 {
        Self::ALL
            .iter()
            .position(|(m, _, _)| *m == self)
            .unwrap_or(0) as f32
    }
}

const BODY: &str = r#"
    let c = textureSample(in0, samp, uv);
    let mode = i32(params.v[0].x + 0.5);
    let k = params.v[0].y;
    var rgb = c.rgb * k;
    var a = c.a;
    switch mode {
        case 1: { a = 1.0; }
        case 2: { rgb = vec3f(c.r * k); a = 1.0; }
        case 3: { rgb = vec3f(c.g * k); a = 1.0; }
        case 4: { rgb = vec3f(c.b * k); a = 1.0; }
        case 5: { rgb = vec3f(c.a); a = 1.0; }
        default: {}
    }
    let cell = floor(uv * params.v[1].xy / 12.0);
    let checker = select(0.32, 0.48, (i32(cell.x) + i32(cell.y)) % 2 == 0);
    return vec4f(mix(vec3f(checker), rgb, a), 1.0);
"#;

/// GPU side of the viewer: the display pass and its target.
pub struct TextureViewer {
    pub mode: Mode,
    /// Exposure in stops (multiplies colour by 2^ev).
    pub ev: f32,
    pass: Option<FullscreenPass>,
    display: Option<wgpu::Texture>,
    pub texture_id: Option<egui::TextureId>,
    /// Settings the display was last rendered with.
    rendered: Option<(Mode, f32)>,
}

impl std::fmt::Debug for TextureViewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextureViewer")
            .field("mode", &self.mode)
            .field("ev", &self.ev)
            .finish_non_exhaustive()
    }
}

impl Default for TextureViewer {
    fn default() -> Self {
        Self {
            mode: Mode::Rgba,
            ev: 0.0,
            pass: None,
            display: None,
            texture_id: None,
            rendered: None,
        }
    }
}

impl TextureViewer {
    /// True when the display must be redrawn although nothing cooked.
    pub fn stale(&self) -> bool {
        self.rendered != Some((self.mode, self.ev)) || self.texture_id.is_none()
    }

    /// Draws `source` into the display texture and hands it to egui.
    pub fn present(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut eframe::egui_wgpu::Renderer,
        source: &wgpu::Texture,
    ) {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let pass = self
            .pass
            .get_or_insert_with(|| FullscreenPass::new(device, "lsr-viewer", format, 1, "", BODY));
        let resized = self
            .display
            .as_ref()
            .is_none_or(|d| d.width() != source.width() || d.height() != source.height());
        if resized {
            self.display = Some(device.create_texture(&wgpu::TextureDescriptor {
                label: Some("lsr-viewer-display"),
                size: source.size(),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            }));
        }
        let display = self.display.as_ref().expect("created above");
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("lsr-viewer"),
        });
        pass.draw(
            device,
            queue,
            &mut encoder,
            &[source],
            &[
                [self.mode.index(), 2f32.powf(self.ev), 0.0, 0.0],
                [source.width() as f32, source.height() as f32, 0.0, 0.0],
            ],
            display,
        );
        queue.submit([encoder.finish()]);
        let view = display.create_view(&Default::default());
        match self.texture_id {
            Some(id) if !resized => renderer.update_egui_texture_from_wgpu_texture(
                device,
                &view,
                wgpu::FilterMode::Linear,
                id,
            ),
            _ => {
                if let Some(old) = self.texture_id.take() {
                    renderer.free_texture(&old);
                }
                self.texture_id =
                    Some(renderer.register_native_texture(device, &view, wgpu::FilterMode::Linear));
            }
        }
        self.rendered = Some((self.mode, self.ev));
    }

    /// The mode and exposure controls.
    pub fn toolbar(&mut self, ui: &mut Ui) {
        for (mode, label, help) in Mode::ALL {
            ui.selectable_value(&mut self.mode, mode, label)
                .on_hover_text(help);
        }
        ui.separator();
        ui.add(
            egui::DragValue::new(&mut self.ev)
                .speed(0.05)
                .range(-10.0..=10.0)
                .prefix("EV ")
                .fixed_decimals(1),
        )
        .on_hover_text("Exposure in stops; double-click to reset")
        .double_clicked()
        .then(|| self.ev = 0.0);
    }
}
