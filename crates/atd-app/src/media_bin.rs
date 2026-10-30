//! The Media Bin in the Stage tab (FR-B3): batch results with their
//! metadata, filtered by wedge or attribute, used as clips.
//!
//! A background thread watches `media/bin/index.toml` (re-read when it
//! changes, checked every second) and decodes thumbnails, so the UI thread
//! never waits on disk (CLAUDE.md rule 1). "Use as clip" (or dragging a
//! thumbnail onto an empty clip cell) adds a Movie In node (`img.movie_in`)
//! reading the image by its project-relative path, wired into a free clip
//! slot of the deck: one undo step.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, SystemTime};

use atd_core::graph::{Edit, Endpoint, Network, Node, NodeId, Param, Wire};
use atd_format::media_bin::{BinEntry, MediaBin};
use atd_gpu::engine::Engine;
use egui::{Color32, ColorImage, TextureHandle, TextureOptions, Ui};
use serde_json::json;

/// Thumbnail size (longest side), in pixels.
const THUMB: usize = 96;
/// How often the index file is checked for changes.
const WATCH: Duration = Duration::from_secs(1);

/// A thumbnail dragged onto the Stage grid.
#[derive(Debug, Clone)]
pub struct BinClip {
    /// `media/bin/<hash>.png`.
    pub path: String,
    pub hash: String,
}

enum ToLoader {
    Thumb(String, PathBuf),
}

enum FromLoader {
    Index(Result<MediaBin, String>),
    Thumb(String, Result<ColorImage, String>),
}

/// The background thread of one project's bin.
struct Loader {
    dir: PathBuf,
    tx: Sender<ToLoader>,
    rx: Receiver<FromLoader>,
}

enum Thumb {
    Pending,
    Ready(TextureHandle),
    Failed(String),
}

#[derive(Default)]
pub struct MediaBinPanel {
    loader: Option<Loader>,
    bin: MediaBin,
    error: Option<String>,
    /// Show only this wedge (`None`: all).
    wedge: Option<String>,
    /// Show only entries with an attribute containing this text.
    filter: String,
    thumbs: HashMap<String, Thumb>,
    selected: Option<String>,
}

impl std::fmt::Debug for MediaBinPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaBinPanel")
            .field("entries", &self.bin.len())
            .finish_non_exhaustive()
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Nearest-neighbour downscale to at most [`THUMB`] pixels a side.
fn thumbnail(img: &atd_io::image::Image) -> ColorImage {
    let (w, h) = (img.width as usize, img.height as usize);
    let scale = (THUMB as f64 / w.max(h) as f64).min(1.0);
    let (tw, th) = (
        ((w as f64 * scale).round() as usize).max(1),
        ((h as f64 * scale).round() as usize).max(1),
    );
    let mut rgba = Vec::with_capacity(tw * th * 4);
    for y in 0..th {
        let sy = y * h / th;
        for x in 0..tw {
            let sx = x * w / tw;
            rgba.extend_from_slice(&img.rgba[(sy * w + sx) * 4..][..4]);
        }
    }
    ColorImage::from_rgba_unmultiplied([tw, th], &rgba)
}

