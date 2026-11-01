//! How wire types look: colours that stay apart for colour-blind users, and
//! a second cue that is not colour (socket shapes, optional type labels).
//!
//! The colours are six of the Okabe–Ito palette (Okabe & Ito, "Color
//! Universal Design", 2008), chosen as the six that stay furthest apart
//! under simulated protanopia, deuteranopia and tritanopia; the test below
//! checks that with the Machado, Oliveira & Fernandes (2009) simulation.
//! Each type also has its own socket shape, so a wire's type reads without
//! colour at either end. See docs/accessibility.md.

use egui::{Color32, Pos2, Shape, Stroke, Vec2};
use lsr_core::registry::WireType;

/// Every wire type, in the order the legend lists them.
pub const ALL: [WireType; 6] = [
    WireType::Img,
    WireType::Sig,
    WireType::Geo,
    WireType::Dat,
    WireType::Mat,
    WireType::Cmp,
];

pub fn colour(w: WireType) -> Color32 {
    match w {
        // Sky blue, yellow, blue, orange, vermillion, grey.
        WireType::Img => Color32::from_rgb(86, 180, 233),
        WireType::Sig => Color32::from_rgb(240, 228, 66),
        WireType::Geo => Color32::from_rgb(0, 114, 178),
        WireType::Dat => Color32::from_rgb(230, 159, 0),
        WireType::Mat => Color32::from_rgb(213, 94, 0),
        WireType::Cmp => Color32::from_rgb(187, 187, 187),
    }
}

/// Text colour for a label on `colour(w)`: black, or white on the blue,
/// whichever contrasts more.
pub fn text_on(w: WireType) -> Color32 {
    let c = colour(w);
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let l = 0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b());
    // Contrast with black is (l + 0.05) / 0.05, with white 1.05 / (l + 0.05).
    if (l + 0.05) * (l + 0.05) > 0.0525 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

/// The short label drawn on wires and named in tooltips.
pub fn tag(w: WireType) -> &'static str {
    match w {
        WireType::Img => "IMG",
        WireType::Sig => "SIG",
        WireType::Geo => "GEO",
        WireType::Dat => "DAT",
        WireType::Mat => "MAT",
        WireType::Cmp => "CMP",
    }
}

/// The socket shape's name, for the legend and tooltips.
pub fn shape_name(w: WireType) -> &'static str {
    match w {
        WireType::Img => "circle",
        WireType::Sig => "square",
        WireType::Geo => "triangle",
        WireType::Dat => "diamond",
        WireType::Mat => "hexagon",
        WireType::Cmp => "ring",
    }
}

