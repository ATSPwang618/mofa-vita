//! Small CPU glyph cache used only while editing/operating the input panel.
use super::Widget;
use ab_glyph::{Font, FontArc, ScaleFont};
use krkr_protocol::graphics::Size;
use std::collections::HashMap;

struct Glyph {
    width: usize,
    height: usize,
    left: i32,
    top: i32,
    advance: f32,
    alpha: Vec<u8>,
}
pub(super) struct Paint {
    font: FontArc,
    glyphs: HashMap<(char, u8), Glyph>,
}
const BACKGROUND: [u8; 4] = [9, 22, 40, 210];
impl Paint {
    pub fn new() -> Self {
        Self {
            font: krkr_render::font::bundled::font(),
            glyphs: HashMap::new(),
        }
    }
    pub fn page(&mut self, size: Size, widgets: &[Widget]) -> Vec<u8> {
        let mut data = BACKGROUND.repeat((size.width * size.height) as usize);
        for widget in widgets {
            let patch = self.widget(widget);
            for (row, bytes) in patch
                .chunks_exact(widget.rect.width as usize * 4)
                .enumerate()
            {
                let start = ((widget.rect.top as usize + row) * size.width as usize
                    + widget.rect.left as usize)
                    * 4;
                data[start..start + bytes.len()].copy_from_slice(bytes);
            }
        }
        data
    }
    pub fn widget(&mut self, widget: &Widget) -> Vec<u8> {
        let (w, h) = (widget.rect.width as usize, widget.rect.height as usize);
        let (color, ink): ([u8; 4], [u8; 3]) = match widget.style {
            1 => ([46, 126, 199, 244], [255, 255, 255]),
            2 => ([86, 79, 167, 240], [255, 244, 179]),
            3 => ([88, 185, 228, 252], [10, 32, 52]),
            4 => (BACKGROUND, [211, 226, 245]),
            _ => ([26, 56, 89, 218], [237, 243, 255]),
        };
        let mut data = color.repeat(w * h);
        if widget.style != 4 {
            for x in 0..w {
                for y in [0, h - 1] {
                    let at = (y * w + x) * 4;
                    data[at..at + 4].copy_from_slice(&[61, 95, 130, 235]);
                }
            }
            for y in 0..h {
                for x in [0, w - 1] {
                    let at = (y * w + x) * 4;
                    data[at..at + 4].copy_from_slice(&[61, 95, 130, 235]);
                }
            }
        }
        let mut text = Vec::new();
        let mut width = 0.;
        for ch in widget.label.chars() {
            self.cache(ch, widget.size);
            let advance = self.glyphs[&(ch, widget.size)].advance;
            if width + advance > w.saturating_sub(8) as f32 {
                break;
            }
            text.push(ch);
            width += advance;
        }
        let mut x = (w as f32 - width) / 2.;
        let baseline =
            ((h as f32 - widget.size as f32) / 2. + widget.size as f32 * 0.85).round() as i32;
        for ch in text {
            let glyph = &self.glyphs[&(ch, widget.size)];
            for gy in 0..glyph.height {
                for gx in 0..glyph.width {
                    let px = x.round() as i32 + glyph.left + gx as i32;
                    let py = baseline + glyph.top + gy as i32;
                    if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                        continue;
                    }
                    let alpha = u32::from(glyph.alpha[gy * glyph.width + gx]);
                    if alpha == 0 {
                        continue;
                    }
                    let at = (py as usize * w + px as usize) * 4;
                    let back = u32::from(data[at + 3]) * (255 - alpha);
                    let out = alpha * 255 + back;
                    for c in 0..3 {
                        data[at + c] = ((u32::from(ink[c]) * alpha * 255
                            + u32::from(data[at + c]) * back
                            + out / 2)
                            / out) as u8;
                    }
                    data[at + 3] = ((out + 127) / 255) as u8;
                }
            }
            x += glyph.advance;
        }
        data
    }
    fn cache(&mut self, ch: char, size: u8) {
        if self.glyphs.contains_key(&(ch, size)) {
            return;
        }
        let scale =
            size as f32 * self.font.height_unscaled() / self.font.units_per_em().unwrap_or(1000.);
        let scaled = self.font.as_scaled(scale);
        let glyph = scaled.scaled_glyph(ch);
        let advance = scaled.h_advance(glyph.id);
        let mut result = Glyph {
            width: 0,
            height: 0,
            left: 0,
            top: 0,
            advance,
            alpha: Vec::new(),
        };
        if let Some(outline) = scaled.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            result.width = bounds.width().ceil() as usize;
            result.height = bounds.height().ceil() as usize;
            result.left = bounds.min.x as i32;
            result.top = bounds.min.y as i32;
            result.alpha = vec![0; result.width * result.height];
            outline.draw(|x, y, a| {
                result.alpha[y as usize * result.width + x as usize] = (a * 255.).round() as u8
            });
        }
        self.glyphs.insert((ch, size), result);
    }
}
