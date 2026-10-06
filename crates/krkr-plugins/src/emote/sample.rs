//! Frame selection and transform interpolation used by both player adapters.
use super::model::{Content, Layer, Motion};

pub(super) fn default_mesh() -> [f64; 32] {
    std::array::from_fn(|i| {
        [0., 0.333333, 0.666667, 1.][if i % 2 == 0 { (i / 2) % 4 } else { i / 8 }]
    })
}
#[derive(Clone, Copy)]
pub(super) struct Bounds {
    pub size: [f64; 2],
    pub origin: [f64; 2],
}
#[derive(Clone, Debug)]
pub(super) struct Sample {
    pub frame: usize,
    pub coord: [f64; 3],
    pub opacity: f64,
    pub angle: f64,
    pub slant: [f64; 2],
    pub zoom: [f64; 2],
    pub origin: [f64; 2],
    pub time_offset: f64,
    pub mesh: Option<[f64; 32]>,
}
fn coordinate(value: f64, axis: usize, bounds: Bounds) -> f64 {
    if value.is_nan() {
        -bounds.origin[axis]
    } else if value.is_infinite() {
        bounds.size[axis] - bounds.origin[axis]
    } else {
        value
    }
}
impl Sample {
    fn fixed(index: usize, c: &Content, bounds: Bounds) -> Self {
        Self {
            frame: index,
            coord: [
                coordinate(c.coord[0], 0, bounds),
                coordinate(c.coord[1], 1, bounds),
                c.coord[2],
            ],
            opacity: c.opacity,
            angle: c.angle,
            slant: c.slant,
            zoom: c.zoom,
            origin: c.origin,
            time_offset: c.time_offset,
            mesh: c.mesh,
        }
    }
}
impl Layer {
    pub fn sample(
        &self,
        mut tick: f32,
        motion: &Motion,
        old_motion: bool,
        parameter_tick: Option<f32>,
        bounds: Bounds,
    ) -> Option<Sample> {
        let frames = &self.frames;
        if old_motion && frames.len() > 1 {
            let previous = &frames[frames.len() - 2];
            let last = frames.last()?;
            if last.kind == 0 {
                if previous.kind == 2 && tick > motion.self_sync_time {
                    tick = motion.self_sync_time;
                }
                if frames.len() == 2 && frames[0].kind == 2 {
                    tick = frames[0].time as f32;
                }
                if previous.kind == 3 {
                    tick = tick.min(previous.time as f32);
                }
            }
        }
        if self.kind != 12 && self.parameter.is_some() {
            tick = parameter_tick?;
            if tick < 0. {
                return None;
            }
        }
        let count = if self.ordered_frames {
            frames.partition_point(|frame| frame.time <= tick as f64)
        } else {
            frames
                .iter()
                .take_while(|frame| frame.time <= tick as f64)
                .count()
        };
        let mut index = count.checked_sub(1)?;
        let mut hold = false;
        if frames[index].content.is_none() {
            if index + 1 != frames.len() {
                return None;
            }
            index = (0..index).rev().find(|&i| frames[i].content.is_some())?;
            hold = true;
        }
        let frame = &frames[index];
        let content = frame.content.as_ref()?;
        let mut result = Sample::fixed(index, content, bounds);
        let next = if hold { None } else { frames.get(index + 1) };
        if let Some(next) = next
            && let Some(target) = &next.content
            && (frame.kind != 2 || next.kind == 2)
        {
            // Preserve operation order and precision of the C++ interpolator.
            let lerp = |a: f64, b: f64| {
                a + (b - a) / (next.time - frame.time) * (tick as f64 - frame.time)
            };
            let destination = Sample::fixed(index + 1, target, bounds);
            for axis in 0..3 {
                result.coord[axis] = lerp(result.coord[axis], destination.coord[axis]);
            }
            result.opacity = lerp(content.opacity, target.opacity);
            result.angle = if target.angle < 180. && content.angle > 180. {
                lerp(content.angle - 360., target.angle)
            } else if target.angle > 180. && content.angle < 180. {
                lerp(content.angle, target.angle - 360.)
            } else {
                lerp(content.angle, target.angle)
            };
            for axis in 0..2 {
                result.slant[axis] = lerp(content.slant[axis], target.slant[axis]);
                result.zoom[axis] = lerp(content.zoom[axis], target.zoom[axis]);
                result.origin[axis] = lerp(content.origin[axis], target.origin[axis]);
            }
            result.time_offset = lerp(content.time_offset, target.time_offset);
            result.mesh = if content.mesh.is_some() || target.mesh.is_some() {
                let a = content.mesh.unwrap_or_else(default_mesh);
                let b = target.mesh.unwrap_or_else(default_mesh);
                Some(std::array::from_fn(|i| lerp(a[i], b[i])))
            } else {
                None
            };
        }
        Some(result)
    }
}