/// A socket of type `w` at `c` with radius `r`.
pub fn socket(c: Pos2, r: f32, w: WireType) -> Shape {
    let fill = colour(w);
    let polygon = |sides: usize, start: f32, scale: f32| {
        let points = (0..sides)
            .map(|i| {
                let a = start + i as f32 * std::f32::consts::TAU / sides as f32;
                c + Vec2::angled(a) * r * scale
            })
            .collect();
        Shape::convex_polygon(points, fill, Stroke::NONE)
    };
    match w {
        WireType::Img => Shape::circle_filled(c, r, fill),
        WireType::Sig => Shape::rect_filled(
            egui::Rect::from_center_size(c, Vec2::splat(r * 1.7)),
            0.0,
            fill,
        ),
        // Pointing right, the way data flows.
        WireType::Geo => polygon(3, 0.0, 1.25),
        WireType::Dat => polygon(4, 0.0, 1.2),
        WireType::Mat => polygon(6, 0.0, 1.1),
        WireType::Cmp => Shape::circle_stroke(c, r * 0.8, Stroke::new(r * 0.5, fill)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Machado et al. (2009) simulation matrices at full severity, for
    /// linear RGB.
    const VIEWS: [(&str, [[f64; 3]; 3]); 4] = [
        (
            "normal",
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        ),
        (
            "protanopia",
            [
                [0.152286, 1.052583, -0.204868],
                [0.114503, 0.786281, 0.099216],
                [-0.003882, -0.048116, 1.051998],
            ],
        ),
        (
            "deuteranopia",
            [
                [0.367322, 0.860646, -0.227968],
                [0.280085, 0.672501, 0.047413],
                [-0.011820, 0.042940, 0.968881],
            ],
        ),
        (
            "tritanopia",
            [
                [1.255528, -0.076749, -0.178779],
                [-0.078411, 0.930809, 0.147602],
                [0.004733, 0.691367, 0.303900],
            ],
        ),
    ];

    fn linear(c: u8) -> f64 {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    fn rgb(c: Color32) -> [f64; 3] {
        [linear(c.r()), linear(c.g()), linear(c.b())]
    }

    /// CIELAB (D65) of linear RGB.
    fn lab([r, g, b]: [f64; 3]) -> [f64; 3] {
        let x = 0.4124 * r + 0.3576 * g + 0.1805 * b;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let z = 0.0193 * r + 0.1192 * g + 0.9505 * b;
        let f = |t: f64| {
            if t > 216.0 / 24389.0 {
                t.cbrt()
            } else {
                (24389.0 / 27.0 * t + 16.0) / 116.0
            }
        };
        let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    }

    fn simulate(m: &[[f64; 3]; 3], c: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|i| (0..3).map(|j| m[i][j] * c[j]).sum::<f64>().clamp(0.0, 1.0))
    }

    fn luminance([r, g, b]: [f64; 3]) -> f64 {
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    /// Every pair of wire colours stays at least 15 ΔE*ab apart (clearly
    /// different side by side) with normal vision and under each full
    /// dichromacy (the closest pair is 17, yellow and orange under
    /// deuteranopia). The previous palette had IMG and GEO 6 apart under
    /// deuteranopia.
    #[test]
    fn wire_colours_stay_apart_under_colour_blindness() {
        for (view, m) in &VIEWS {
            let labs: Vec<[f64; 3]> = ALL
                .iter()
                .map(|w| lab(simulate(m, rgb(colour(*w)))))
                .collect();
            for (i, a) in labs.iter().enumerate() {
                for (j, b) in labs.iter().enumerate().skip(i + 1) {
                    let d = (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f64>().sqrt();
                    assert!(
                        d >= 15.0,
                        "{view}: {:?} and {:?} are only {d:.1} apart",
                        ALL[i],
                        ALL[j]
                    );
                }
            }
        }
    }

    /// Wires and sockets reach 3:1 contrast (WCAG 2 non-text contrast)
    /// against the dark canvas.
    #[test]
    fn wire_colours_contrast_with_the_canvas() {
        let bg = luminance(rgb(egui::Visuals::dark().panel_fill));
        for w in ALL {
            let l = luminance(rgb(colour(w)));
            let ratio = (l + 0.05) / (bg + 0.05);
            assert!(ratio >= 3.0, "{w:?}: {ratio:.2}:1");
        }
    }

    /// Wire labels reach 4.5:1 (WCAG AA for text) on their wire colour.
    #[test]
    fn wire_labels_are_readable() {
        for w in ALL {
            let (a, b) = (luminance(rgb(colour(w))), luminance(rgb(text_on(w))));
            let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
            assert!(ratio >= 4.5, "{w:?}: {ratio:.2}:1");
        }
    }

    #[test]
    fn every_type_has_its_own_tag_and_shape() {
        let tags: std::collections::BTreeSet<_> = ALL.iter().map(|w| tag(*w)).collect();
        let shapes: std::collections::BTreeSet<_> = ALL.iter().map(|w| shape_name(*w)).collect();
        assert_eq!(tags.len(), ALL.len());
        assert_eq!(shapes.len(), ALL.len());
        for w in ALL {
            assert!(!matches!(socket(Pos2::ZERO, 5.0, w), Shape::Noop));
        }
    }
}
