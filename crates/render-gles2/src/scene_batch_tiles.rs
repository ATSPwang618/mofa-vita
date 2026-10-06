//! Partition a sampled stack into regions with one source tile per
//! layer. Uncertain floating-point boundary pixels keep the ordinary draw path.
use crate::{Result, scene::raster::Raster, scene_batch::Layer};
use krkr_protocol::graphics::Rect;

const MAX_PARTS: usize = 128;

pub(crate) struct Part {
    pub area: Rect,
    pub tiles: Option<[usize; 4]>,
}

#[derive(Clone, Copy)]
struct Axis {
    step: f64,
    offset: f64,
    scale: f64,
    display_limit: Option<f64>,
}
impl Axis {
    fn interval(self, pixel: i64) -> [f64; 2] {
        let product = self.step * pixel as f64;
        let q = product + self.offset;
        // Include input conversion and fused/unfused arithmetic. The map is
        // monotone, so endpoints bound both nearest and bilinear footprints.
        let error = (product.abs() + self.offset.abs() + 1.) * 8. * f64::from(f32::EPSILON);
        if let Some(limit) = self.display_limit {
            return [
                (q - error).clamp(0., limit).floor(),
                ((q + error).clamp(0., limit).floor() + 1.).min(limit),
            ];
        }
        let low = ((q - error + 0.5).floor() + 0.5) * self.scale;
        let high = ((q + error + 0.5).floor() + 0.5) * self.scale;
        let error = (low.abs().max(high.abs()) + 1.) * 8. * f64::from(f32::EPSILON);
        [(low - error).floor(), (high + error).floor()]
    }
    fn crossing(self, start: i64, end: i64, edge: i64, bound: usize) -> i64 {
        let (mut lo, mut hi) = (start, end);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.interval(mid)[bound] >= edge as f64 {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        lo
    }
}

pub(crate) fn partition(
    layers: &[Layer],
    area: Rect,
    raster: Raster,
    origin: (i64, i64),
    display: bool,
) -> Result<Option<smallvec::SmallVec<[Part; 1]>>> {
    if layers.iter().all(|l| {
        l.image.main.as_ref().is_some_and(|p| {
            p.tiles.len() == 1 && (!display || p.tiles[0].rectangle == p.size.rect())
        })
    }) {
        return Ok(Some(smallvec::smallvec![Part {
            area,
            tiles: Some([0; 4]),
        }]));
    }
    let spans = [
        (
            i64::from(area.left),
            i64::from(area.left) + i64::from(area.width),
        ),
        (
            i64::from(area.top),
            i64::from(area.top) + i64::from(area.height),
        ),
    ];
    let mut cuts: [smallvec::SmallVec<[i64; 16]>; 2] = [
        smallvec::smallvec![spans[0].0, spans[0].1],
        smallvec::smallvec![spans[1].0, spans[1].1],
    ];
    let mut mappings = smallvec::SmallVec::<[_; 4]>::with_capacity(layers.len());
    let mut baseline = 0;
    for layer in layers {
        let plane = layer.image.plane(false)?;
        if plane.tiles.len() > 64 {
            return Ok(None);
        }
        let map = raster.mapping(origin, layer.origin);
        let map = if display {
            crate::scene::raster::stored_mapping(&layer.image, map)?
        } else {
            map
        };
        let axes = [
            Axis {
                display_limit: display.then_some(f64::from(plane.size.width - 1)),
                step: f64::from(map[0]),
                offset: f64::from(map[2]),
                scale: f64::from(plane.size.width as f32 / layer.image.size.width as f32),
            },
            Axis {
                display_limit: display.then_some(f64::from(plane.size.height - 1)),
                step: f64::from(map[4]),
                offset: f64::from(map[5]),
                scale: f64::from(plane.size.height as f32 / layer.image.size.height as f32),
            },
        ];
        if axes.iter().any(|a| {
            !a.step.is_finite()
                || a.step <= 0.
                || !a.offset.is_finite()
                || !a.scale.is_finite()
                || a.scale <= 0.
        }) {
            return Ok(None);
        }
        let Some(active) = area.intersection(layer.coverage) else {
            return Ok(None);
        };
        let bounds = bounds(active, axes);
        baseline += plane
            .tiles
            .iter()
            .filter(|t| overlaps(t.rectangle, bounds))
            .count();
        if plane.tiles.len() > 1 || display {
            for axis in 0..2 {
                let mut edges =
                    smallvec::SmallVec::<[i64; 16]>::with_capacity(plane.tiles.len() * 2);
                for tile in &plane.tiles {
                    let (start, length) = if axis == 0 {
                        (tile.rectangle.left, tile.rectangle.width)
                    } else {
                        (tile.rectangle.top, tile.rectangle.height)
                    };
                    edges.extend([i64::from(start), i64::from(start) + i64::from(length)]);
                }
                edges.sort_unstable();
                edges.dedup();
                for edge in edges {
                    for bound in 0..2 {
                        cuts[axis].push(axes[axis].crossing(
                            spans[axis].0,
                            spans[axis].1,
                            edge,
                            bound,
                        ));
                    }
                }
            }
        }
        mappings.push(axes);
    }
    for cuts in &mut cuts {
        cuts.sort_unstable();
        cuts.dedup();
    }
    let count = (cuts[0].len() - 1) * (cuts[1].len() - 1);
    // Filtered tile edges have a seam band between their two interiors.
    // Account for those extra cuts, but bound setup before writing pixels.
    if count > MAX_PARTS || count > baseline * if display { 4 } else { 1 } {
        return Ok(None);
    }
    let mut parts = smallvec::SmallVec::<[Part; 1]>::with_capacity(count);
    let mut cost = 0;
    let mut fused = 0u64;
    for y in cuts[1].windows(2) {
        for x in cuts[0].windows(2) {
            let area = Rect {
                left: x[0] as i32,
                top: y[0] as i32,
                width: (x[1] - x[0]) as u32,
                height: (y[1] - y[0]) as u32,
            };
            let mut selected = [0; 4];
            let mut safe = true;
            let mut separate = 0;
            for (i, layer) in layers.iter().enumerate() {
                let Some(active) = area.intersection(layer.coverage) else {
                    continue;
                };
                let plane = layer.image.plane(false)?;
                let bounds = bounds(active, mappings[i]);
                separate += plane
                    .tiles
                    .iter()
                    .filter(|t| overlaps(t.rectangle, bounds))
                    .count();
                if plane.tiles.len() > 1 || display {
                    if let Some(index) = plane
                        .tiles
                        .iter()
                        .position(|t| contains(t.rectangle, bounds))
                    {
                        selected[i] = index;
                    } else {
                        safe = false;
                    }
                }
            }
            if safe {
                cost += 1;
                fused += u64::from(area.width) * u64::from(area.height);
            } else {
                cost += separate;
            }
            let part = Part {
                area,
                tiles: safe.then_some(selected),
            };
            if !safe
                && let Some(previous) = parts.last_mut().filter(|p: &&mut Part| {
                    p.tiles.is_none()
                        && p.area.top == area.top
                        && p.area.height == area.height
                        && i64::from(p.area.left) + i64::from(p.area.width) == i64::from(area.left)
                })
            {
                previous.area.width += area.width;
            } else {
                parts.push(part);
            }
        }
    }
    // Require at least 75% of pixels to avoid intermediate blends. A filtered
    // reference also splits/gathers around seams, so permit more setup than
    // the nearest path's single draw per source tile. Both stay bounded.
    if cost * 2 > baseline * if display { 12 } else { 3 }
        || fused * 4 < u64::from(area.width) * u64::from(area.height) * 3
    {
        return Ok(None);
    }
    // Crossing another layer's row boundary must not split the same uncertain
    // vertical seam into repeated per-layer draws. Rectangles remain disjoint.
    for i in 0..parts.len() {
        if i >= parts.len() {
            break;
        }
        if parts[i].tiles.is_some() {
            continue;
        }
        let mut j = i + 1;
        while j < parts.len() {
            let a = parts[i].area;
            let b = parts[j].area;
            if parts[j].tiles.is_none()
                && a.left == b.left
                && a.width == b.width
                && i64::from(a.top) + i64::from(a.height) == i64::from(b.top)
            {
                parts[i].area.height += b.height;
                parts.remove(j);
            } else {
                j += 1;
            }
        }
    }
    Ok(Some(parts))
}

fn bounds(area: Rect, axes: [Axis; 2]) -> [[f64; 2]; 2] {
    [
        [
            axes[0].interval(i64::from(area.left))[0],
            axes[0].interval(i64::from(area.left) + i64::from(area.width) - 1)[1],
        ],
        [
            axes[1].interval(i64::from(area.top))[0],
            axes[1].interval(i64::from(area.top) + i64::from(area.height) - 1)[1],
        ],
    ]
}
fn contains(rect: Rect, bounds: [[f64; 2]; 2]) -> bool {
    bounds[0][0] >= f64::from(rect.left)
        && bounds[0][1] < f64::from(rect.left) + f64::from(rect.width)
        && bounds[1][0] >= f64::from(rect.top)
        && bounds[1][1] < f64::from(rect.top) + f64::from(rect.height)
}
fn overlaps(rect: Rect, bounds: [[f64; 2]; 2]) -> bool {
    bounds[0][1] >= f64::from(rect.left)
        && bounds[0][0] < f64::from(rect.left) + f64::from(rect.width)
        && bounds[1][1] >= f64::from(rect.top)
        && bounds[1][0] < f64::from(rect.top) + f64::from(rect.height)
}

#[cfg(test)]
#[path = "../tests/scene_batch/internal.rs"]
mod tests;
