//! MaxRects rectangle packing (Jylänki, "A Thousand Ways to Pack the Bin", 2010): the free
//! space is kept as the list of maximal empty rectangles, each rectangle goes where a scoring
//! rule likes it best, and the free list is split and pruned after each placement.
//!
//! One rule and one input order rarely win everywhere, so the public entry points try a fixed
//! set of (rule, order) pairs in parallel and keep the best result, picking between equal
//! results by the fixed order of the set: the output depends only on the input.

#[derive(Clone, Copy, Debug)]
struct R {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl R {
    fn contains(&self, o: &R) -> bool {
        o.x >= self.x
            && o.y >= self.y
            && o.x + o.w <= self.x + self.w
            && o.y + o.h <= self.y + self.h
    }
}

/// Where a rectangle goes in a free rectangle; lower scores are better.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rule {
    /// Smallest leftover on the shorter side, then on the longer side.
    ShortSide,
    /// Smallest leftover on the longer side, then on the shorter side.
    LongSide,
    /// Smallest leftover area, then shorter side.
    Area,
    /// Lowest top edge, then leftmost.
    BottomLeft,
    /// Contact with placed rectangles and atlas borders, most first.
    Contact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Order {
    Area,
    Perimeter,
    MaxSide,
    Height,
    Width,
}

pub(crate) const RULES: [Rule; 5] = [
    Rule::BottomLeft,
    Rule::ShortSide,
    Rule::Contact,
    Rule::Area,
    Rule::LongSide,
];
pub(crate) const ORDERS: [Order; 5] = [
    Order::MaxSide,
    Order::Area,
    Order::Perimeter,
    Order::Height,
    Order::Width,
];

fn order(sizes: &[(u32, u32)], o: Order) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..sizes.len()).collect();
    let key = |i: usize| -> (u64, u64) {
        let (w, h) = (sizes[i].0 as u64, sizes[i].1 as u64);
        match o {
            Order::Area => (w * h, w.max(h)),
            Order::Perimeter => (w + h, w * h),
            Order::MaxSide => (w.max(h), w * h),
            Order::Height => (h, w),
            Order::Width => (w, h),
        }
    };
    // Stable sort: equal keys keep input order.
    idx.sort_by_key(|&i| std::cmp::Reverse(key(i)));
    idx
}

/// Common interval length of [a0, a1) and [b0, b1).
fn overlap(a0: u32, a1: u32, b0: u32, b1: u32) -> u32 {
    a1.min(b1).saturating_sub(a0.max(b0))
}

struct Bin {
    width: u32,
    height: u32,
    free: Vec<R>,
    used: Vec<R>,
}

impl Bin {
    fn new(width: u32, height: u32) -> Bin {
        Bin {
            width,
            height,
            free: vec![R {
                x: 0,
                y: 0,
                w: width,
                h: height,
            }],
            used: Vec::new(),
        }
    }

    fn contact(&self, x: u32, y: u32, w: u32, h: u32) -> u64 {
        let mut s = 0u64;
        if x == 0 || x + w == self.width {
            s += h as u64;
        }
        if y == 0 || y + h == self.height {
            s += w as u64;
        }
        for u in &self.used {
            if u.x == x + w || u.x + u.w == x {
                s += overlap(u.y, u.y + u.h, y, y + h) as u64;
            }
            if u.y == y + h || u.y + u.h == y {
                s += overlap(u.x, u.x + u.w, x, x + w) as u64;
            }
        }
        s
    }

    fn score(&self, f: &R, w: u32, h: u32, rule: Rule) -> (u64, u64) {
        let (dw, dh) = ((f.w - w) as u64, (f.h - h) as u64);
        match rule {
            Rule::ShortSide => (dw.min(dh), dw.max(dh)),
            Rule::LongSide => (dw.max(dh), dw.min(dh)),
            Rule::Area => (f.w as u64 * f.h as u64 - w as u64 * h as u64, dw.min(dh)),
            Rule::BottomLeft => ((f.y + h) as u64, f.x as u64),
            Rule::Contact => (u64::MAX - self.contact(f.x, f.y, w, h), (f.y + h) as u64),
        }
    }

    /// Best spot for a `w × h` rectangle: (x, y, rotated).
    fn find(&self, w: u32, h: u32, rot: bool, rule: Rule) -> Option<(u32, u32, bool)> {
        let mut best: Option<((u64, u64), u32, u32, bool)> = None;
        for f in &self.free {
            for (cw, ch, r) in [(w, h, false), (h, w, true)] {
                if r && (!rot || w == h) {
                    continue;
                }
                if cw <= f.w && ch <= f.h {
                    let s = self.score(f, cw, ch, rule);
                    if best.is_none_or(|b| s < b.0) {
                        best = Some((s, f.x, f.y, r));
                    }
                }
            }
        }
        best.map(|b| (b.1, b.2, b.3))
    }

