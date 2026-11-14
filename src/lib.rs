//! # snugly
//!
//! Deterministic 2D atlas packing, independent of meshes and UVs:
//!
//! * [`pack_rects`] — skyline bottom-left packing of rectangles with padding and optional 90°
//!   rotation (aggregation atlases, impostor frames, sprite sheets).
//! * [`pack`] — irregular shapes ([`Shape`], given as per-column and per-row outlines, from
//!   triangles, a raster mask or a rectangle) with padding and quarter-turn rotations. A
//!   skyline packer that lets shapes interlock, and for few shapes also a hole-filling packer
//!   that nests shapes under each other's overhangs; the lower atlas wins.
//!
//! Output depends only on the input, never on thread count. Placement is bottom-left search
//! over outlines and free intervals: no FFT or cross-correlation placement.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod maxrects;
mod par;

/// The README's examples, compiled and run as doctests.
#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;

/// The `glam` version this crate's API uses ([`ChartMask::from_triangles`], [`rotate_point`]).
pub use glam;

/// Where [`pack_rects`] / [`pack_rects_strip`] put a rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Placement {
    /// Left edge of the rectangle (padding excluded).
    pub x: u32,
    /// Bottom edge of the rectangle (padding excluded; y grows upwards from the atlas's first
    /// row).
    pub y: u32,
    /// Width as placed (after rotation, without padding).
    pub w: u32,
    /// Height as placed (after rotation, without padding).
    pub h: u32,
    /// Turned by a quarter turn (`w` and `h` are swapped relative to the input size).
    pub rotated: bool,
}

#[derive(Clone, Copy)]
struct Seg {
    x: u32,
    w: u32,
    y: u32,
}

/// Packs `sizes` (w, h) into a fixed `width × height` atlas with `padding` pixels around each
/// rect. Returns placements in input order, or None if they don't fit.
///
/// ```
/// let p = snugly::pack_rects(&[(100, 50), (50, 100)], 128, 128, 1, true).unwrap();
/// assert!(p.iter().all(|r| r.x + r.w <= 128 && r.y + r.h <= 128));
/// assert!(snugly::pack_rects(&[(200, 10)], 128, 128, 0, false).is_none());
/// ```
///
/// Tries a skyline packer and a set of `MaxRects` variants (in parallel) and keeps the packing
/// that uses the least height; ties go to the earliest in a fixed order, so the result only
/// depends on the input.
#[must_use]
pub fn pack_rects(
    sizes: &[(u32, u32)],
    width: u32,
    height: u32,
    padding: u32,
    allow_rotate: bool,
) -> Option<Vec<Placement>> {
    let padded = padded(sizes, padding);
    let (sky, mr) = par::join(
        || skyline_rects(sizes, width, height, padding, allow_rotate),
        || maxrects::best_fixed(&padded, width, height, allow_rotate),
    );
    let mr = mr.map(|(p, top)| (unpad(&p, sizes, padding), top));
    match (sky, mr) {
        (Some(a), Some(b)) => Some(if b.1 < used_height(&a, padding) { b.0 } else { a }),
        (a, b) => a.or(b.map(|b| b.0)),
    }
}

/// Why a packing failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PackError {
    /// Item `index` is wider than the atlas in every allowed orientation (padding included).
    TooWide {
        /// Index of the item in the input.
        index: usize,
        /// The atlas width.
        width: u32,
    },
    /// The packing needs `height` rows, more than `max_height`.
    TooTall {
        /// Height the packing needs.
        height: u32,
        /// The limit it exceeds ([`PackOptions::max_height`]).
        max_height: u32,
    },
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooWide { index, width } => {
                write!(
                    f,
                    "item {index} is wider than the {width}px atlas in every allowed orientation"
                )
            }
            Self::TooTall { height, max_height } => {
                write!(f, "the packing needs {height}px, more than the {max_height}px limit")
            }
        }
    }
}

impl std::error::Error for PackError {}

/// Packs `sizes` into a strip `width` wide, as low as it can. Returns placements in input
/// order and the height used (padding included). Binary search on the height over the
/// [`pack_rects`] packers.
///
/// ```
/// let sizes = [(120, 40), (64, 64), (30, 200)];
/// let (placements, height) = snugly::pack_rects_strip(&sizes, 256, 2, true).unwrap();
/// assert_eq!(placements.len(), 3);
/// assert!(height >= 64);
/// ```
///
/// # Errors
///
/// [`PackError::TooWide`] if a rectangle (padding included) is wider than `width` in every
/// allowed orientation.
#[expect(
    clippy::missing_panics_doc,
    reason = "widths are checked above, so the skyline always fits"
)]
pub fn pack_rects_strip(
    sizes: &[(u32, u32)],
    width: u32,
    padding: u32,
    allow_rotate: bool,
) -> Result<(Vec<Placement>, u32), PackError> {
    let padded = padded(sizes, padding);
    let mut lo = 0u32;
    let mut area = 0u64;
    for (index, &(w, h)) in padded.iter().enumerate() {
        let need = match (w <= width, h <= width && allow_rotate) {
            (true, true) => h.min(w),
            (true, false) => h,
            (false, true) => w,
            (false, false) => return Err(PackError::TooWide { index, width }),
        };
        lo = lo.max(need);
        area += u64::from(w) * u64::from(h);
    }
    lo = lo.max(area.div_ceil(u64::from(width.max(1))) as u32);
    let sky = skyline_rects(sizes, width, u32::MAX / 2, padding, allow_rotate)
        .expect("every rectangle fits the strip's width");
    let hi = used_height(&sky, padding);
    Ok(match maxrects::best_strip(&padded, width, lo, hi, allow_rotate) {
        Some((p, top)) if top < hi => (unpad(&p, sizes, padding), top),
        _ => (sky, hi),
    })
}

fn padded(sizes: &[(u32, u32)], padding: u32) -> Vec<(u32, u32)> {
    sizes.iter().map(|&(w, h)| (w + 2 * padding, h + 2 * padding)).collect()
}

fn unpad(p: &[(u32, u32, bool)], sizes: &[(u32, u32)], padding: u32) -> Vec<Placement> {
    p.iter()
        .zip(sizes)
        .map(|(&(x, y, r), &(w, h))| Placement {
            x: x + padding,
            y: y + padding,
            w: if r { h } else { w },
            h: if r { w } else { h },
            rotated: r,
        })
        .collect()
}

fn used_height(p: &[Placement], padding: u32) -> u32 {
    p.iter().map(|q| q.y + q.h + padding).max().unwrap_or(0)
}

