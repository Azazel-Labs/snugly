//! The README / social-preview hero: a few dozen irregular shapes (blobs, stars, rings with
//! shapes nested inside, rectangle unions) packed by [`snugly::pack`], drawn as SVG.

use snugly::glam::Vec2;
use snugly::{ChartMask, HoleFill, PackOptions, pack, rotate_point};
use std::f32::consts::TAU;
use std::fmt::Write;

/// Pixels per atlas unit.
const SCALE: f32 = 3.0;
/// Units of margin inside each shape's frame. Shapes are packed without padding; the
/// conservative rasterization alone leaves a gap of about a unit between neighbours.
const PAD: f32 = 1.0;

const BG: &str = "#0e1320";
const PANEL: &str = "#151c2c";
const GRID: &str = "#1c2538";
const INK: &str = "#f3f1ea";
const MUTED: &str = "#8b93a7";
const PALETTE: [&str; 7] = [
    "#ff7a59", "#ffc145", "#2ec4b6", "#4ea8de", "#9b7bea", "#7bd88f", "#f25f9a",
];

/// A shape: polygons in its own frame (nonzero fill, holes wound the other way) and the
/// triangles snugly packs it from.
struct Piece {
    polys: Vec<Vec<Vec2>>,
    tris: Vec<[Vec2; 3]>,
    w: u32,
    h: u32,
    color: &'static str,
}

/// Small deterministic RNG (xorshift), so the image is the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[((self.next() * v.len() as f32) as usize).min(v.len() - 1)]
    }
}

fn polar(r: impl Fn(f32) -> f32, n: usize) -> Vec<Vec2> {
    (0..n)
        .map(|i| {
            let t = i as f32 / n as f32 * TAU;
            Vec2::new(t.cos(), t.sin()) * r(t)
        })
        .collect()
}

