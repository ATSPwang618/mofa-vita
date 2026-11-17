#[derive(Default)]
pub(super) struct BgmFocus {
    slot: Option<usize>,
    since: u64,
    seen: u64,
    finished: bool,
}

impl BgmFocus {
    pub(super) fn ready(&mut self, slot: Option<usize>, frame: u64) -> bool {
        const DWELL_FRAMES: u64 = 12;
        if self.slot != slot {
            self.slot = slot;
            self.since = frame;
            self.finished = false;
        }
        self.seen = frame;
        slot.is_some() && !self.finished && frame.saturating_sub(self.since) >= DWELL_FRAMES
    }

    pub(super) fn finish(&mut self) {
        self.finished = true;
    }

    pub(super) fn expire(&mut self, frame: u64) -> bool {
        if self.slot.is_some() && frame.saturating_sub(self.seen) > 1 {
            *self = Self::default();
            return true;
        }
        false
    }
}

pub(crate) fn promote<T>(queue: &mut [T], preferred: usize, slot: impl Fn(&T) -> usize) {
    if let Some(index) = queue.iter().position(|entry| slot(entry) == preferred) {
        queue[..=index].rotate_right(1);
    }
}

pub(crate) fn maintain_priority<T>(
    queue: &mut [T], focus: Option<(usize, u64)>, frame: u64, slot: impl Fn(&T) -> usize,
) {
    const RETRY_FRAMES: u64 = 120;
    if queue.len() < 2 || frame % RETRY_FRAMES != 0 {
        return;
    }
    if let Some((preferred, since)) = focus
        && frame.saturating_sub(since) < RETRY_FRAMES
        && queue.iter().any(|entry| slot(entry) == preferred)
    {
        promote(queue, preferred, slot);
    } else {
        queue.rotate_left(1);
    }
}

pub(crate) fn black_material(
    blend: u8, mode: Option<i32>, shade: u8, fill: Option<[f32; 3]>, tint: Option<[f32; 3]>,
) -> bool {
    matches!(blend, 0 | 5)
        && mode.is_none_or(|value| matches!(value, -1 | 0 | 26 | 29))
        && matches!(shade, 0 | 3)
        && fill.is_none_or(|color| color == [0.0; 3])
        && tint.is_none_or(|value| value.into_iter().all(f32::is_finite))
}

pub(crate) fn fullscreen_crop(
    pos: [[f32; 2]; 4], uv: [[f32; 2]; 4], width: f32, height: f32,
) -> Option<[[f32; 2]; 4]> {
    let [[x0, y0], [right, top], [x1, y1], [left, bottom]] = pos;
    if !(x0 <= 0.0 && y0 <= 0.0 && x1 >= width && y1 >= height
        && right == x1 && top == y0 && left == x0 && bottom == y1) {
        return None;
    }
    let [[u0, v0], [ur, vt], [u1, v1], [ul, vb]] = uv;
    if !(ur == u1 && vt == v0 && ul == u0 && vb == v1) {
        return None;
    }
    let u = |x: f32| u0 + (u1 - u0) * ((x - x0) / (x1 - x0));
    let v = |y: f32| v0 + (v1 - v0) * ((y - y0) / (y1 - y0));
    Some([[u(0.0), v(0.0)], [u(width), v(0.0)], [u(width), v(height)], [u(0.0), v(height)]])
}