fn skyline_rects(
    sizes: &[(u32, u32)],
    width: u32,
    height: u32,
    padding: u32,
    allow_rotate: bool,
) -> Option<Vec<Placement>> {
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse((sizes[i].0.max(sizes[i].1), sizes[i].0 * sizes[i].1)));
    let mut sky = vec![Seg { x: 0, w: width, y: 0 }];
    let mut out = vec![
        Placement {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            rotated: false
        };
        sizes.len()
    ];
    for i in order {
        let (w0, h0) = sizes[i];
        let mut best: Option<(u32, u32, usize, bool, u32, u32)> = None; // (top, x, seg, rot, pw, ph)
        let candidates: &[(u32, u32, bool)] = &if allow_rotate && w0 != h0 {
            vec![(w0, h0, false), (h0, w0, true)]
        } else {
            vec![(w0, h0, false)]
        };
        for &(w, h, rot) in candidates {
            let (pw, ph) = (w + 2 * padding, h + 2 * padding);
            if pw > width || ph > height {
                continue;
            }
            for s in 0..sky.len() {
                if sky[s].x + pw > width {
                    break;
                }
                // Height needed: max y over the segments spanned.
                let mut y = 0;
                let mut span = 0;
                let mut j = s;
                while span < pw && j < sky.len() {
                    y = y.max(sky[j].y);
                    span += sky[j].w;
                    j += 1;
                }
                if span < pw || y + ph > height {
                    continue;
                }
                let top = y + ph;
                if best.is_none_or(|b| (top, sky[s].x) < (b.0, b.1)) {
                    best = Some((top, sky[s].x, s, rot, pw, ph));
                }
            }
        }
        let (top, x, _s, rot, pw, ph) = best?;
        let y = top - ph;
        out[i] = Placement {
            x: x + padding,
            y: y + padding,
            w: pw - 2 * padding,
            h: ph - 2 * padding,
            rotated: rot,
        };
        // Update skyline: insert new segment [x, x+pw) at height top, trim overlapped ones.
        let mut next = Vec::with_capacity(sky.len() + 2);
        for seg in &sky {
            let (s0, s1) = (seg.x, seg.x + seg.w);
            let (n0, n1) = (x, x + pw);
            if s1 <= n0 || s0 >= n1 {
                next.push(*seg);
                continue;
            }
            if s0 < n0 {
                next.push(Seg {
                    x: s0,
                    w: n0 - s0,
                    y: seg.y,
                });
            }
            if s1 > n1 {
                next.push(Seg {
                    x: n1,
                    w: s1 - n1,
                    y: seg.y,
                });
            }
        }
        next.push(Seg { x, w: pw, y: top });
        next.sort_by_key(|s| s.x);
        // Merge neighbours at equal height.
        let mut merged: Vec<Seg> = Vec::with_capacity(next.len());
        for s in next {
            match merged.last_mut() {
                Some(m) if m.y == s.y && m.x + m.w == s.x => m.w += s.w,
                _ => merged.push(s),
            }
        }
        sky = merged;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlaps(a: &Placement, b: &Placement, pad: u32) -> bool {
        let (ax0, ay0, ax1, ay1) = (a.x - pad, a.y - pad, a.x + a.w + pad, a.y + a.h + pad);
        let (bx0, by0, bx1, by1) = (b.x, b.y, b.x + b.w, b.y + b.h);
        ax0 < bx1 && bx0 < ax1 && ay0 < by1 && by0 < ay1
    }

    #[test]
    fn packs_without_overlap() {
        let mut seed = 0x2545_f491u32;
        let mut below = |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % n
        };
        let sizes: Vec<(u32, u32)> = (0..120).map(|_| (4 + below(60), 4 + below(60))).collect();
        let p = pack_rects(&sizes, 1024, 1024, 2, true).expect("fits");
        for i in 0..p.len() {
            assert!(p[i].x + p[i].w <= 1024 && p[i].y + p[i].h <= 1024);
            let (w, h) = sizes[i];
            assert!((p[i].w, p[i].h) == (w, h) || (p[i].w, p[i].h) == (h, w));
            for j in 0..i {
                assert!(!overlaps(&p[i], &p[j], 2), "{i} overlaps {j}");
            }
        }
    }

    #[test]
    fn reports_overflow() {
        assert!(pack_rects(&[(600, 600), (600, 600)], 1000, 1000, 0, false).is_none());
    }

    fn sizes(n: usize, seed: u32) -> Vec<(u32, u32)> {
        let mut s = seed;
        let mut below = |n: u32| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s % n
        };
        (0..n).map(|_| (4 + below(90), 4 + below(40))).collect()
    }

    #[test]
    fn strip_is_tight_and_valid() {
        for rot in [false, true] {
            let sz = sizes(300, 0x1234_5679);
            let (p, h) = pack_rects_strip(&sz, 512, 1, rot).unwrap();
            let area: u64 = sz.iter().map(|&(w, h)| u64::from(w + 2) * u64::from(h + 2)).sum();
            let util = area as f64 / (512.0 * f64::from(h));
            assert!(util > 0.9, "utilization {util}");
            for i in 0..p.len() {
                assert!(rot || !p[i].rotated);
                assert!(p[i].x + p[i].w < 512 && p[i].y + p[i].h < h + 1);
                for j in 0..i {
                    assert!(!overlaps(&p[i], &p[j], 1), "{i} overlaps {j}");
                }
            }
        }
    }

    /// FNV-1a over every placement of a fixed workload (rectangles, the strip search, and
    /// irregular shapes through both outline packers). The same constant must come out with
    /// and without the `parallel` feature, on any platform: CI runs it both ways.
    #[test]
    fn golden_output() {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut eat = |x: u32| {
            for b in x.to_le_bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x100_0000_01b3);
            }
        };
        let sz = sizes(160, 0x5eed_1234);
        for p in pack_rects(&sz, 700, 700, 1, true).unwrap() {
            [p.x, p.y, p.w, p.h, u32::from(p.rotated)]
                .into_iter()
                .for_each(&mut eat);
        }
        let (ps, top) = pack_rects_strip(&sz, 512, 1, true).unwrap();
        eat(top);
        for p in ps {
            [p.x, p.y, u32::from(p.rotated)].into_iter().for_each(&mut eat);
        }
        let shapes: Vec<ChartMask> = (0..40u32)
            .map(|i| {
                let (w, h) = (6 + i * 7 % 23, 5 + i * 11 % 19);
                let bits: Vec<bool> = (0..w * h).map(|k| (k % w) * 3 < w * 2 || (k / w) * 2 < h).collect();
                ChartMask::from_bits(w, h, &bits)
            })
            .collect();
        for fill in [HoleFill::Never, HoleFill::Always] {
            let p = pack(&shapes, &PackOptions::width(96).with_fill_holes(fill)).unwrap();
            eat(p.height);
            for q in p.placements {
                [q.x, q.y, q.rotation].into_iter().for_each(&mut eat);
            }
        }
        assert_eq!(h, GOLDEN, "packing output changed: {h:#018x}");
    }

    const GOLDEN: u64 = 0x76e6_acf7_393d_18a5;

    #[cfg(feature = "parallel")]
    #[test]
    fn strip_same_for_any_thread_count() {
        let sz = sizes(200, 0xdead_beef);
        let run = |t: usize| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(t)
                .build()
                .unwrap()
                .install(|| pack_rects_strip(&sz, 400, 0, true).unwrap())
        };
        let a = run(1);
        assert_eq!(a, run(4));
        assert_eq!(a, run(7));
    }

    #[test]
    fn pack_uses_rect_path_for_rectangles() {
        let sz = sizes(150, 0x0bad_f00d);
        let shapes: Vec<ChartMask> = sz.iter().map(|&(w, h)| ChartMask::rect(w, h)).collect();
        let p = pack(&shapes, &PackOptions::width(300)).unwrap();
        let (_, h) = pack_rects_strip(&sz, 300, 0, true).unwrap();
        assert_eq!(p.height, h);
    }
}

/// A chart footprint in its own pixel frame, padding included, kept as outlines: per column
/// and per row, the extent of the pixels a conservative rasterization would cover. The packer
/// only ever reads these outlines, so the pixel mask itself is never built: cost is per
/// triangle edge, not per covered pixel.
#[derive(Clone, Debug)]
pub struct ChartMask {
    /// Frame width in pixels (padding included).
    pub w: u32,
    /// Frame height in pixels (padding included).
    pub h: u32,
    /// Per column: (lowest covered row, one past the highest).
    col: Vec<Option<(u32, u32)>>,
    /// Per row: (leftmost covered column, one past the rightmost).
    row: Vec<Option<(u32, u32)>>,
    /// Pixels inside the column outlines (orders charts for packing).
    area: usize,
    /// Per column / row: uncovered intervals strictly inside its outline (holes in the shape,
    /// after padding). The hole-filling packer hands them back as free space once the shape is
    /// placed, so smaller shapes can go inside.
    col_gaps: Vec<Vec<(u32, u32)>>,
    row_gaps: Vec<Vec<(u32, u32)>>,
}

/// Rows `[lo, hi)` touched within column slab `[x, x + 1]` by triangle `t` (2D, any winding),
/// matching a closed pixel/triangle overlap test; None if the slab misses the triangle.
fn slab_extent(t: &[glam::Vec2; 3], x: f32) -> Option<(f32, f32)> {
    let (x0, x1) = (x, x + 1.0);
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    let mut add = |y: f32| {
        lo = lo.min(y);
        hi = hi.max(y);
    };
    for e in 0..3 {
        let (a, b) = (t[e], t[(e + 1) % 3]);
        if a.x >= x0 && a.x <= x1 {
            add(a.y);
        }
        #[expect(clippy::float_cmp, reason = "only exactly vertical edges would divide by zero")]
        if a.x != b.x {
            for xs in [x0, x1] {
                if (a.x - xs) * (b.x - xs) <= 0.0 {
                    let u = (xs - a.x) / (b.x - a.x);
                    add(a.y + (b.y - a.y) * u);
                }
            }
        }
    }
    (lo <= hi).then_some((lo, hi))
}

/// Per column of a `w × h` frame, the covered row extent of conservatively rasterized
/// triangles (a pixel is covered if it touches a triangle and lies in its bounding box).
/// With `swap`, x and y trade places (row outlines).
fn outlines(tris: &[[glam::Vec2; 3]], w: u32, h: u32, swap: bool) -> Vec<Option<(u32, u32)>> {
    let mut out: Vec<Option<(u32, u32)>> = vec![None; w as usize];
    for t in tris {
        let t = if swap { t.map(|p| glam::Vec2::new(p.y, p.x)) } else { *t };
        let mn = t[0].min(t[1]).min(t[2]);
        let mx = t[0].max(t[1]).max(t[2]);
        let (bx0, bx1) = (mn.x.floor().max(0.0) as u32, (mx.x.ceil().min(w as f32)) as u32);
        let (by0, by1) = (mn.y.floor().max(0.0) as i64, (mx.y.ceil().min(h as f32)) as i64);
        if by1 <= by0 {
            continue;
        }
        let degenerate = (t[1] - t[0]).perp_dot(t[2] - t[0]).abs() < 1e-12;
        for x in bx0..bx1 {
            let (y0, y1) = if degenerate {
                (by0, by1)
            } else {
                let Some((lo, hi)) = slab_extent(&t, x as f32) else {
                    continue;
                };
                // Row y touches [lo, hi] when y <= hi and y + 1 >= lo.
                ((lo.ceil() as i64 - 1).max(by0), (hi.floor() as i64 + 1).min(by1))
            };
            if y1 <= y0 {
                continue;
            }
            let (y0, y1) = (y0 as u32, y1 as u32);
            let c = &mut out[x as usize];
            *c = Some(c.map_or((y0, y1), |(a, b)| (a.min(y0), b.max(y1))));
        }
    }
    out
}

/// Outlines of a square dilation by `r`: column x of the dilated mask spans the union of the
/// columns within r, grown by r and clipped to `[0, extent)`.
fn dilate_outline(o: &[Option<(u32, u32)>], r: u32, extent: u32) -> Vec<Option<(u32, u32)>> {
    if r == 0 {
        return o.to_vec();
    }
    let n = o.len();
    let r = r as usize;
    (0..n)
        .map(|x| {
            let mut acc: Option<(u32, u32)> = None;
            for c in o[x.saturating_sub(r)..(x + r + 1).min(n)].iter().flatten() {
                acc = Some(acc.map_or(*c, |(a, b)| (a.min(c.0), b.max(c.1))));
            }
            acc.map(|(a, b)| (a.saturating_sub(r as u32), (b + r as u32).min(extent)))
        })
        .collect()
}