/// Fan triangles of a polygon that is star-shaped around the origin.
fn fan(poly: &[Vec2]) -> Vec<[Vec2; 3]> {
    (0..poly.len())
        .map(|i| [Vec2::ZERO, poly[i], poly[(i + 1) % poly.len()]])
        .collect()
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Vec<Vec2> {
    vec![
        Vec2::new(x, y),
        Vec2::new(x + w, y),
        Vec2::new(x + w, y + h),
        Vec2::new(x, y + h),
    ]
}

fn rect_tris(r: &[Vec2]) -> Vec<[Vec2; 3]> {
    vec![[r[0], r[1], r[2]], [r[0], r[2], r[3]]]
}

/// Moves the geometry so its bounding box starts at (PAD, PAD) and sizes the frame to fit.
fn piece(polys: Vec<Vec<Vec2>>, tris: Vec<[Vec2; 3]>, color: &'static str) -> Piece {
    let pts = polys.iter().flatten();
    let lo = pts.clone().fold(Vec2::splat(f32::MAX), |a, &p| a.min(p));
    let hi = pts.fold(Vec2::splat(f32::MIN), |a, &p| a.max(p));
    let d = Vec2::splat(PAD) - lo;
    let size = hi - lo + Vec2::splat(2.0 * PAD);
    Piece {
        polys: polys
            .into_iter()
            .map(|p| p.into_iter().map(|q| q + d).collect())
            .collect(),
        tris: tris.into_iter().map(|t| t.map(|q| q + d)).collect(),
        w: size.x.ceil() as u32,
        h: size.y.ceil() as u32,
        color,
    }
}

fn make_piece(rng: &mut Rng, kind: u32) -> Piece {
    let color = rng.pick(&PALETTE);
    match kind {
        // Wobbly blob.
        0 => {
            let r = rng.range(7.0, 17.0);
            let (a, b, p, q) = (
                rng.range(0.05, 0.2),
                rng.range(0.03, 0.15),
                rng.range(0.0, TAU),
                rng.range(0.0, TAU),
            );
            let poly = polar(|t| r * (1.0 + a * (2.0 * t + p).sin() + b * (3.0 * t + q).sin()), 64);
            let tris = fan(&poly);
            piece(vec![poly], tris, color)
        }
        // Star.
        1 => {
            let k = rng.pick(&[4.0, 5.0, 6.0]);
            let r = rng.range(9.0, 17.0);
            let inner = rng.range(0.45, 0.6);
            let poly = polar(
                |t| {
                    if ((t / TAU * 2.0 * k).round() as i32) % 2 == 0 {
                        r
                    } else {
                        r * inner
                    }
                },
                (2.0 * k) as usize,
            );
            let tris = fan(&poly);
            piece(vec![poly], tris, color)
        }
        // Ring: room for other shapes inside.
        2 => {
            let r = rng.range(16.0, 26.0);
            let ri = r * rng.range(0.55, 0.68);
            let outer = polar(|_| r, 72);
            let inner = polar(|_| ri, 72);
            let n = outer.len();
            let tris = (0..n)
                .flat_map(|i| {
                    let j = (i + 1) % n;
                    [[outer[i], outer[j], inner[j]], [outer[i], inner[j], inner[i]]]
                })
                .collect();
            piece(vec![outer, inner.into_iter().rev().collect()], tris, color)
        }
        // Rectangle union on a coarse grid: L, T, S, U and friends.
        3 => {
            let c = rng.range(5.0, 9.0).round();
            let shapes: [&[(u8, u8)]; 6] = [
                &[(0, 0), (1, 0), (2, 0), (0, 1), (0, 2)],
                &[(0, 0), (1, 0), (2, 0), (1, 1)],
                &[(0, 0), (1, 0), (1, 1), (2, 1)],
                &[(0, 0), (1, 0), (2, 0), (0, 1), (2, 1)],
                &[(0, 0), (1, 0), (2, 0), (3, 0), (3, 1)],
                &[(0, 0), (1, 0), (1, 1), (1, 2), (2, 2)],
            ];
            let cells = rng.pick(&shapes);
            let polys: Vec<Vec<Vec2>> = cells
                .iter()
                .map(|&(x, y)| rect(f32::from(x) * c, f32::from(y) * c, c, c))
                .collect();
            let tris = polys.iter().flat_map(|r| rect_tris(r)).collect();
            piece(polys, tris, color)
        }
        // Rounded rectangle.
        4 => {
            let (w, h) = (rng.range(10.0, 34.0), rng.range(6.0, 16.0));
            let r = w.min(h) * 0.3;
            let mut poly = Vec::new();
            for (cx, cy, a0) in [
                (w / 2.0 - r, h / 2.0 - r, 0.0),
                (r - w / 2.0, h / 2.0 - r, 0.25),
                (r - w / 2.0, r - h / 2.0, 0.5),
                (w / 2.0 - r, r - h / 2.0, 0.75),
            ] {
                for i in 0..=6 {
                    let t = (a0 + i as f32 / 24.0) * TAU;
                    poly.push(Vec2::new(cx + r * t.cos(), cy + r * t.sin()));
                }
            }
            let tris = fan(&poly);
            piece(vec![poly], tris, color)
        }
        // Half disc or regular polygon, like a UV chart.
        _ => {
            let r = rng.range(7.0, 15.0);
            let poly = if rng.next() < 0.5 {
                let mut p = polar(|_| r, 40)[..=20].to_vec();
                p.push(Vec2::ZERO);
                p.iter().map(|&q| q - Vec2::new(0.0, r * 0.4)).collect()
            } else {
                polar(|_| r, rng.pick(&[3usize, 5, 6]))
            };
            let tris = fan(&poly);
            piece(vec![poly], tris, color)
        }
    }
}

/// Gives each shape a palette colour that no shape touching its placed bounding box has, so
/// neighbours (and shapes nested in rings) always contrast.
fn recolor(pieces: &mut [Piece], places: &[snugly::MaskPlacement], rng: &mut Rng) {
    let boxes: Vec<(f32, f32, f32, f32)> = pieces
        .iter()
        .zip(places)
        .map(|(p, q)| {
            let (w, h) = if q.rotation % 2 == 1 { (p.h, p.w) } else { (p.w, p.h) };
            (q.x as f32, q.y as f32, (q.x + w) as f32, (q.y + h) as f32)
        })
        .collect();
    let near = |a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)| {
        a.0 < b.2 + 2.0 && b.0 < a.2 + 2.0 && a.1 < b.3 + 2.0 && b.1 < a.3 + 2.0
    };
    for i in 0..pieces.len() {
        let taken: Vec<&str> = (0..i)
            .filter(|&j| near(boxes[i], boxes[j]))
            .map(|j| pieces[j].color)
            .collect();
        let start = (rng.next() * PALETTE.len() as f32) as usize;
        let free = (0..PALETTE.len()).map(|k| PALETTE[(start + k) % PALETTE.len()]);
        // Fewest uses among the neighbours: none, when the palette allows.
        pieces[i].color = free
            .min_by_key(|c| taken.iter().filter(|t| *t == c).count())
            .unwrap_or(PALETTE[0]);
    }
}

