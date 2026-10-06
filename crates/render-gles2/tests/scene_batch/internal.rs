use super::*;

#[test]
fn intervals_cover_float_sampling_and_crossings_bracket_tile_edges() {
    let mut seed = 0x97a8_152cu32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for case in 0..2000 {
        let step = [1., 0.5, 2., 137. / 121., 1024. / 960., 1.73][case % 6];
        let offset = (next() as i32 % 32768) as f32 / 7.;
        let scale = [1., 0.5, 2., 1024. / 963., 32. / 71.][case % 5];
        let axis = Axis {
            display_limit: None,
            step: f64::from(step),
            offset: f64::from(offset),
            scale: f64::from(scale),
        };
        let start = (next() as i32 % 16384) as i64;
        let end = start + 1024;
        for pixel in (start..end).step_by(13).chain([start, end - 1]) {
            let range = axis.interval(pixel);
            for q in [
                step * pixel as f32 + offset,
                step.mul_add(pixel as f32, offset),
            ] {
                let nearest = (q + 0.5).floor();
                for stored in [
                    ((nearest + 0.5) * scale).floor(),
                    nearest.mul_add(scale, 0.5 * scale).floor(),
                ] {
                    assert!(
                        range[0] <= f64::from(stored) && f64::from(stored) <= range[1],
                        "{case}: {pixel}, {stored}, {range:?}"
                    );
                }
            }
        }
        for edge in [-1024, 0, 64, 1024, 8192] {
            for bound in 0..2 {
                let cross = axis.crossing(start, end, edge, bound);
                assert!(cross == start || axis.interval(cross - 1)[bound] < edge as f64);
                assert!(cross == end || axis.interval(cross)[bound] >= edge as f64);
            }
        }
    }
}