/// Per column of a `w × h` frame, the covered row intervals of conservatively rasterized
/// triangles, merged (like [`outlines`], but keeping holes). With `swap`, rows.
fn spans(tris: &[[glam::Vec2; 3]], w: u32, h: u32, swap: bool) -> Vec<Vec<(u32, u32)>> {
    let mut out: Vec<Vec<(u32, u32)>> = vec![Vec::new(); w as usize];
    for t in tris {
        let t = if swap { t.map(|p| glam::Vec2::new(p.y, p.x)) } else { *t };
        let mn = t[0].min(t[1]).min(t[2]);
        let mx = t[0].max(t[1]).max(t[2]);
        let (bx0, bx1) = (mn.x.floor().max(0.0) as u32, (mx.x.ceil().min(w as f32)) as u32);
        let (by0, by1) = (mn.y.floor().max(0.0) as i64, (mx.y.ceil().min(h as f32)) as i64);
        if by1 <= by0 {
            continue;
        }
        let degenerate = (t[1] - t[0]).perp_dot(t[2] - t[0]).abs() < 1e-12;
        for x in bx0..bx1 {
            let (y0, y1) = if degenerate {
                (by0, by1)
            } else {
                let Some((lo, hi)) = slab_extent(&t, x as f32) else {
                    continue;
                };
                ((lo.ceil() as i64 - 1).max(by0), (hi.floor() as i64 + 1).min(by1))
            };
            if y1 > y0 {
                out[x as usize].push((y0 as u32, y1 as u32));
            }
        }
    }
    for c in &mut out {
        merge(c);
    }
    out
}

/// Sorts and merges overlapping or touching intervals.
fn merge(v: &mut Vec<(u32, u32)>) {
    v.sort_unstable();
    let mut w: Vec<(u32, u32)> = Vec::with_capacity(v.len());
    for &(a, b) in v.iter() {
        match w.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => w.push((a, b)),
        }
    }
    *v = w;
}

/// Square dilation by `r` of per-column interval sets.
fn dilate_spans(cols: &[Vec<(u32, u32)>], r: u32, extent: u32) -> Vec<Vec<(u32, u32)>> {
    if r == 0 {
        return cols.to_vec();
    }
    let n = cols.len();
    let r = r as usize;
    (0..n)
        .map(|x| {
            let mut v: Vec<(u32, u32)> = cols[x.saturating_sub(r)..(x + r + 1).min(n)]
                .iter()
                .flatten()
                .map(|&(a, b)| (a.saturating_sub(r as u32), (b + r as u32).min(extent)))
                .collect();
            merge(&mut v);
            v
        })
        .collect()
}

/// The uncovered intervals between consecutive covered ones of each column.
fn gaps(cols: &[Vec<(u32, u32)>]) -> Vec<Vec<(u32, u32)>> {
    cols.iter()
        .map(|c| c.windows(2).map(|w| (w[0].1, w[1].0)).collect())
        .collect()
}

impl ChartMask {
    /// Conservatively rasterizes 2D triangles (already in pixel units, frame origin at 0,0)
    /// and dilates the result by `padding` pixels, as outlines.
    #[must_use]
    pub fn from_triangles(tris: &[[glam::Vec2; 3]], w: u32, h: u32, padding: u32) -> Self {
        let col = dilate_outline(&outlines(tris, w, h, false), padding, h);
        let row = dilate_outline(&outlines(tris, h, w, true), padding, w);
        let area = col.iter().flatten().map(|(a, b)| (b - a) as usize).sum();
        let col_gaps = gaps(&dilate_spans(&spans(tris, w, h, false), padding, h));
        let row_gaps = gaps(&dilate_spans(&spans(tris, h, w, true), padding, w));
        Self {
            w,
            h,
            col,
            row,
            area,
            col_gaps,
            row_gaps,
        }
    }

    /// From an explicit pixel mask (tests).
    /// From a row-major raster mask (`w × h`, true = covered). Padding is not added.
    #[must_use]
    pub fn from_bits(w: u32, h: u32, bits: &[bool]) -> Self {
        let mut col: Vec<Option<(u32, u32)>> = vec![None; w as usize];
        let mut row: Vec<Option<(u32, u32)>> = vec![None; h as usize];
        for y in 0..h {
            for x in 0..w {
                if bits[(y * w + x) as usize] {
                    let c = &mut col[x as usize];
                    *c = Some(c.map_or((y, y + 1), |(lo, hi)| (lo.min(y), hi.max(y + 1))));
                    let r = &mut row[y as usize];
                    *r = Some(r.map_or((x, x + 1), |(lo, hi)| (lo.min(x), hi.max(x + 1))));
                }
            }
        }
        let area = col.iter().flatten().map(|(a, b)| (b - a) as usize).sum();
        let run = |n: u32, cov: &dyn Fn(u32) -> bool| {
            let mut out = Vec::new();
            let mut start = None;
            for i in 0..=n {
                let c = i < n && cov(i);
                match (c, start) {
                    (true, None) => start = Some(i),
                    (false, Some(a)) => {
                        out.push((a, i));
                        start = None;
                    }
                    _ => {}
                }
            }
            out
        };
        let col_spans: Vec<Vec<(u32, u32)>> = (0..w).map(|x| run(h, &|y| bits[(y * w + x) as usize])).collect();
        let row_spans: Vec<Vec<(u32, u32)>> = (0..h).map(|y| run(w, &|x| bits[(y * w + x) as usize])).collect();
        Self {
            w,
            h,
            col,
            row,
            area,
            col_gaps: gaps(&col_spans),
            row_gaps: gaps(&row_spans),
        }
    }

    /// A solid `w × h` rectangle.
    #[must_use]
    pub fn rect(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            col: vec![Some((0, h)); w as usize],
            row: vec![Some((0, w)); h as usize],
            area: (w * h) as usize,
            col_gaps: vec![Vec::new(); w as usize],
            row_gaps: vec![Vec::new(); h as usize],
        }
    }

    /// Interior gaps per column of the frame after `k` quarter turns (as in
    /// [`rotated_profiles`](Self::rotated_profiles)).
    fn rotated_gaps(&self, k: u32) -> Vec<Vec<(u32, u32)>> {
        let (w, h) = (self.w, self.h);
        let flip =
            |v: &Vec<(u32, u32)>, n: u32| -> Vec<(u32, u32)> { v.iter().rev().map(|&(a, b)| (n - b, n - a)).collect() };
        match k % 4 {
            0 => self.col_gaps.clone(),
            1 => (0..h).map(|nc| self.row_gaps[(h - 1 - nc) as usize].clone()).collect(),
            2 => (0..w)
                .map(|nc| flip(&self.col_gaps[(w - 1 - nc) as usize], h))
                .collect(),
            _ => (0..h).map(|nc| flip(&self.row_gaps[nc as usize], w)).collect(),
        }
    }

    /// True if the shape covers its whole frame.
    pub fn is_rect(&self) -> bool {
        self.col.iter().all(|c| *c == Some((0, self.h))) && self.col_gaps.iter().all(Vec::is_empty)
    }

    /// Frame width and height.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    /// The shape on a grid of `block × block` pixel cells: a cell is covered when any of its
    /// pixels is (frame `⌈w / block⌉ × ⌈h / block⌉`). Packing these and scaling the
    /// placements by `block` (what [`PackOptions::block`] does) gives every shape whole cells.
    #[must_use]
    pub fn downsample(&self, block: u32) -> Self {
        let b = block.max(1);
        if b == 1 {
            return self.clone();
        }
        let (w, h) = (self.w.div_ceil(b), self.h.div_ceil(b));
        // Per coarse column (or row): the union of its fine columns' covered intervals.
        let coarse = |outline: &[Option<(u32, u32)>], gaps: &[Vec<(u32, u32)>], n: u32| {
            (0..n)
                .map(|c| {
                    let lo = (c * b) as usize;
                    let hi = (lo + b as usize).min(outline.len());
                    let mut v: Vec<(u32, u32)> = Vec::new();
                    for x in lo..hi {
                        let Some((a, z)) = outline[x] else { continue };
                        let mut start = a;
                        for &(g0, g1) in &gaps[x] {
                            v.push((start, g0));
                            start = g1;
                        }
                        v.push((start, z));
                    }
                    for e in &mut v {
                        *e = (e.0 / b, e.1.div_ceil(b));
                    }
                    merge(&mut v);
                    v
                })
                .collect::<Vec<_>>()
        };
        let cols = coarse(&self.col, &self.col_gaps, w);
        let rows = coarse(&self.row, &self.row_gaps, h);
        let outline = |v: &[Vec<(u32, u32)>]| -> Vec<Option<(u32, u32)>> {
            v.iter().map(|s| Some((s.first()?.0, s.last()?.1))).collect()
        };
        let col = outline(&cols);
        let area = col.iter().flatten().map(|(a, z)| (z - a) as usize).sum();
        Self {
            w,
            h,
            row: outline(&rows),
            col,
            area,
            col_gaps: gaps(&cols),
            row_gaps: gaps(&rows),
        }
    }

    /// [`rotated_profiles`](Self::rotated_profiles), with the turned ones left empty (never
    /// chosen) when rotation is not allowed.
    #[allow(clippy::type_complexity)]
    fn profiles(&self, rotations: bool) -> [(u32, u32, Vec<Option<(u32, u32)>>); 4] {
        let mut p = self.rotated_profiles();
        if !rotations {
            for e in &mut p[1..] {
                e.2.iter_mut().for_each(|c| *c = None);
            }
        }
        p
    }

    /// Column profiles of the mask under each of the four rotations: (frame width, frame
    /// height, per column (lowest row, one past the highest)).
    #[allow(clippy::type_complexity)]
    fn rotated_profiles(&self) -> [(u32, u32, Vec<Option<(u32, u32)>>); 4] {
        let (w, h) = (self.w, self.h);
        let (col, row) = (self.col.clone(), &self.row);
        // rot1: (i, j) -> (h-1-j, i): new column h-1-j is old row j, new rows are old columns.
        let r1: Vec<_> = (0..h).map(|nc| row[(h - 1 - nc) as usize]).collect();
        // rot2: (i, j) -> (w-1-i, h-1-j).
        let r2: Vec<_> = (0..w)
            .map(|nc| col[(w - 1 - nc) as usize].map(|(lo, hi)| (h - hi, h - lo)))
            .collect();
        // rot3: (i, j) -> (j, w-1-i): new column j is old row j, new row w-1-i.
        let r3: Vec<_> = (0..h)
            .map(|nc| row[nc as usize].map(|(lo, hi)| (w - hi, w - lo)))
            .collect();
        [(w, h, col), (h, w, r1), (w, h, r2), (h, w, r3)]
    }
}