    fn place(&mut self, r: R) {
        let mut kept = Vec::with_capacity(self.free.len() + 4);
        let mut fresh: Vec<R> = Vec::new();
        for f in &self.free {
            let hit = r.x < f.x + f.w && f.x < r.x + r.w && r.y < f.y + f.h && f.y < r.y + r.h;
            if !hit {
                kept.push(*f);
                continue;
            }
            if r.x > f.x {
                fresh.push(R {
                    x: f.x,
                    y: f.y,
                    w: r.x - f.x,
                    h: f.h,
                });
            }
            if r.x + r.w < f.x + f.w {
                fresh.push(R {
                    x: r.x + r.w,
                    y: f.y,
                    w: f.x + f.w - r.x - r.w,
                    h: f.h,
                });
            }
            if r.y > f.y {
                fresh.push(R {
                    x: f.x,
                    y: f.y,
                    w: f.w,
                    h: r.y - f.y,
                });
            }
            if r.y + r.h < f.y + f.h {
                fresh.push(R {
                    x: f.x,
                    y: r.y + r.h,
                    w: f.w,
                    h: f.y + f.h - r.y - r.h,
                });
            }
        }
        // Pieces of a split rectangle can't contain a rectangle that was kept (the kept list was
        // already free of nested rectangles), so only the new pieces need pruning.
        let mut dead = vec![false; fresh.len()];
        for i in 0..fresh.len() {
            if kept.iter().any(|k| k.contains(&fresh[i])) {
                dead[i] = true;
                continue;
            }
            for j in 0..fresh.len() {
                if i != j && !dead[j] && fresh[j].contains(&fresh[i]) {
                    // Equal pieces: keep the first.
                    let same = fresh[i].contains(&fresh[j]);
                    if !same || j < i {
                        dead[i] = true;
                        break;
                    }
                }
            }
        }
        kept.extend(fresh.into_iter().zip(dead).filter(|p| !p.1).map(|p| p.0));
        self.free = kept;
        self.used.push(r);
    }
}

/// Places padded sizes into a `width × height` bin with one (rule, order) pair. Returns
/// (x, y, rotated) per input and the height used, or None if something doesn't fit.
pub(crate) fn run(
    sizes: &[(u32, u32)],
    width: u32,
    height: u32,
    rot: bool,
    rule: Rule,
    ord: Order,
) -> Option<(Vec<(u32, u32, bool)>, u32)> {
    let mut bin = Bin::new(width, height);
    let mut out = vec![(0, 0, false); sizes.len()];
    let mut top = 0;
    for i in order(sizes, ord) {
        let (w, h) = sizes[i];
        let (x, y, r) = bin.find(w, h, rot, rule)?;
        let (pw, ph) = if r { (h, w) } else { (w, h) };
        bin.place(R { x, y, w: pw, h: ph });
        top = top.max(y + ph);
        out[i] = (x, y, r);
    }
    Some((out, top))
}

/// Contact scoring is quadratic in the rectangle count; above this it is skipped.
const CONTACT_MAX: usize = 400;

/// (rule, order, rotate) triples to try. With rotations allowed the unrotated variants run
/// too, so allowing rotations never gives a worse packing.
fn configs(n: usize, rot: bool) -> Vec<(Rule, Order, bool)> {
    let rots: &[bool] = if rot { &[true, false] } else { &[false] };
    rots.iter()
        .flat_map(|&t| {
            RULES
                .iter()
                .filter(|&&r| r != Rule::Contact || n <= CONTACT_MAX)
                .flat_map(move |&r| ORDERS.iter().map(move |&o| (r, o, t)))
        })
        .collect()
}

/// Best of all configurations in a fixed bin: the one using the least height, earliest in the
/// fixed configuration order on ties.
pub(crate) fn best_fixed(
    sizes: &[(u32, u32)],
    width: u32,
    height: u32,
    rot: bool,
) -> Option<(Vec<(u32, u32, bool)>, u32)> {
    crate::par::map(&configs(sizes.len(), rot), 1, |&(r, o, t)| {
        run(sizes, width, height, t, r, o)
    })
    .into_iter()
    .flatten()
    .min_by_key(|p| p.1)
}

/// Lowest strip: binary search on the bin height between `lo` and `hi` (`hi` must be known to
/// fit some packing), each probe trying every configuration in parallel.
pub(crate) fn best_strip(
    sizes: &[(u32, u32)],
    width: u32,
    lo: u32,
    hi: u32,
    rot: bool,
) -> Option<(Vec<(u32, u32, bool)>, u32)> {
    let cfg = configs(sizes.len(), rot);
    let probe = |h: u32| -> Option<(Vec<(u32, u32, bool)>, u32)> {
        crate::par::map(&cfg, 1, |&(r, o, t)| run(sizes, width, h, t, r, o))
            .into_iter()
            .flatten()
            .min_by_key(|p| p.1)
    };
    let mut best = probe(hi)?;
    let (mut lo, mut hi) = (lo.min(best.1), best.1);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        match probe(mid) {
            Some(p) => {
                hi = p.1;
                best = p;
            }
            None => lo = mid + 1,
        }
    }
    Some(best)
}
