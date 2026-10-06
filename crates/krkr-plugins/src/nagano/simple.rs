use super::*;
pub(super) fn prepare(
    effect: &Effect,
    t: u64,
    d: u64,
    p: &mut [u32; 16],
    budget: &Budget,
) -> Result<Arc<Bytes>, String> {
    let v = &effect.values;
    let w = effect.size.width as usize;
    let h = effect.size.height as usize;
    match effect.kind {
        Kind::Zoom => {
            let phase = f64::from(p[1]) / 255.;
            let scales = [
                1. / (1. + (v[0] / 100. - 1.) * phase),
                1. / (v[1] / 100. - (v[1] / 100. - 1.) * phase),
            ];
            // Two independent 1-D maps retain x87 truncation without a full
            // frame coordinate upload or precision-dependent shader division.
            table((w + h) * 2, budget, |out| {
                for (axis, n) in [(0, w), (w, h)] {
                    let center = n as f64 * 0.5;
                    for i in 0..n {
                        for (source, scale) in scales.into_iter().enumerate() {
                            let q = (center + (i as f64 - center) * scale) as i32;
                            put(out, (axis + i) * 2 + source, q.clamp(0, n as i32 - 1));
                        }
                    }
                }
            })
        }
        Kind::Scanline => {
            p[3] = (w as u128 * u128::from(t) / u128::from(d)) as u32;
            effect
                .table
                .get_or_init(|| table(1, budget, |_| {}))
                .clone()
        }
        Kind::Rgb => {
            let duration = i128::from(d);
            let max = v.iter().copied().fold(0., f64::max) as i128;
            let effective = ((255 - max) * duration / 255).max(1);
            for i in 0..4 {
                let delay = v[i] as i128 * duration / 255;
                p[4 + i] = ((i128::from(t) - delay) * 255 / effective).clamp(0, 255) as u32;
            }
            effect
                .table
                .get_or_init(|| table(1, budget, |_| {}))
                .clone()
        }
        Kind::Book => {
            let phase = ((w / 2) as f64 * (t as f64 / d as f64).powi(2)).floor() as i32;
            p[3] = phase as u32;
            p[4] = v[0] as u32;
            let q = ((((w as i64 / 2 - i64::from(phase)) * 65536 / w as i64) / 2) as f64).sqrt()
                as f32 as u32;
            p[5] = q;
            effect
                .table
                .get_or_init(|| table(1, budget, |_| {}))
                .clone()
        }
        Kind::Spin => spin::prepare(effect, t, d, budget),
        Kind::Wipe => wipe::prepare(effect, t, d, p, budget),
        _ => advanced::prepare(effect, t, d, p, budget),
    }
}