/// Maps a point in a chart's pixel frame (w × h) to its frame after k × 90° CCW rotation.
#[must_use]
pub fn rotate_point(p: glam::Vec2, w: u32, h: u32, k: u32) -> glam::Vec2 {
    let (w, h) = (w as f32, h as f32);
    match k % 4 {
        0 => p,
        1 => glam::Vec2::new(h - p.y, p.x),
        2 => glam::Vec2::new(w - p.x, h - p.y),
        _ => glam::Vec2::new(p.y, w - p.x),
    }
}

#[cfg(test)]
fn tri_box_overlap(t: &[glam::Vec2; 3], lo: glam::Vec2, hi: glam::Vec2) -> bool {
    // Separating axes: box axes (handled by the caller's bounding box) and the triangle edges.
    let corners = [lo, glam::Vec2::new(hi.x, lo.y), hi, glam::Vec2::new(lo.x, hi.y)];
    let area = (t[1] - t[0]).perp_dot(t[2] - t[0]);
    if area.abs() < 1e-12 {
        // Degenerate: test the segment bounding box only.
        return true;
    }
    for e in 0..3 {
        let (a, b) = (t[e], t[(e + 1) % 3]);
        let n = (b - a).perp() * area.signum(); // inward normal
        // If all corners are strictly outside this edge, separated.
        if corners.iter().all(|&c| n.dot(c - a) < 0.0) {
            return false;
        }
    }
    true
}

/// max(0, `max_c(window`[c] − lo[c])): where a chart's lower outline comes to rest on the skyline.
/// A dense, branch-free max that the compiler vectorizes.
#[inline]
fn resting_height(window: &[i32], lo: &[i32]) -> i32 {
    let mut y = 0i32;
    for (s, l) in window.iter().zip(lo) {
        y = y.max(s.wrapping_sub(*l));
    }
    y
}

/// Placement of a shape: pixel offset of its frame after `rotation` quarter turns (CCW). Map
/// points of the shape's own frame with [`rotate_point`], then add `(x, y)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct MaskPlacement {
    /// Column of the rotated frame's left edge.
    pub x: u32,
    /// Row of the rotated frame's bottom edge.
    pub y: u32,
    /// Quarter turns counter-clockwise, 0..=3.
    pub rotation: u32,
}

/// Packs chart footprints into a `width × height` atlas: each chart (largest first, best of four
/// rotations) drops onto a per-column skyline where its lower outline fits with the least wasted
/// area, so irregular charts interlock instead of being packed as bounding boxes.
#[deprecated(since = "0.1.0", note = "use `pack` with `HoleFill::Never`")]
#[must_use]
pub fn pack_charts(masks: &[ChartMask], width: u32, height: u32) -> Option<(Vec<MaskPlacement>, u32)> {
    skyline(&reserve_empty(masks), width, height, true)
}

/// `masks` with every shape that covers no pixels replaced by its solid frame. The outline
/// packers rest a shape on its covered columns, so an empty one would never find a place.
fn reserve_empty(masks: &[ChartMask]) -> std::borrow::Cow<'_, [ChartMask]> {
    if masks.iter().all(|m| m.area > 0) {
        return std::borrow::Cow::Borrowed(masks);
    }
    std::borrow::Cow::Owned(
        masks
            .iter()
            .map(|m| {
                if m.area > 0 {
                    m.clone()
                } else {
                    ChartMask::rect(m.w, m.h)
                }
            })
            .collect(),
    )
}

fn skyline(masks: &[ChartMask], width: u32, height: u32, rotations: bool) -> Option<(Vec<MaskPlacement>, u32)> {
    let covered: Vec<usize> = masks.iter().map(|m| m.area).collect();
    let mut order: Vec<usize> = (0..masks.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse((masks[i].w.max(masks[i].h), covered[i])));
    let mut sky = vec![0i32; width as usize];
    // prefix[x] = Σ sky[..x]: window sums for the wasted-area term in O(1).
    let mut prefix = vec![0i64; width as usize + 1];
    let mut top = 0i64;
    let mut out = vec![
        MaskPlacement {
            x: 0,
            y: 0,
            rotation: 0
        };
        masks.len()
    ];
    // Lower profile per column with empty columns pushed far down (EMPTY), so the resting
    // height is a dense max over the window (no branches; vectorizes).
    for i in order {
        let profs = masks[i].profiles(rotations);
        let sky_now = Skyline {
            sky: &sky,
            prefix: &prefix,
            top,
            width,
            height,
        };
        // The rotations are searched independently (in parallel for big charts) and the best
        // is taken in rotation order: the same choice for any thread count.
        let big = masks[i].w.max(masks[i].h) as usize * width as usize >= 1 << 15;
        let found: Vec<Option<Fit>> = if big {
            par::map(&profs, 1, |(mw, mh, prof)| sky_now.search(*mw, *mh, prof))
        } else {
            profs
                .into_iter()
                .map(|(mw, mh, prof)| sky_now.search(mw, mh, &prof))
                .collect()
        };
        let mut best: Option<(u64, u32, u32, u32)> = None; // (score, x, y, rotation)
        for (k, f) in found.iter().enumerate() {
            if let Some(Fit {
                best: Some((score, x0, y0)),
                ..
            }) = f
                && best.is_none_or(|bb| *score < bb.0)
            {
                best = Some((*score, *x0, *y0, k as u32));
            }
        }
        let (_, x0, y0, k) = best?;
        let Fit { lo, hi, .. } = found[k as usize].as_ref().unwrap();
        for c in 0..lo.len() {
            if lo[c] != EMPTY {
                let s = &mut sky[x0 as usize + c];
                *s = (*s).max(y0 as i32 + hi[c]);
                top = top.max(i64::from(y0) + i64::from(hi[c]));
            }
        }
        for x in 0..width as usize {
            prefix[x + 1] = prefix[x] + i64::from(sky[x]);
        }
        out[i] = MaskPlacement {
            x: x0,
            y: y0,
            rotation: k,
        };
    }
    Some((out, top as u32))
}

/// Packs like [`pack_charts`], but each atlas column keeps a list of free intervals instead of
/// a single top, so a chart can also go into a hole left under an earlier chart's overhang or
/// interlock beside it. Each chart takes the position (of four rotations) that raises the
/// atlas top least, then the lowest, then the leftmost; x is searched at a stride and refined
/// around the best few. Worth it when there are few, large, irregular charts, where the
/// skyline's lost space under overhangs is a large share of the atlas.
#[deprecated(since = "0.1.0", note = "use `pack` with `HoleFill::Always`")]
#[must_use]
pub fn pack_charts_holes(masks: &[ChartMask], width: u32, height: u32) -> Option<(Vec<MaskPlacement>, u32)> {
    holes(&reserve_empty(masks), width, height, true)
}

fn holes(masks: &[ChartMask], width: u32, height: u32, rotations: bool) -> Option<(Vec<MaskPlacement>, u32)> {
    holes_nest(masks, width, height, rotations, false)
}

