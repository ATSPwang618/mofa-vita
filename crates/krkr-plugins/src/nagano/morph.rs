//! Resolve triangle coverage into ordered row spans. The shader binary-searches
//! spans instead of testing as many as 256 triangles for every output pixel.
use super::*;
use std::collections::BTreeSet;
pub(super) fn prepare(
    e: &Effect,
    r: f64,
    p: &mut [u32; 16],
    budget: &Budget,
) -> Result<Arc<Bytes>, String> {
    let count = e.geometry.len() / 12;
    let stride = 1 + (count * 2 + 1) * 2;
    let base = e.size.height as usize * stride;
    p[3] = stride as u32;
    p[4] = base as u32;
    let triangles: Vec<([i32; 6], &[i32; 12])> = e
        .geometry
        .as_chunks::<12>()
        .0
        .iter()
        .map(|g| {
            let current = std::array::from_fn(|i| {
                (g[i] as f64 + (g[i + 6] as f64 - g[i] as f64) * r).round() as i32
            });
            (current, g)
        })
        .collect();
    table(base + count * 18, budget, |out| {
        for (i, (current, g)) in triangles.iter().enumerate() {
            for (j, &v) in current.iter().chain(g.iter()).enumerate() {
                put(out, base + i * 18 + j, v);
            }
        }
        let mut events = Vec::with_capacity(count * 2);
        let mut active = BTreeSet::new();
        for y in 0..e.size.height as i32 {
            events.clear();
            active.clear();
            for (i, (c, _)) in triangles.iter().enumerate() {
                let cross = (c[2] as f64 - c[0] as f64) * (c[5] as f64 - c[1] as f64)
                    - (c[4] as f64 - c[0] as f64) * (c[3] as f64 - c[1] as f64);
                if cross == 0. {
                    continue;
                }
                let mut lo = f64::INFINITY;
                let mut hi = f64::NEG_INFINITY;
                for edge in 0..3 {
                    let next = (edge + 1) % 3;
                    let (x0, y0, x1, y1) = (
                        c[edge * 2] as f64,
                        c[edge * 2 + 1] as f64,
                        c[next * 2] as f64,
                        c[next * 2 + 1] as f64,
                    );
                    let y = y as f64;
                    if y < y0.min(y1) || y > y0.max(y1) {
                        continue;
                    }
                    if y0 == y1 {
                        lo = lo.min(x0.min(x1));
                        hi = hi.max(x0.max(x1));
                    } else {
                        let x = x0 + (x1 - x0) * (y - y0) / (y1 - y0);
                        lo = lo.min(x);
                        hi = hi.max(x);
                    }
                }
                let left = (lo.ceil() as i64).clamp(0, i64::from(e.size.width)) as i32;
                let right = (hi.floor() as i64)
                    .saturating_add(1)
                    .clamp(0, i64::from(e.size.width)) as i32;
                if left < right {
                    events.push((left, i, true));
                    events.push((right, i, false));
                }
            }
            events.sort_unstable();
            let offset = y as usize * stride;
            let mut start = 0;
            let mut num = 0;
            let mut event = 0;
            while event < events.len() {
                let x = events[event].0;
                if x > start {
                    put(out, offset + 1 + num * 2, x);
                    put(
                        out,
                        offset + 2 + num * 2,
                        active.last().map_or(-1, |&i| i as i32),
                    );
                    num += 1;
                }
                while event < events.len() && events[event].0 == x {
                    let (_, id, enter) = events[event];
                    if enter {
                        active.insert(id);
                    } else {
                        active.remove(&id);
                    }
                    event += 1;
                }
                start = x;
            }
            if start < e.size.width as i32 {
                put(out, offset + 1 + num * 2, e.size.width as i32);
                put(out, offset + 2 + num * 2, -1);
                num += 1;
            }
            put(out, offset, num as i32);
        }
    })
}
