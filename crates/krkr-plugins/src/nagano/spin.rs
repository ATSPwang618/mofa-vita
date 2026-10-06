//! Column projection from extNagano 10017e00..10018790. The rational form
//! also gives a defined limit for the DLL's uninitialised static-plane terms.
use super::*;
// Deliberately retain the original DLL's PI literal at sampling boundaries.
const HALF_PI: f64 = 1.5707963179489661;
fn coefficients(kind: i32, second: bool, r: f64, w: usize) -> [f64; 4] {
    let half = (w / 2) as f64;
    let width = w as f64;
    let distance = width * 2.;
    if kind == -1 {
        return [-half * distance, distance, distance, 0.];
    }
    let angle = match (second, kind) {
        (false, 0) => -HALF_PI - r,
        (false, 4) => r,
        (false, 5) => -r,
        (false, 6) => 2. * HALF_PI - r,
        (false, 7) => r + 2. * HALF_PI,
        (false, _) => r - HALF_PI,
        (true, 1) => -r,
        (true, 4) => HALF_PI - r,
        (true, 5) => r - HALF_PI,
        (true, 6) => r + HALF_PI,
        (true, 7) => -HALF_PI - r,
        (true, _) => r + 2. * HALF_PI,
    };
    match kind {
        4 | 5 => [
            -half * distance,
            angle.cos() * distance,
            distance,
            angle.sin(),
        ],
        6 | 7 => [
            (angle.cos() + 0.5) * width * distance,
            -angle.cos() * distance,
            angle.sin() * width + distance,
            -angle.sin(),
        ],
        _ => {
            let low = angle - HALF_PI * 0.5;
            let high = angle + HALF_PI * 0.5;
            let root = std::f64::consts::SQRT_2;
            [
                low.cos() * width / root * distance,
                (high.cos() - low.cos()) / root * distance,
                low.sin() * width / root + half + distance,
                (high.sin() - low.sin()) / root,
            ]
        }
    }
}
pub(super) fn prepare(
    effect: &Effect,
    t: u64,
    d: u64,
    budget: &Budget,
) -> Result<Arc<Bytes>, String> {
    let w = effect.size.width as usize;
    let angle = t.max(1) as f64 * HALF_PI / d as f64;
    let c = [
        coefficients(effect.values[0] as i32, false, angle, w),
        coefficients(effect.values[1] as i32, true, angle, w),
    ];
    table(w * 3, budget, |out| {
        for x in 0..w {
            let centered = x as f64 - (w / 2) as f64;
            let samples = c.map(|[a, b, c, e]| {
                let q = (c * centered - a) / (b - e * centered);
                let sx = q as i32;
                let slope = ((q * e + c) / (w as f64 * 2.) * 256.) as i32;
                (q.is_finite() && sx >= 0 && sx < w as i32, sx, slope)
            });
            let which = match (samples[0].0, samples[1].0) {
                (false, false) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (true, true) => {
                    if samples[1].2 < samples[0].2 {
                        2
                    } else {
                        1
                    }
                }
            };
            let (_, sx, slope) = samples[if which == 2 { 1 } else { 0 }];
            put(out, x * 3, sx);
            put(out, x * 3 + 1, slope);
            put(out, x * 3 + 2, which);
        }
    })
}