fn holes_nest(
    masks: &[ChartMask],
    width: u32,
    height: u32,
    rotations: bool,
    nest: bool,
) -> Option<(Vec<MaskPlacement>, u32)> {
    let mut order: Vec<usize> = (0..masks.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse((masks[i].w.max(masks[i].h), masks[i].area)));
    let w = width as usize;
    // Holes shorter than every column of every chart can never be used: dropped.
    let min_span = masks
        .iter()
        .flat_map(|m| m.col.iter().chain(&m.row).flatten())
        .map(|&(a, b)| b - a)
        .min()
        .unwrap_or(1);
    // Per column: free intervals (ascending; the last runs to `height`), the start of that
    // last interval (the skyline), and a running count of columns with holes below it.
    let mut free: Vec<Vec<(u32, u32)>> = vec![vec![(0, height)]; w];
    let mut tops = vec![0i32; w];
    let mut holed = vec![0u32; w + 1];
    let mut top = 0u32;
    let mut out = vec![
        MaskPlacement {
            x: 0,
            y: 0,
            rotation: 0
        };
        masks.len()
    ];
    for i in order {
        let profs = masks[i].profiles(rotations);
        // (raised top, y, x, rotation)
        let mut best: Option<(u32, u32, u32, u32)> = None;
        for (k, (mw, mh, prof)) in profs.iter().enumerate() {
            let (mw, mh) = (*mw, *mh);
            if mw > width || mh > height || prof.iter().all(std::option::Option::is_none) {
                continue;
            }
            let hmax = prof.iter().flatten().map(|p| p.1).max().unwrap_or(0);
            let lo: Vec<i32> = prof.iter().map(|p| p.map_or(EMPTY, |(l, _)| l as i32)).collect();
            let (free, tops, holed) = (&free, &tops, &holed);
            // Lowest y at which every column of the mask fits into a free interval. Without
            // holes in the window this is the skyline's resting height. With holes, columns
            // without them only bound y from below; the holed ones are settled by raising y
            // to each one's next fitting interval until a pass changes nothing.
            // With `step` > 1 only every step-th column is checked: a lower bound, for ranking.
            let lowest = |x: u32, step: usize| -> Option<u32> {
                let x = x as usize;
                let sky = resting_height(&tops[x..x + mw as usize], &lo).max(0) as u32;
                if holed[x + mw as usize] == holed[x] {
                    return Some(sky);
                }
                // Feasible y: [base, sky] intersected with each holed column's fitting ranges.
                let mut base = 0u32;
                for (c, span) in prof.iter().enumerate().step_by(step) {
                    if let Some((l, _)) = *span
                        && free[x + c].len() == 1
                    {
                        base = base.max((tops[x + c] - l as i32).max(0) as u32);
                    }
                }
                if base >= sky {
                    return Some(sky);
                }
                let mut ok: Vec<(u32, u32)> = vec![(base, sky)];
                let mut next: Vec<(u32, u32)> = Vec::with_capacity(4);
                for (c, span) in prof.iter().enumerate().step_by(step) {
                    let Some((l, h)) = *span else { continue };
                    let col = &free[x + c];
                    if col.len() == 1 {
                        continue;
                    }
                    next.clear();
                    let mut j = 0;
                    // Only intervals that can hold the column somewhere in [ok first, ok last].
                    let (ymin, ymax) = (ok[0].0, ok[ok.len() - 1].1);
                    let from = col.partition_point(|&(_, b)| b < ymin + h);
                    for &(a, b) in &col[from..] {
                        if a > ymax + l {
                            break;
                        }
                        if b - a < h - l {
                            continue;
                        }
                        let (ya, yb) = (a.saturating_sub(l), b - h);
                        while j < ok.len() && ok[j].1 < ya {
                            j += 1;
                        }
                        let mut m = j;
                        while m < ok.len() && ok[m].0 <= yb {
                            let (s0, e0) = (ok[m].0.max(ya), ok[m].1.min(yb));
                            if s0 <= e0 {
                                next.push((s0, e0));
                            }
                            m += 1;
                        }
                    }
                    std::mem::swap(&mut ok, &mut next);
                    // Only sky itself is left (always feasible): nothing lower fits.
                    if ok.is_empty() || ok[0].0 >= sky {
                        return Some(sky);
                    }
                }
                Some(ok[0].0.min(sky))
            };
            let score = |x: u32, step: usize| lowest(x, step).map(|y| (top.max(y + hmax), y, x));
            let span = width - mw;
            let stride = (mw / 24).max(width / 128).max(2).min(span.max(1));
            let mut xs: Vec<u32> = (0..=span).step_by(stride as usize).collect();
            if xs.last() != Some(&span) {
                xs.push(span);
            }
            let par = xs.len() * mw as usize >= 1 << 15;
            let mut coarse: Vec<(u32, u32, u32)> = if par {
                par::map(&xs, 8, |&x| score(x, 4)).into_iter().flatten().collect()
            } else {
                xs.iter().filter_map(|&x| score(x, 4)).collect()
            };
            coarse.sort_unstable();
            let mut refine: Vec<u32> = coarse
                .iter()
                .take(3)
                .flat_map(|&(_, _, cx)| cx.saturating_sub(stride - 1)..=(cx + stride - 1).min(span))
                .collect();
            refine.sort_unstable();
            refine.dedup();
            // Branch and bound: rank the window by the cheap lower bound, then check exactly in
            // that order until the bound exceeds the best exact score (over all rotations so
            // far). Same choice as checking every position exactly.
            let mut bounds: Vec<(u32, u32, u32)> = if refine.len() * mw as usize >= 1 << 15 {
                par::map(&refine, 8, |&x| score(x, 4)).into_iter().flatten().collect()
            } else {
                refine.iter().filter_map(|&x| score(x, 4)).collect()
            };
            bounds.sort_unstable();
            for lb in bounds {
                if best.is_some_and(|b| lb > (b.0, b.1, b.2)) {
                    break;
                }
                if let Some((t, y, x)) = score(lb.2, 1)
                    && best.is_none_or(|b| (t, y, x) < (b.0, b.1, b.2))
                {
                    best = Some((t, y, x, k as u32));
                }
            }
        }
        let (_, y, x, k) = best?;
        let (_, _, prof) = &profs[k as usize];
        let shape_gaps = if nest {
            masks[i].rotated_gaps(k)
        } else {
            vec![Vec::new(); prof.len()]
        };
        for (c, span) in prof.iter().enumerate() {
            let Some((lo, hi)) = *span else { continue };
            let (y0, y1) = (y + lo, y + hi);
            let col = &mut free[x as usize + c];
            let at = col.iter().position(|&(a, b)| a <= y0 && y1 <= b)?;
            let (a, b) = col[at];
            col.remove(at);
            if y1 < b {
                col.insert(at, (y1, b));
            }
            // Holes inside the shape stay free (in order, between the two remainders).
            for &(g0, g1) in shape_gaps[c].iter().rev() {
                if g1 - g0 >= min_span {
                    col.insert(at, (y + g0, y + g1));
                }
            }
            if y0 - a >= min_span {
                col.insert(at, (a, y0));
            }
            tops[x as usize + c] = col.last().unwrap().0 as i32;
            top = top.max(y1);
        }
        for c in 0..w {
            holed[c + 1] = holed[c] + u32::from(free[c].len() > 1);
        }
        out[i] = MaskPlacement { x, y, rotation: k };
    }
    Some((out, top))
}

/// A rotation's resting profile and its best placement, if any.
struct Fit {
    lo: Vec<i32>,
    hi: Vec<i32>,
    /// (score, x, y)
    best: Option<(u64, u32, u32)>,
}

/// Lower profile marker for empty columns (see [`pack_charts`]).
const EMPTY: i32 = i32::MAX / 4;

/// The packer's skyline as one placement sees it.
struct Skyline<'a> {
    sky: &'a [i32],
    prefix: &'a [i64],
    top: i64,
    width: u32,
    height: u32,
}

impl Skyline<'_> {
    /// Best position of a mask with this rotated profile: least wasted area under it plus
    /// any raise of the top, coarse pass at a stride then refinement around the best few.
    fn search(&self, mw: u32, mh: u32, prof: &[Option<(u32, u32)>]) -> Option<Fit> {
        let (width, height_u) = (self.width, self.height);
        if mw > width || mh > height_u {
            return None;
        }
        let lo: Vec<i32> = prof.iter().map(|p| p.map_or(EMPTY, |(l, _)| l as i32)).collect();
        let hi: Vec<i32> = prof.iter().map(|p| p.map_or(0, |(_, h)| h as i32)).collect();
        let filled = prof.iter().filter(|p| p.is_some()).count() as i64;
        if filled == 0 {
            return None;
        }
        // Window sum over filled columns = full window sum − the (few) empty columns.
        let empties: Vec<usize> = (0..lo.len()).filter(|&c| lo[c] == EMPTY).collect();
        let sum_lo: i64 = lo.iter().filter(|&&l| l != EMPTY).map(|&l| i64::from(l)).sum();
        let hmax = i64::from(*hi.iter().max().unwrap());
        let (w, height, width_u, top_now) = (mw as usize, i64::from(height_u), u64::from(width), self.top);
        let (sky_ref, prefix_ref, lo_ref, empties_ref) = (self.sky, self.prefix, &lo, &empties);
        let eval = |x0: u32| -> Option<(u64, u32)> {
            let x = x0 as usize;
            let window = &sky_ref[x..x + w];
            let y0 = i64::from(resting_height(window, lo_ref));
            let peak = y0 + hmax;
            if peak > height {
                return None;
            }
            // waste = Σ over filled columns of (y0 + lo − sky)
            let sky_sum =
                prefix_ref[x + w] - prefix_ref[x] - empties_ref.iter().map(|&c| i64::from(window[c])).sum::<i64>();
            let waste = (filled * y0 + sum_lo - sky_sum) as u64;
            let raise = (peak - top_now).max(0) as u64;
            Some((waste + raise * width_u, y0 as u32))
        };
        let span = width - mw;
        let stride = (mw / 24).max(3).min(span.max(1));
        let mut xs: Vec<u32> = Vec::new();
        let mut x0 = 0;
        loop {
            xs.push(x0);
            if x0 == span {
                break;
            }
            x0 = (x0 + stride).min(span);
        }
        // Wide charts in wide atlases: positions are scored in parallel (same scores, same
        // order, so the same choice for any thread count).
        let parallel = xs.len() * w >= 1 << 16;
        let scored: Vec<Option<(u64, u32)>> = if parallel {
            par::map(&xs, 16, |&x| eval(x))
        } else {
            xs.iter().map(|&x| eval(x)).collect()
        };
        let mut coarse: Vec<(u64, u32)> = xs
            .iter()
            .zip(&scored)
            .filter_map(|(&x, s)| s.map(|(sc, _)| (sc, x)))
            .collect();
        // Only the best few coarse positions are refined: partial selection, not a full sort.
        let keep = (if stride > 1 { 4 } else { 1 }).min(coarse.len());
        if keep > 0 && keep < coarse.len() {
            coarse.select_nth_unstable(keep - 1);
            coarse.truncate(keep);
        }
        coarse.sort_unstable();
        let refine: Vec<u32> = coarse
            .iter()
            .take(keep)
            .flat_map(|&(_, cx)| cx.saturating_sub(stride - 1)..=(cx + stride - 1).min(span))
            .collect();
        let scored: Vec<Option<(u64, u32)>> = if parallel {
            par::map(&refine, 16, |&x| eval(x))
        } else {
            refine.iter().map(|&x| eval(x)).collect()
        };
        // First strictly better score in refinement order wins.
        let mut best: Option<(u64, u32, u32)> = None;
        for (&x0, s) in refine.iter().zip(scored) {
            if let Some((score, y0)) = s
                && best.is_none_or(|bb| score < bb.0)
            {
                best = Some((score, x0, y0));
            }
        }
        Some(Fit { lo, hi, best })
    }
}

