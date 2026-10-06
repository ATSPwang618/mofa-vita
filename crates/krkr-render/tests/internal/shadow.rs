use super::*;

fn reference(glyph: &Glyph, level: i32, width: i32) -> Vec<u8> {
    let r = width.unsigned_abs() as i32;
    let w = glyph.size.width as i32 + 2 * r;
    let h = glyph.size.height as i32 + 2 * r;
    let maximum = if glyph.levels == 65 { 64 } else { 255 };
    if r == 0 {
        return glyph
            .mask
            .as_slice()
            .iter()
            .map(|&v| ((i64::from(v) * i64::from(level)) >> 8).clamp(0, maximum) as u8)
            .collect();
    }
    let distance = |x: i32, y: i32| {
        let a = x.abs().max(y.abs());
        let b = x.abs().min(y.abs());
        let t = b + (b >> 1);
        a - (a >> 5) - (a >> 7) + (t >> 2) + (t >> 6)
    };
    let sum: i64 = (-r..=r)
        .flat_map(|y| (-r..=r).map(move |x| (x, y)))
        .map(|(x, y)| (r - distance(x, y) + 1).max(0) as i64)
        .sum();
    let norm = (1 << 18) / sum.max(1);
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let mut sum = 0i64;
            for sy in 0..glyph.size.height as i32 {
                for sx in 0..glyph.size.width as i32 {
                    let dx = x - sx - r;
                    let dy = y - sy - r;
                    if dx.abs() > r || dy.abs() > r {
                        continue;
                    }
                    let d = distance(dx, dy);
                    if d > r {
                        continue;
                    }
                    let weight = (i64::from(r - d + 1) * norm * i64::from(level)) >> 8;
                    sum += (i64::from(
                        glyph.mask.as_slice()[(sy * glyph.size.width as i32 + sx) as usize],
                    ) * weight)
                        >> 18;
                }
            }
            sum.clamp(0, maximum) as u8
        })
        .collect()
}

#[test]
fn radial_shadow_matches_native_integer_convolution() {
    let mut system = System::default();
    for levels in [65, 256] {
        let mask = vec![
            0,
            1,
            4,
            16,
            32,
            64,
            if levels == 65 { 63 } else { 255 },
            0,
            8,
            24,
            48,
            1,
        ];
        let permit = system.reserve(mask.len()).unwrap();
        let glyph = Glyph {
            id: glyph_id(),
            size: Size {
                width: 4,
                height: 3,
            },
            origin: [7, 9],
            advance: [5, 0],
            levels,
            mask: Bytes::with_permit(mask, permit),
        };
        for width in [-3, 0, 1, 4] {
            for level in [i32::MIN, -1, 0, 1, 128, 255, 700, i32::MAX] {
                let got = blur(&glyph, level, width, &mut system, &AtomicBool::new(false)).unwrap();
                assert_eq!(
                    got.mask.as_slice(),
                    reference(&glyph, level, width),
                    "width={width} level={level} levels={levels}"
                );
                assert_eq!(got.advance, glyph.advance);
            }
        }
    }
}