fn run_loader(dir: &Path, rx: &Receiver<ToLoader>, tx: &Sender<FromLoader>, ctx: &egui::Context) {
    let index = dir.join(atd_format::media_bin::INDEX);
    let mut stamp: Option<Option<SystemTime>> = None;
    loop {
        let now = modified(&index);
        if stamp != Some(now) {
            stamp = Some(now);
            let bin = atd_format::media_bin::load_bin(dir).map_err(|e| e.to_string());
            if tx.send(FromLoader::Index(bin)).is_err() {
                return;
            }
            ctx.request_repaint();
        }
        match rx.recv_timeout(WATCH) {
            Ok(ToLoader::Thumb(hash, path)) => {
                let img = atd_io::image::load(&path).map(|i| thumbnail(&i));
                if tx.send(FromLoader::Thumb(hash, img)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Whether `e` has an attribute (`name=value`) containing `filter`,
/// ignoring case.
fn matches(e: &BinEntry, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let f = filter.to_lowercase();
    e.attributes.iter().any(|(k, v)| {
        let v = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
        format!("{k}={v}").to_lowercase().contains(&f)
    })
}

/// The hover text of an entry.
fn describe(e: &BinEntry) -> String {
    let mut s = format!(
        "{} · item {} · {}×{}\ncooked on {} in {:.0} ms · {}\n",
        e.wedge,
        e.index,
        e.size[0],
        e.size[1],
        e.host,
        e.cook_ms,
        atd_format::media_bin::created_text(e.created)
    );
    for (k, v) in &e.attributes {
        let v = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
        s += &format!("{k} = {v}\n");
    }
    s += &format!("media/bin/{}.png", e.hash);
    s
}

/// The edits that add Movie In node `id` reading `clip` and, given a
/// layer and slot, wire it into that slot.
pub fn clip_edits(
    net: &Network,
    id: NodeId,
    clip: &BinClip,
    slot: Option<(NodeId, usize)>,
) -> Vec<Edit> {
    let short: String = clip.hash.chars().take(8).collect();
    let name = net.unique_name(&format!("bin_{short}_"));
    let mut node = Node::new(id, "img.movie_in", name);
    if let Some((layer, _)) = slot
        && let Some(l) = net.node(layer)
    {
        node.pos = [l.pos[0] - 220.0, l.pos[1]];
    }
    let mut edits = vec![
        Edit::AddNode { node },
        Edit::SetParam {
            node: id,
            param: "file".into(),
            value: Some(Param::constant(json!(clip.path))),
        },
    ];
    if let Some((layer, slot)) = slot {
        edits.push(Edit::Connect {
            wire: Wire::new(
                Endpoint::new(id, "out"),
                Endpoint::new(layer, format!("clip{slot}")),
            ),
        });
    }
    edits
}

impl MediaBinPanel {
    /// Starts watching the bin of the project at `dir` (stops watching
    /// the previous one).
    fn watch(&mut self, dir: Option<&Path>, ctx: &egui::Context) {
        if self.loader.as_ref().map(|l| l.dir.as_path()) == dir {
            return;
        }
        *self = Self::default();
        let Some(dir) = dir else { return };
        let (to_tx, to_rx) = channel();
        let (from_tx, from_rx) = channel();
        let (d, c) = (dir.to_owned(), ctx.clone());
        let spawned = std::thread::Builder::new()
            .name("atd-media-bin".into())
            .spawn(move || run_loader(&d, &to_rx, &from_tx, &c));
        if let Err(e) = spawned {
            self.error = Some(format!("cannot watch the media bin: {e}"));
            return;
        }
        self.loader = Some(Loader {
            dir: dir.to_owned(),
            tx: to_tx,
            rx: from_rx,
        });
    }

    fn receive(&mut self, ctx: &egui::Context) {
        let Some(loader) = &self.loader else { return };
        while let Ok(msg) = loader.rx.try_recv() {
            match msg {
                FromLoader::Index(Ok(bin)) => {
                    // Images may have been re-made: reload thumbnails.
                    self.thumbs.clear();
                    self.bin = bin;
                    self.error = None;
                }
                FromLoader::Index(Err(e)) => self.error = Some(e),
                FromLoader::Thumb(hash, Ok(img)) => {
                    let tex = ctx.load_texture(format!("bin-{hash}"), img, TextureOptions::LINEAR);
                    self.thumbs.insert(hash, Thumb::Ready(tex));
                }
                FromLoader::Thumb(hash, Err(e)) => {
                    self.thumbs.insert(hash, Thumb::Failed(e));
                }
            }
        }
    }

    /// The bin; returns edits to apply ("Use as clip"). `free_slot` gives a
    /// free clip slot of the shown deck, if there is one.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        engine: &mut Engine,
        free_slot: Option<(NodeId, usize)>,
    ) -> Option<(&'static str, Vec<Edit>)> {
        let dir = engine.project_dir().map(Path::to_owned);
        self.watch(dir.as_deref(), ui.ctx());
        self.receive(ui.ctx());
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            ui.strong("Media Bin");
            if self.loader.is_none() {
                ui.weak("Save the project to use its media bin.");
                return;
            }
            let wedges: BTreeSet<&str> = self.bin.entries().iter().map(|e| e.wedge.as_str()).collect();
            egui::ComboBox::from_id_salt("bin-wedge")
                .selected_text(self.wedge.as_deref().unwrap_or("all wedges"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.wedge, None, "all wedges");
                    for w in wedges {
                        ui.selectable_value(&mut self.wedge, Some(w.to_owned()), w);
                    }
                })
                .response
                .on_hover_text("Show one wedge's results");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("filter: prompt=fox")
                    .desired_width(160.0),
            )
            .on_hover_text("Show results with an attribute (name=value) containing this text");
            let selected = self
                .selected
                .as_ref()
                .and_then(|h| self.bin.entries().iter().find(|e| &e.hash == h));
            let use_clip = ui
                .add_enabled(selected.is_some(), egui::Button::new("Use as clip"))
                .on_hover_text(match free_slot {
                    Some((_, slot)) => format!(
                        "Adds a Movie In node reading the selected image, in clip slot {slot} of the deck's top layer (one undo step)"
                    ),
                    None => "Adds a Movie In node reading the selected image (no free clip slot on the deck)".into(),
                });
            if use_clip.clicked()
                && let Some(e) = selected
            {
                let clip = BinClip {
                    path: e.image(),
                    hash: e.hash.clone(),
                };
                let id = engine.alloc_id();
                action = Some((
                    "Use media bin clip",
                    clip_edits(engine.network(), id, &clip, free_slot),
                ));
            }
        });
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        let Some(loader) = &self.loader else {
            return action;
        };
        let shown: Vec<&BinEntry> = self
            .bin
            .entries()
            .iter()
            .filter(|e| self.wedge.as_ref().is_none_or(|w| &e.wedge == w))
            .filter(|e| matches(e, &self.filter))
            .collect();
        if shown.is_empty() {
            ui.weak(if self.bin.is_empty() {
                "No results yet: run a wedge with `atidarla batch run <project> <wedge>` (docs/batch.md)."
            } else {
                "Nothing matches the filter."
            });
            return action;
        }
        let size = egui::vec2(THUMB as f32, THUMB as f32);
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for e in shown {
                    let visible =
                        ui.is_rect_visible(egui::Rect::from_min_size(ui.cursor().min, size));
                    if visible && !self.thumbs.contains_key(&e.hash) {
                        let path = loader.dir.join(e.image());
                        if loader
                            .tx
                            .send(ToLoader::Thumb(e.hash.clone(), path))
                            .is_ok()
                        {
                            self.thumbs.insert(e.hash.clone(), Thumb::Pending);
                        }
                    }
                    let selected = self.selected.as_deref() == Some(e.hash.as_str());
                    let payload = BinClip {
                        path: e.image(),
                        hash: e.hash.clone(),
                    };
                    let r = ui
                        .dnd_drag_source(
                            egui::Id::unique(("bin", &e.wedge, &e.hash)),
                            payload,
                            |ui| {
                                let (rect, r) = ui.allocate_exact_size(size, egui::Sense::click());
                                match self.thumbs.get(&e.hash) {
                                    Some(Thumb::Ready(tex)) => {
                                        let fit = tex.size_vec2()
                                            * (THUMB as f32 / tex.size_vec2().max_elem());
                                        let img_rect =
                                            egui::Rect::from_center_size(rect.center(), fit);
                                        ui.painter().image(
                                            tex.id(),
                                            img_rect,
                                            egui::Rect::from_min_max(
                                                egui::pos2(0.0, 0.0),
                                                egui::pos2(1.0, 1.0),
                                            ),
                                            Color32::WHITE,
                                        );
                                    }
                                    Some(Thumb::Failed(why)) => {
                                        ui.painter().rect_filled(
                                            rect,
                                            4.0,
                                            ui.visuals().extreme_bg_color,
                                        );
                                        ui.put(rect, egui::Label::new("missing"))
                                            .on_hover_text(why.as_str());
                                    }
                                    _ => {
                                        ui.painter().rect_filled(
                                            rect,
                                            4.0,
                                            ui.visuals().faint_bg_color,
                                        );
                                    }
                                }
                                if selected {
                                    ui.painter().rect_stroke(
                                        rect,
                                        4.0,
                                        egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
                                        egui::StrokeKind::Outside,
                                    );
                                }
                                r
                            },
                        )
                        .response;
                    let r = r.on_hover_text(describe(e));
                    if r.clicked() {
                        self.selected = Some(e.hash.clone());
                    }
                }
            });
        });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn entry(attrs: &[(&str, serde_json::Value)]) -> BinEntry {
        BinEntry {
            hash: "ab".repeat(32),
            wedge: "seeds".into(),
            index: 3,
            host: "lan:a".into(),
            size: [64, 32],
            created: 0,
            cook_ms: 12.0,
            attributes: attrs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn filters_match_attribute_names_and_values() {
        let e = entry(&[
            ("prompt", json!("A Red Fox")),
            ("diffusion1.seed", json!(7)),
        ]);
        assert!(matches(&e, ""));
        assert!(matches(&e, "red fox"));
        assert!(matches(&e, "prompt=a red"));
        assert!(matches(&e, "seed=7"));
        assert!(!matches(&e, "seed=8"));
        let d = describe(&e);
        assert!(
            d.contains("prompt = A Red Fox") && d.contains("lan:a"),
            "{d}"
        );
    }

    #[test]
    fn use_as_clip_adds_a_movie_in_wired_into_the_slot() {
        let reg = {
            let mut r = atd_core::Registry::default();
            atd_ops_img::register(&mut r);
            r
        };
        let mut net = Network::default();
        let layer = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(layer, "img.layer", "layer1"),
            },
        )
        .unwrap();
        let clip = BinClip {
            path: format!("media/bin/{}.png", "cd".repeat(32)),
            hash: "cd".repeat(32),
        };
        let id = net.alloc_id();
        let edits = clip_edits(&net, id, &clip, Some((layer, 2)));
        for e in edits {
            net.apply(&reg, e).unwrap();
        }
        let movie = net.nodes().find(|n| n.type_id == "img.movie_in").unwrap();
        assert_eq!(movie.name, "bin_cdcdcdcd_1");
        assert_eq!(
            net.param_value(&reg, movie.id, "file"),
            Some(&json!(clip.path))
        );
        let wired: Vec<_> = net
            .inputs_of(layer)
            .map(|(s, f)| (s.to_owned(), f.node))
            .collect();
        assert_eq!(wired, [("clip2".to_owned(), movie.id)]);
        // Without a slot only the node is added.
        let id = net.alloc_id();
        assert_eq!(clip_edits(&net, id, &clip, None).len(), 2);
    }
}