/// Irregular shape to pack: per-column and per-row covered extents in its own pixel frame.
pub type Shape = ChartMask;

/// When to also run the hole-filling packer (see [`pack`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum HoleFill {
    /// For at most [`HOLE_FILL_AUTO_MAX`] shapes, where it pays off (default).
    #[default]
    Auto,
    /// Always, alongside the skyline packer.
    Always,
    /// Never: skyline packing only (fastest).
    Never,
}

/// Shape count up to which [`HoleFill::Auto`] runs the hole-filling packer.
pub const HOLE_FILL_AUTO_MAX: usize = 32;

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
/// Options for [`pack`]. Build them from [`PackOptions::width`] and the `with_*` methods (or
/// `..PackOptions::width(w)`): fields may be added in minor versions, always with a default.
pub struct PackOptions {
    /// Atlas width in pixels.
    pub width: u32,
    /// Fail if the packing would be taller than this. None: unbounded.
    pub max_height: Option<u32>,
    /// Allow quarter turns.
    pub rotations: bool,
    /// When to also run the hole-filling packer.
    pub fill_holes: HoleFill,
    /// Let shapes go inside holes of other shapes (a ring's middle). Needs the hole-filling
    /// packer. Off by default: UV charts mostly have sliver holes, which only fragment the
    /// free space (water bottle 0.626 → 0.601 utilization with it on).
    #[cfg_attr(feature = "serde", serde(default))]
    pub nest_in_holes: bool,
    /// Grid cell size in pixels. Above 1, shapes are packed on a grid of `block × block`
    /// cells and each shape owns every cell it touches (see [`ChartMask::downsample`]), so
    /// no cell holds pixels of two shapes. With `block = 2^k`, texels of mip level `k` and
    /// coarser each belong to one shape, so mipmaps don't bleed one shape into another.
    /// Pad shapes by at least `block` pixels to also keep a one-texel gutter at that level.
    /// The width is used in whole cells and the height is a multiple of `block`. 1 (default):
    /// pixel placement.
    #[cfg_attr(feature = "serde", serde(default = "one"))]
    pub block: u32,
}

#[cfg(feature = "serde")]
fn one() -> u32 {
    1
}

impl PackOptions {
    /// Defaults for an atlas `width` pixels wide: unbounded height, rotations on,
    /// [`HoleFill::Auto`], no nesting in holes.
    #[must_use]
    pub fn width(width: u32) -> Self {
        Self {
            width,
            max_height: None,
            rotations: true,
            fill_holes: HoleFill::Auto,
            nest_in_holes: false,
            block: 1,
        }
    }

    /// Sets [`max_height`](Self::max_height).
    #[must_use]
    pub fn with_max_height(mut self, max_height: u32) -> Self {
        self.max_height = Some(max_height);
        self
    }

    /// Sets [`rotations`](Self::rotations).
    #[must_use]
    pub fn with_rotations(mut self, rotations: bool) -> Self {
        self.rotations = rotations;
        self
    }

    /// Sets [`fill_holes`](Self::fill_holes).
    #[must_use]
    pub fn with_fill_holes(mut self, fill_holes: HoleFill) -> Self {
        self.fill_holes = fill_holes;
        self
    }

    /// Sets [`nest_in_holes`](Self::nest_in_holes).
    #[must_use]
    pub fn with_nest_in_holes(mut self, nest: bool) -> Self {
        self.nest_in_holes = nest;
        self
    }

    /// Sets [`block`](Self::block).
    #[must_use]
    pub fn with_block(mut self, block: u32) -> Self {
        self.block = block;
        self
    }
}

/// Result of [`pack`]: one placement per shape, in input order, and the height used.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Packing {
    /// One per input shape, in input order.
    pub placements: Vec<MaskPlacement>,
    /// Rows used (the atlas can be cropped to `width × height`).
    pub height: u32,
}

/// Packs irregular shapes into an atlas `opts.width` wide, as low as it can. Shapes are
/// placed largest first; one that covers no pixels (say, rasterized from zero-area triangles)
/// reserves its whole frame, and one with no width or height goes at the origin. The skyline
/// packer drops each one where its lower outline wastes
/// least; the hole-filling packer (per [`PackOptions::fill_holes`]) can also nest shapes
/// under overhangs. Both run (in parallel), so do the rectangle packers of
/// [`pack_rects_strip`] on the shapes' bounding rectangles, and the lowest atlas wins. When
/// every shape is a full rectangle, only the rectangle packers run.
///
/// ```
/// use snugly::{pack, ChartMask, PackOptions};
/// // An L shape (from a raster mask) and a square that fits in its notch.
/// let l: Vec<bool> = (0..100).map(|i| i % 10 < 4 || i / 10 < 4).collect();
/// let shapes = [ChartMask::from_bits(10, 10, &l), ChartMask::rect(6, 6)];
/// let packing = pack(&shapes, &PackOptions::width(10)).unwrap();
/// assert_eq!(packing.height, 10);
/// ```
///
/// # Errors
///
/// [`PackError::TooWide`] if a shape is wider than `opts.width` in every allowed orientation,
/// [`PackError::TooTall`] if the packing needs more than [`PackOptions::max_height`] rows.
#[expect(
    clippy::missing_panics_doc,
    reason = "widths are checked above, so a packer always succeeds"
)]
pub fn pack(shapes: &[Shape], opts: &PackOptions) -> Result<Packing, PackError> {
    if opts.block > 1 {
        return pack_blocks(shapes, opts);
    }
    let shapes = &*reserve_empty(shapes);
    for (index, s) in shapes.iter().enumerate() {
        if s.w > opts.width && !(opts.rotations && s.h <= opts.width) {
            return Err(PackError::TooWide {
                index,
                width: opts.width,
            });
        }
    }
    // Frames with no width or height take no room: placed at the origin, packed without.
    if shapes.iter().any(|s| s.w == 0 || s.h == 0) {
        let sized: Vec<usize> = (0..shapes.len())
            .filter(|&i| shapes[i].w > 0 && shapes[i].h > 0)
            .collect();
        let sub: Vec<ChartMask> = sized.iter().map(|&i| shapes[i].clone()).collect();
        let packed = pack(&sub, opts)?;
        let mut placements = vec![
            MaskPlacement {
                x: 0,
                y: 0,
                rotation: 0
            };
            shapes.len()
        ];
        for (k, &i) in sized.iter().enumerate() {
            placements[i] = packed.placements[k];
        }
        return Ok(Packing {
            placements,
            height: packed.height,
        });
    }
    let packing = if !shapes.is_empty() && shapes.iter().all(ChartMask::is_rect) {
        // Outline packing can't beat MaxRects on plain rectangles.
        let sizes: Vec<(u32, u32)> = shapes.iter().map(|s| (s.w, s.h)).collect();
        let (p, h) = pack_rects_strip(&sizes, opts.width, 0, opts.rotations)?;
        Packing {
            placements: p
                .iter()
                .map(|q| MaskPlacement {
                    x: q.x,
                    y: q.y,
                    rotation: u32::from(q.rotated),
                })
                .collect(),
            height: h,
        }
    } else {
        let height = u32::MAX / 2;
        let run_holes = match opts.fill_holes {
            HoleFill::Auto => shapes.len() <= HOLE_FILL_AUTO_MAX,
            HoleFill::Always => true,
            HoleFill::Never => false,
        };
        let (sky, hole) = par::join(
            || skyline(shapes, opts.width, height, opts.rotations),
            || {
                (run_holes || opts.nest_in_holes)
                    .then(|| holes_nest(shapes, opts.width, height, opts.rotations, opts.nest_in_holes))
                    .flatten()
            },
        );
        let (mut placements, mut height) = match (sky, hole) {
            (Some(a), Some(b)) => {
                if b.1 < a.1 {
                    b
                } else {
                    a
                }
            }
            (a, b) => a.or(b).expect("every shape fits the atlas width"),
        };
        // Bounding rectangles packed with the rectangle packers: wins for many nearly
        // rectangular shapes, where outlines add little and MaxRects packs tighter. Only
        // heights below the outline packers' result are searched, and not at all when the
        // rectangles' area alone rules that out.
        let sizes: Vec<(u32, u32)> = shapes.iter().map(|s| (s.w, s.h)).collect();
        let area: u64 = sizes.iter().map(|&(w, h)| u64::from(w) * u64::from(h)).sum();
        let lo = sizes
            .iter()
            .map(|&(w, h)| {
                if opts.rotations && w.max(h) <= opts.width {
                    w.min(h)
                } else {
                    h
                }
            })
            .max()
            .unwrap_or(0)
            .max(area.div_ceil(u64::from(opts.width.max(1))) as u32);
        if lo < height
            && let Some((p, h)) = maxrects::best_strip(&sizes, opts.width, lo, height - 1, opts.rotations)
            && h < height
        {
            height = h;
            placements = p
                .iter()
                .map(|&(x, y, r)| MaskPlacement {
                    x,
                    y,
                    rotation: u32::from(r),
                })
                .collect();
        }
        Packing { placements, height }
    };
    match opts.max_height {
        Some(m) if packing.height > m => Err(PackError::TooTall {
            height: packing.height,
            max_height: m,
        }),
        _ => Ok(packing),
    }
}

