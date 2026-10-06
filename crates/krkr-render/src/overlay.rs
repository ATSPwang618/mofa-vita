//! Small host HUD. Only instantiated when enabled; sampling and text generation
//! happen once a second. Frame counts exclude redraws requested by the HUD.
use crate::Size;
use std::time::{Duration, Instant};

pub const SIZE: Size = Size {
    width: 240,
    height: 60,
};
pub const INTERVAL: Duration = Duration::from_secs(1);

pub struct Counter {
    started: Instant,
    frames: u32,
    pub next: Instant,
}
impl Counter {
    pub fn new(now: Instant) -> Self {
        Self {
            started: now,
            frames: 0,
            next: now,
        }
    }
    pub fn frame(&mut self) {
        self.frames += 1;
    }
    pub fn sample(&mut self, now: Instant) -> Option<f32> {
        if now < self.next {
            return None;
        }
        let elapsed = now.duration_since(self.started).as_secs_f32();
        let fps = if elapsed > 0.0 {
            self.frames as f32 / elapsed
        } else {
            0.0
        };
        self.started = now;
        self.next = now + INTERVAL;
        self.frames = 0;
        Some(fps)
    }
}

/// An original 5x7 bitmap alphabet avoids font discovery, outline rasterization
/// and an atlas in the game render path. The fixed texture is just 57.6 KB.
pub fn pixels(fps: f32, memory: Option<usize>, free: bool, graphics: usize) -> Vec<u8> {
    let memory = memory.map_or_else(
        || "--".into(),
        |bytes| format!("{:.1}", bytes as f64 / 1048576.0),
    );
    let lines = [
        format!("FPS {fps:.1}"),
        format!("RAM{} {memory} MIB", if free { " FREE" } else { "" }),
        format!("GPU {:.1} MIB", graphics as f64 / 1048576.0),
    ];
    let mut pixels = [15, 25, 39, 230].repeat((SIZE.width * SIZE.height) as usize);
    for (line, text) in lines.iter().enumerate() {
        for (column, ch) in text.chars().take(19).enumerate() {
            let rows = glyph(ch);
            for (y, row) in rows.into_iter().enumerate() {
                for x in 0..5 {
                    if row & (1 << (4 - x)) == 0 {
                        continue;
                    }
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let offset = ((6 + line * 17 + y * 2 + dy) * SIZE.width as usize
                                + 6
                                + column * 12
                                + x * 2
                                + dx)
                                * 4;
                            pixels[offset..offset + 4].copy_from_slice(if line == 0 {
                                &[118, 222, 255, 255]
                            } else {
                                &[238, 246, 255, 255]
                            });
                        }
                    }
                }
            }
        }
    }
    pixels
}

fn glyph(ch: char) -> [u8; 7] {
    match ch {
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'G' => [14, 17, 16, 23, 17, 17, 14],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        _ => [0; 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn averages_frames_and_reports_idle_without_counting_hud_redraws() {
        let start = Instant::now();
        let mut counter = Counter::new(start);
        assert_eq!(counter.sample(start), Some(0.0));
        for _ in 0..60 {
            counter.frame();
        }
        assert_eq!(counter.sample(start + Duration::from_millis(500)), None);
        assert_eq!(counter.sample(start + INTERVAL), Some(60.0));
        assert_eq!(counter.sample(start + INTERVAL * 2), Some(0.0));
        assert_eq!(
            pixels(60.0, Some(256 << 20), true, 64 << 20).len(),
            SIZE.rgba_bytes().unwrap()
        );
    }
}
