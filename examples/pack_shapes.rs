//! Packs a mix of rectangles and irregular shapes and prints the atlas as ASCII.
//!
//! `cargo run -p snugly --example pack_shapes`

use snugly::{pack, pack_rects_strip, rotate_point, ChartMask, HoleFill, PackOptions};

fn main() {
    // 1. Plain rectangles: sprite sheet / impostor frames, 1 px padding, rotations allowed.
    let sizes = [
        (12, 5),
        (7, 7),
        (3, 14),
        (9, 4),
        (5, 5),
        (16, 3),
        (4, 9),
        (6, 2),
    ];
    let (rects, height) = pack_rects_strip(&sizes, 24, 1, true).expect("fits");
    println!("rectangles: 24 × {height}");
    for (i, p) in rects.iter().enumerate() {
        println!(
            "  #{i} {:?} at ({}, {}){}",
            sizes[i],
            p.x,
            p.y,
            if p.rotated { " rotated" } else { "" }
        );
    }

    // 2. Irregular shapes from a raster mask: an L, a ring and two blocks. The block can sit
    //    inside the ring when `nest_in_holes` is on.
    let l = mask(10, 10, |x, y| x < 4 || y < 4);
    let ring = mask(14, 14, |x, y| {
        !(4..10).contains(&x) || !(4..10).contains(&y)
    });
    let block = ChartMask::rect(6, 6);
    let bar = ChartMask::rect(10, 3);
    let shapes = vec![l, ring, block, bar];
    let opts = PackOptions {
        fill_holes: HoleFill::Always,
        nest_in_holes: true,
        ..PackOptions::width(24)
    };
    let packing = pack(&shapes, &opts).expect("fits");
    println!("\nshapes: 24 × {}", packing.height);
    let mut grid = vec![vec!['.'; 24]; packing.height as usize];
    for (i, (s, p)) in shapes.iter().zip(&packing.placements).enumerate() {
        let (w, h) = s.size();
        for y in 0..h {
            for x in 0..w {
                if covered(&shapes[i], x, y) {
                    // Shape pixel centre → rotated frame → atlas.
                    let c = rotate_point(
                        glam::Vec2::new(x as f32 + 0.5, y as f32 + 0.5),
                        w,
                        h,
                        p.rotation,
                    );
                    grid[(p.y as f32 + c.y) as usize][(p.x as f32 + c.x) as usize] =
                        (b'A' + i as u8) as char;
                }
            }
        }
    }
    for row in grid.iter().rev() {
        println!("  {}", row.iter().collect::<String>());
    }
}

fn mask(w: u32, h: u32, f: impl Fn(u32, u32) -> bool) -> ChartMask {
    let bits: Vec<bool> = (0..w * h).map(|i| f(i % w, i / w)).collect();
    ChartMask::from_bits(w, h, &bits)
}

/// Pixel test for the demo shapes (the packer itself only keeps outlines).
fn covered(s: &ChartMask, x: u32, y: u32) -> bool {
    match s.size() {
        (10, 10) => x < 4 || y < 4,
        (14, 14) => !(4..10).contains(&x) || !(4..10).contains(&y),
        _ => true,
    }
}