/// [`pack`] on a grid of `opts.block`-pixel cells: packs the shapes' cell masks, then maps
/// the placements back to pixels.
fn pack_blocks(shapes: &[Shape], opts: &PackOptions) -> Result<Packing, PackError> {
    let b = opts.block;
    let coarse: Vec<ChartMask> = shapes.iter().map(|s| s.downsample(b)).collect();
    let sub = PackOptions {
        width: opts.width / b,
        max_height: opts.max_height.map(|m| m / b),
        block: 1,
        ..opts.clone()
    };
    let packed = pack(&coarse, &sub).map_err(|e| match e {
        PackError::TooWide { index, .. } => PackError::TooWide {
            index,
            width: opts.width,
        },
        PackError::TooTall { height, .. } => PackError::TooTall {
            height: height * b,
            max_height: opts.max_height.unwrap_or(0),
        },
    })?;
    let placements = packed
        .placements
        .iter()
        .zip(shapes.iter().zip(&coarse))
        .map(|(p, (s, c))| {
            // The cell frame is larger than the shape's own frame, and `rotate_point` with the
            // shape's frame turns about a different centre: shift by the difference so callers
            // keep mapping points with their own frame size.
            let k = p.rotation;
            let big = rotate_point(glam::Vec2::ZERO, c.w * b, c.h * b, k);
            let own = rotate_point(glam::Vec2::ZERO, s.w, s.h, k);
            let d = big - own;
            MaskPlacement {
                x: p.x * b + d.x as u32,
                y: p.y * b + d.y as u32,
                rotation: k,
            }
        })
        .collect();
    Ok(Packing {
        placements,
        height: packed.height * b,
    })
}

#[cfg(test)]
#[allow(deprecated)]
mod mask_tests {
    use super::*;
    use glam::Vec2;

    /// Shapes that cover no pixels (no triangles, or only zero-area ones) reserve their
    /// frame, and zero-size frames go at the origin, instead of panicking.
    #[test]
    fn empty_and_zero_size_shapes_pack() {
        let p = Vec2::splat(1.0);
        let l: Vec<bool> = (0..100).map(|i| i % 10 < 4 || i / 10 < 4).collect();
        let empties = [
            ChartMask::from_bits(5, 5, &[false; 25]),
            ChartMask::from_triangles(&[], 5, 5, 1),
            ChartMask::from_triangles(&[[p; 3]], 3, 3, 0),
            ChartMask::rect(0, 0),
            ChartMask::from_bits(4, 0, &[]),
        ];
        for empty in empties {
            for fill in [HoleFill::Never, HoleFill::Always] {
                for nest in [false, true] {
                    let opts = PackOptions::width(10).with_fill_holes(fill).with_nest_in_holes(nest);
                    let shapes = [ChartMask::from_bits(10, 10, &l), empty.clone()];
                    let packing = pack(&shapes, &opts).unwrap();
                    let (w, h) = empty.size();
                    if w > 0 && h > 0 {
                        // The frame is reserved: clear of the L's covered pixels.
                        let q = packing.placements[1];
                        let (fw, fh) = if q.rotation % 2 == 1 { (h, w) } else { (w, h) };
                        assert!(q.x + fw <= 10 && q.x >= 4 && q.y >= 4 && fh <= packing.height - q.y);
                    }
                    assert!(pack(std::slice::from_ref(&empty), &opts).is_ok());
                }
            }
        }
    }

    /// Pixel-mask reference: conservative raster plus square dilation.
    fn raster(tris: &[[Vec2; 3]], w: u32, h: u32, pad: u32) -> Vec<bool> {
        let mut bits = vec![false; (w * h) as usize];
        for t in tris {
            let lo = t[0].min(t[1]).min(t[2]).floor().max(Vec2::ZERO);
            let hi = t[0].max(t[1]).max(t[2]).ceil().min(Vec2::new(w as f32, h as f32));
            for y in lo.y as u32..hi.y as u32 {
                for x in lo.x as u32..hi.x as u32 {
                    let (a, b) = (Vec2::new(x as f32, y as f32), Vec2::new(x as f32 + 1.0, y as f32 + 1.0));
                    if tri_box_overlap(t, a, b) {
                        bits[(y * w + x) as usize] = true;
                    }
                }
            }
        }
        let r = i64::from(pad);
        (0..i64::from(w * h))
            .map(|i| {
                let (x, y) = (i % i64::from(w), i / i64::from(w));
                (-r..=r).any(|dy| {
                    (-r..=r).any(|dx| {
                        let (xx, yy) = (x + dx, y + dy);
                        xx >= 0
                            && yy >= 0
                            && xx < i64::from(w)
                            && yy < i64::from(h)
                            && bits[(yy * i64::from(w) + xx) as usize]
                    })
                })
            })
            .collect()
    }

    fn rotate_bits(bits: &[bool], w: u32, h: u32, k: u32) -> (u32, u32, Vec<bool>) {
        let (nw, nh) = if k % 2 == 1 { (h, w) } else { (w, h) };
        let mut out = vec![false; bits.len()];
        for j in 0..h {
            for i in 0..w {
                if bits[(j * w + i) as usize] {
                    let (ni, nj) = match k % 4 {
                        0 => (i, j),
                        1 => (h - 1 - j, i),
                        2 => (w - 1 - i, h - 1 - j),
                        _ => (j, w - 1 - i),
                    };
                    out[(nj * nw + ni) as usize] = true;
                }
            }
        }
        (nw, nh, out)
    }

    fn col_profiles(bits: &[bool], w: u32, h: u32) -> Vec<Option<(u32, u32)>> {
        ChartMask::from_bits(w, h, bits).col
    }

    #[test]
    fn one_pass_profiles_match_explicit_rotations() {
        let (w, h) = (7u32, 4u32);
        let mut bits = vec![false; (w * h) as usize];
        for &(x, y) in &[(0, 0), (1, 0), (3, 1), (6, 3), (2, 2), (5, 0)] {
            bits[(y * w + x) as usize] = true;
        }
        let fast = ChartMask::from_bits(w, h, &bits).rotated_profiles();
        for k in 0..4u32 {
            let (rw, rh, rb) = rotate_bits(&bits, w, h, k);
            assert_eq!((rw, rh), (fast[k as usize].0, fast[k as usize].1), "rotation {k} frame");
            assert_eq!(col_profiles(&rb, rw, rh), fast[k as usize].2, "rotation {k} profiles");
        }
    }

