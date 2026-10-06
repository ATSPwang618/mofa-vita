#[path = "../src/draw_bounds.rs"]
mod bounds;
use krkr_protocol::graphics::Rect;

#[test]
fn conservative_tile_geometry_contains_every_nearest_sample_after_rounding() {
    let mut random = 17u32;
    let mut next = || {
        random = random.wrapping_mul(1664525).wrapping_add(1013904223);
        (random >> 8) as f32 / 16777216.
    };
    for index in 0..1200 {
        let area = Rect {
            left: [0, -37, 17321][index % 3],
            top: [0, 11231, -51][index % 3],
            width: 37,
            height: 29,
        };
        let tile = Rect {
            left: [0, 32, 16384][index % 3],
            top: [0, 256, 1024][index % 3],
            width: 32,
            height: 16,
        };
        let scale = match index % 5 {
            0 => None,
            1 => Some([1., 1.]),
            2 => Some([0.9375, 0.5]),
            3 => Some([3.71, 1.97]),
            _ => Some([0.0137, 0.0023]),
        };
        let s = scale.unwrap_or([1., 1.]);
        let a = next() * 10. - 5.;
        let b = if index % 4 == 0 { 0. } else { next() * 6. - 3. };
        let c = if index % 4 == 0 { 0. } else { next() * 6. - 3. };
        let d = next() * 10. - 5.;
        let x = area.left as f32 + 18.;
        let y = area.top as f32 + 14.;
        let tx = (tile.left as f32 + 16.) / s[0] - a * x - b * y;
        let ty = (tile.top as f32 + 8.) / s[1] - c * x - d * y;
        let map = [a, b, tx, c, d, ty];
        let bounded = bounds::tile_area(area, map, scale, tile);
        for y in area.top..area.top + area.height as i32 {
            for x in area.left..area.left + area.width as i32 {
                for fused in [false, true] {
                    let dot = |a: f32, b: f32, t: f32| {
                        if fused {
                            a.mul_add(x as f32, b.mul_add(y as f32, t))
                        } else {
                            a * x as f32 + b * y as f32 + t
                        }
                    };
                    let mut q = [(dot(a, b, tx) + 0.5).floor(), (dot(c, d, ty) + 0.5).floor()];
                    if let Some(s) = scale {
                        q = [((q[0] + 0.5) * s[0]).floor(), ((q[1] + 0.5) * s[1]).floor()];
                    }
                    if q[0] >= tile.left as f32
                        && q[0] < (tile.left + tile.width as i32) as f32
                        && q[1] >= tile.top as f32
                        && q[1] < (tile.top + tile.height as i32) as f32
                    {
                        let pixel = Rect {
                            left: x,
                            top: y,
                            width: 1,
                            height: 1,
                        };
                        assert!(
                            bounded.is_some_and(|r| r.intersection(pixel) == Some(pixel)),
                            "dropped {x},{y}, map={map:?}, scale={scale:?}, bounds={bounded:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn singular_and_invalid_mappings_keep_the_original_coverage() {
    let area = Rect {
        left: -3,
        top: 5,
        width: 16,
        height: 8,
    };
    for map in [
        [0.; 6],
        [1., 2., 3., 2., 4., 5.],
        [f32::NAN, 0., 0., 0., 1., 0.],
        [f32::INFINITY, 0., 0., 0., 1., 0.],
    ] {
        assert_eq!(bounds::tile_area(area, map, None, area), Some(area));
    }
    for scale in [[0., 1.], [-1., 1.], [f32::NAN, 1.]] {
        assert_eq!(
            bounds::tile_area(area, [1., 0., 0., 0., 1., 0.], Some(scale), area),
            Some(area)
        );
    }
}