/// The hero as an SVG document, `w × h` pixels.
pub fn svg(w: u32, h: u32) -> String {
    let (w, h) = (w as f32, h as f32);

    // Layout: wordmark on the left, the atlas panel on the right.
    let margin = (h * 0.09).round();
    let panel = (w * 0.47, margin, w - margin, h - margin);
    let (pw, ph) = (panel.2 - panel.0, panel.3 - panel.1);
    let units = (pw / SCALE) as u32;

    // Shapes until their area is comfortably more than the panel's: the atlas is cut off at the
    // top of the panel, where a bottom-left packing is raggedest.
    let mut rng = Rng(0x5eed_51ab_1e5e_ed00);
    let mut pieces = Vec::new();
    let mut area = 0.0;
    while area < 1.15 * pw * ph / (SCALE * SCALE) {
        let kind = rng.pick(&[0, 0, 1, 1, 2, 3, 3, 4, 4, 5, 5]);
        let p = make_piece(&mut rng, kind);
        area += (p.w * p.h) as f32 * if kind == 2 { 0.6 } else { 0.8 };
        pieces.push(p);
    }
    let masks: Vec<ChartMask> = pieces
        .iter()
        .map(|p| ChartMask::from_triangles(&p.tris, p.w, p.h, 0))
        .collect();
    let opts = PackOptions::width(units)
        .with_fill_holes(HoleFill::Always)
        .with_nest_in_holes(true);
    let packing = pack(&masks, &opts).expect("every shape is narrower than the atlas");
    recolor(&mut pieces, &packing.placements, &mut rng);

    let mut s = String::new();
    let font = "'Cascadia Code', 'Cascadia Mono', monospace";
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">
<rect width="{w}" height="{h}" fill="{BG}"/>
<defs><clipPath id="atlas"><rect x="{x}" y="{y}" width="{pw}" height="{ph}" rx="14"/></clipPath></defs>
<rect x="{x}" y="{y}" width="{pw}" height="{ph}" rx="14" fill="{PANEL}"/>
<g clip-path="url(#atlas)">
"#,
        x = panel.0,
        y = panel.1,
    );
    // Faint pixel grid, as in a texture atlas.
    let step = 8.0 * SCALE;
    let mut gx = panel.0 + step;
    while gx < panel.2 {
        let _ = writeln!(
            s,
            r#"<path d="M{gx} {} V{}" stroke="{GRID}" stroke-width="1"/>"#,
            panel.1, panel.3
        );
        gx += step;
    }
    let mut gy = panel.3 - step;
    while gy > panel.1 {
        let _ = writeln!(
            s,
            r#"<path d="M{} {gy} H{}" stroke="{GRID}" stroke-width="1"/>"#,
            panel.0, panel.2
        );
        gy -= step;
    }
    for (p, place) in pieces.iter().zip(&packing.placements) {
        let mut d = String::new();
        for poly in &p.polys {
            for (i, &q) in poly.iter().enumerate() {
                let a = rotate_point(q, p.w, p.h, place.rotation) + Vec2::new(place.x as f32, place.y as f32);
                let (px, py) = (panel.0 + a.x * SCALE, panel.3 - a.y * SCALE);
                let _ = write!(d, "{}{px:.1} {py:.1}", if i == 0 { "M" } else { "L" });
            }
            d.push('Z');
        }
        let _ = writeln!(s, r#"<path d="{d}" fill="{}"/>"#, p.color);
    }
    s.push_str("</g>\n");

    // Wordmark.
    let tx = margin * 1.25;
    // Monospace glyphs are about 0.6 em wide: size each line to the column left of the panel.
    let avail = panel.0 - tx - margin * 0.75;
    let big = (h * 0.2).min(avail / (6.0 * 0.6));
    let (f2, f3) = (
        (big * 0.215).min(avail / (34.0 * 0.6)),
        (big * 0.17).min(avail / (39.0 * 0.6)),
    );
    let cy = h * 0.5;
    let _ = write!(
        s,
        r#"<text x="{tx}" y="{y1}" font-family="{font}" font-weight="700" font-size="{big}" fill="{INK}" letter-spacing="-2">snugly</text>
<text x="{tx}" y="{y2}" font-family="{font}" font-size="{f2}" fill="{INK}">Fits your shapes together, snugly.</text>
<text x="{tx}" y="{y3}" font-family="{font}" font-size="{f3}" fill="{MUTED}">Deterministic 2D atlas packing in Rust</text>
<text x="{tx}" y="{y4}" font-family="{font}" font-size="{f3}" fill="{MUTED}">rectangles · shapes · holes · rotations</text>
<rect x="{tx}" y="{bar_y}" width="{bar_w}" height="{bar_h}" rx="{bar_r}" fill="{accent}"/>
</svg>
"#,
        y1 = cy - big * 0.12,
        y2 = cy + big * 0.12 + f2 * 1.5,
        y3 = cy + big * 0.12 + f2 * 1.5 + f3 * 1.9,
        y4 = cy + big * 0.12 + f2 * 1.5 + f3 * 3.3,
        bar_y = cy - big * 0.98,
        bar_w = big * 0.9,
        bar_h = big * 0.07,
        bar_r = big * 0.035,
        accent = PALETTE[0],
    );
    s
}