    #[test]
    fn outlines_match_pixel_raster() {
        // Random triangle fans (a chart-like blob), several paddings.
        let mut seed = 12345u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 10_000) as f32 / 10_000.0
        };
        for case in 0..40 {
            let (w, h) = (30 + case % 7, 20 + case % 5);
            let pad = case % 3;
            let tris: Vec<[Vec2; 3]> = (0..6)
                .map(|_| {
                    [0; 3].map(|_| {
                        Vec2::new(
                            pad as f32 + rnd() * (w - 2 * pad - 1) as f32,
                            pad as f32 + rnd() * (h - 2 * pad - 1) as f32,
                        )
                    })
                })
                .collect();
            let reference = raster(&tris, w, h, pad);
            let m = ChartMask::from_triangles(&tris, w, h, pad);
            let r = ChartMask::from_bits(w, h, &reference);
            assert_eq!(m.col, r.col, "case {case} columns");
            assert_eq!(m.row, r.row, "case {case} rows");
        }
    }

    #[test]
    fn rotate_point_matches_mask_rotation() {
        let (w, h) = (5u32, 3u32);
        for k in 0..4 {
            // centre of pixel (1, 2) must land on the centre of the rotated pixel
            let p = rotate_point(Vec2::new(1.5, 2.5), w, h, k);
            let mut bits = vec![false; 15];
            bits[(2 * w + 1) as usize] = true;
            let (rw, _, rb) = rotate_bits(&bits, w, h, k);
            let (px, py) = (p.x.floor() as u32, p.y.floor() as u32);
            assert!(rb[(py * rw + px) as usize], "rotation {k}");
        }
    }
    /// Occupied pixels of a placement, from the column outlines the packers use.
    fn cells(m: &ChartMask, p: &MaskPlacement) -> Vec<(u32, u32)> {
        let (_, _, prof) = &m.rotated_profiles()[p.rotation as usize];
        let gaps = m.rotated_gaps(p.rotation);
        let mut out = Vec::new();
        for (c, s) in prof.iter().enumerate() {
            if let Some((lo, hi)) = *s {
                out.extend(
                    (lo..hi)
                        .filter(|y| !gaps[c].iter().any(|&(a, b)| (a..b).contains(y)))
                        .map(|y| (p.x + c as u32, p.y + y)),
                );
            }
        }
        out
    }

    #[test]
    fn hole_packer_nests_under_an_overhang() {
        // An H (notches above and below its bar) and two blocks, one per notch. The skyline
        // can fill the upper notch only; no rotation opens the lower one to it.
        let (hw, hh) = (40u32, 40u32);
        let h_shape: Vec<bool> = (0..hh)
            .flat_map(|y| (0..hw).map(move |x| !(10..30).contains(&x) || (15..25).contains(&y)))
            .collect();
        let block = vec![true; 18 * 12];
        let masks = [
            ChartMask::from_bits(hw, hh, &h_shape),
            ChartMask::from_bits(18, 12, &block),
            ChartMask::from_bits(18, 12, &block),
        ];
        let (sky_p, sky_h) = pack_charts(&masks, 40, 1000).unwrap();
        let (hole_p, hole_h) = pack_charts_holes(&masks, 40, 1000).unwrap();
        assert!(hole_h < sky_h, "holes {hole_h} vs skyline {sky_h}");
        assert_eq!(hole_h, 40);
        for placements in [&sky_p, &hole_p] {
            let mut used = std::collections::HashSet::new();
            for (m, p) in masks.iter().zip(placements.iter()) {
                for c in cells(m, p) {
                    assert!(c.0 < 40 && used.insert(c), "overlap or outside at {c:?}");
                }
            }
        }
    }

    #[test]
    fn hole_packer_never_overlaps() {
        let mut seed = 0x9e37_79b9u32;
        let mut rnd = |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % n
        };
        // Random blobby masks (union of rectangles) packed into a narrow atlas.
        let masks: Vec<ChartMask> = (0..24)
            .map(|_| {
                let (w, h) = (6 + rnd(30), 6 + rnd(30));
                let mut bits = vec![false; (w * h) as usize];
                for _ in 0..3 {
                    let (x0, y0) = (rnd(w), rnd(h));
                    let (x1, y1) = (x0 + 1 + rnd(w - x0), y0 + 1 + rnd(h - y0));
                    for y in y0..y1 {
                        for x in x0..x1 {
                            bits[(y * w + x) as usize] = true;
                        }
                    }
                }
                ChartMask::from_bits(w, h, &bits)
            })
            .collect();
        let (pl, top) = pack_charts_holes(&masks, 64, 100_000).unwrap();
        let mut used = std::collections::HashSet::new();
        for (m, p) in masks.iter().zip(&pl) {
            for c in cells(m, p) {
                assert!(c.0 < 64 && c.1 < top);
                assert!(used.insert(c), "overlap at {c:?}");
            }
        }
    }

    #[test]
    fn pack_api_respects_rotation_and_height() {
        // A tall thin shape fits a short wide atlas only when turned.
        let shapes = [Shape::rect(10, 60), Shape::rect(30, 10)];
        let turned = pack(&shapes, &PackOptions::width(64)).unwrap();
        assert!(turned.height <= 20, "{}", turned.height);
        let upright = pack(
            &shapes,
            &PackOptions {
                rotations: false,
                ..PackOptions::width(64)
            },
        )
        .unwrap();
        assert!(upright.placements.iter().all(|p| p.rotation == 0));
        assert!(upright.height >= 60);
        let capped = PackOptions {
            rotations: false,
            max_height: Some(40),
            ..PackOptions::width(64)
        };
        assert!(matches!(pack(&shapes, &capped), Err(PackError::TooTall { .. })));
    }

    #[test]
    fn shapes_nest_inside_holes() {
        // A square ring (40×40, 8 thick) and a 16×16 block that fits in its hole.
        let ring: Vec<bool> = (0..40u32)
            .flat_map(|y| (0..40u32).map(move |x| !(8..32).contains(&x) || !(8..32).contains(&y)))
            .collect();
        let shapes = [Shape::from_bits(40, 40, &ring), Shape::rect(16, 16)];
        let opts = PackOptions {
            fill_holes: HoleFill::Always,
            nest_in_holes: true,
            ..PackOptions::width(40)
        };
        let p = pack(&shapes, &opts).unwrap();
        assert_eq!(p.height, 40, "block not nested: {:?}", p.placements);
        let flat = pack(&shapes, &PackOptions::width(40)).unwrap();
        assert!(flat.height > 40);
        let mut used = std::collections::HashSet::new();
        for (m, pl) in shapes.iter().zip(&p.placements) {
            for c in cells(m, pl) {
                assert!(used.insert(c), "overlap at {c:?}");
            }
        }
    }

    #[test]
    fn downsample_matches_cell_raster() {
        let mut seed = 0x51ed_270bu32;
        let mut rnd = |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % n
        };
        for case in 0..30 {
            let (w, h) = (5 + rnd(40), 5 + rnd(40));
            let mut bits = vec![false; (w * h) as usize];
            for _ in 0..4 {
                let (x0, y0) = (rnd(w), rnd(h));
                let (x1, y1) = (x0 + 1 + rnd(w - x0), y0 + 1 + rnd(h - y0));
                for y in y0..y1 {
                    for x in x0..x1 {
                        bits[(y * w + x) as usize] = (x + 2 * y) % 7 != 0 || case % 2 == 0;
                    }
                }
            }
            for b in [2u32, 3, 4, 8] {
                let (cw, ch) = (w.div_ceil(b), h.div_ceil(b));
                let cell: Vec<bool> = (0..cw * ch)
                    .map(|i| {
                        let (cx, cy) = (i % cw, i / cw);
                        (cy * b..((cy + 1) * b).min(h))
                            .any(|y| (cx * b..((cx + 1) * b).min(w)).any(|x| bits[(y * w + x) as usize]))
                    })
                    .collect();
                let want = ChartMask::from_bits(cw, ch, &cell);
                let got = ChartMask::from_bits(w, h, &bits).downsample(b);
                assert_eq!((got.w, got.h), (want.w, want.h));
                assert_eq!(got.col, want.col, "case {case} block {b} columns");
                assert_eq!(got.row, want.row, "case {case} block {b} rows");
                assert_eq!(got.col_gaps, want.col_gaps, "case {case} block {b} column gaps");
                assert_eq!(got.row_gaps, want.row_gaps, "case {case} block {b} row gaps");
                assert_eq!(got.area, want.area);
            }
        }
    }

    /// With `block`, no grid cell holds pixels of two shapes, and points mapped with each
    /// shape's own frame (as callers do) still land on its pixels.
    #[test]
    fn block_packing_gives_every_shape_whole_cells() {
        let mut seed = 0x2545_f491u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 10_000) as f32 / 10_000.0
        };
        let shapes: Vec<ChartMask> = (0..20)
            .map(|i| {
                let (w, h) = (9 + i % 13, 7 + (i * 5) % 17);
                let tris: Vec<[Vec2; 3]> = (0..3)
                    .map(|_| [0; 3].map(|_| Vec2::new(1.0 + rnd() * (w - 2) as f32, 1.0 + rnd() * (h - 2) as f32)))
                    .collect();
                ChartMask::from_triangles(&tris, w, h, 1)
            })
            .collect();
        for b in [2u32, 4, 8] {
            for fill in [HoleFill::Never, HoleFill::Always] {
                let opts = PackOptions::width(100).with_block(b).with_fill_holes(fill);
                let p = pack(&shapes, &opts).unwrap();
                assert_eq!(p.height % b, 0);
                let mut owner = std::collections::HashMap::new();
                for (i, (m, pl)) in shapes.iter().zip(&p.placements).enumerate() {
                    for (x, y) in cells(m, pl) {
                        assert!(x < 100 && y < p.height, "outside at {x},{y}");
                        let o = *owner.entry((x / b, y / b)).or_insert(i);
                        assert_eq!(o, i, "block {b}: cell ({}, {}) shared", x / b, y / b);
                    }
                    // A covered pixel's centre maps (with the shape's own frame) onto a pixel
                    // the placement covers.
                    let (fx, fy) = (0..m.w).find_map(|x| m.col[x as usize].map(|(lo, _)| (x, lo))).unwrap();
                    let q = rotate_point(Vec2::new(fx as f32 + 0.5, fy as f32 + 0.5), m.w, m.h, pl.rotation);
                    let at = (pl.x + q.x.floor() as u32, pl.y + q.y.floor() as u32);
                    assert!(cells(m, pl).contains(&at), "block {b}: point maps to {at:?}");
                }
            }
        }
        let capped = PackOptions::width(100).with_block(4).with_max_height(8);
        assert!(matches!(
            pack(&shapes, &capped),
            Err(PackError::TooTall { max_height: 8, .. })
        ));
    }
}
