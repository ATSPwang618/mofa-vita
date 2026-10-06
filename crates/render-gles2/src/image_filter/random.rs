//! Separable LCG coordinates, never pixels. One row seed and one column affine
//! transform let the fragment shader obtain its state with a single multiply.
use krkr_protocol::{filter::Kind, graphics::Rect};

pub(super) struct Sequence {
    pub a: u32,
    pub c: u32,
    seed: u32,
    layout: Layout,
}
enum Layout {
    Noise {
        rectangle: Rect,
        color: bool,
    },
    Fill {
        rectangle: Rect,
        legacy: bool,
        mono: bool,
        hold: bool,
        full: bool,
    },
}
fn affine(mut a: u32, mut c: u32, mut count: u32) -> [u32; 2] {
    let mut result = [1u32, 0u32];
    while count != 0 {
        if count & 1 != 0 {
            result = [
                result[0].wrapping_mul(a),
                result[1].wrapping_mul(a).wrapping_add(c),
            ];
        }
        c = c.wrapping_mul(a.wrapping_add(1));
        a = a.wrapping_mul(a);
        count >>= 1;
    }
    result
}
impl Sequence {
    pub fn new(kind: Kind, rectangle: Rect) -> Option<Self> {
        match kind {
            Kind::Noise { seed, level } => Some(Self {
                a: 214013,
                c: 2531011,
                seed,
                layout: Layout::Noise {
                    rectangle,
                    color: level.is_some(),
                },
            }),
            Kind::RandomFill {
                seed,
                rectangle,
                legacy,
                monochrome,
                hold_alpha,
                range,
                ..
            } => Some(Self {
                a: if legacy { 0x5d588b65 } else { 0x7d2b89dd },
                c: 1,
                seed,
                layout: Layout::Fill {
                    rectangle,
                    legacy,
                    mono: monochrome,
                    hold: hold_alpha,
                    full: range == 255,
                },
            }),
            _ => None,
        }
    }
    fn column(&self, global: i32) -> (u32, [u8; 4]) {
        let mut control = [0; 4];
        let index = match self.layout {
            Layout::Noise { rectangle, color } => {
                (global.wrapping_sub(rectangle.left) as u32).wrapping_mul(if color { 3 } else { 1 })
            }
            Layout::Fill {
                rectangle,
                legacy,
                mono,
                hold,
                full,
            } => {
                let x = global.wrapping_sub(rectangle.left) as u32;
                let width = rectangle.width;
                if mono {
                    if full {
                        let mut part = x % 3;
                        if legacy {
                            if x >= width - width % 3 {
                                part = if x == width.wrapping_sub(1) { 2 } else { 1 };
                            } else {
                                control[1] = 1;
                            }
                        } else if x == width.wrapping_sub(1) && width % 3 == 1 {
                            part = 1;
                        }
                        control[0] = part as u8;
                        x / 3
                    } else {
                        control[0] = u8::from(!x.is_multiple_of(2) || x == width.wrapping_sub(1));
                        x / 2
                    }
                } else if full {
                    let mut index = x.wrapping_add(if hold { x / 4 } else { 0 });
                    if legacy && width >= 4 {
                        let tail = width % 4;
                        if x >= width - tail {
                            control[2] = 1;
                        }
                        if x < tail {
                            index = (width - tail).wrapping_add(x);
                        }
                    }
                    index
                } else {
                    control[0] = (x % 2) as u8;
                    (x / 2).wrapping_mul(3).wrapping_add(x % 2)
                }
            }
        };
        (index, control)
    }
    fn row(&self, global: i32) -> u32 {
        let (rectangle, steps) = match self.layout {
            Layout::Noise { rectangle, color } => (
                rectangle,
                rectangle.width.wrapping_mul(if color { 3 } else { 1 }),
            ),
            Layout::Fill {
                rectangle,
                mono,
                hold,
                full,
                ..
            } => {
                let width = rectangle.width;
                let steps = if mono {
                    if full {
                        width / 3 + 1
                    } else {
                        width.wrapping_add(1) / 2
                    }
                } else if full {
                    width.wrapping_add(if hold { width / 4 } else { 0 })
                } else {
                    (width / 2).wrapping_mul(3).wrapping_add((width % 2) * 2)
                };
                (rectangle, steps)
            }
        };
        (global.wrapping_sub(rectangle.top) as u32).wrapping_mul(steps)
    }
    pub fn fill(&self, bytes: &mut [u8], width: usize, part: Rect) {
        for x in 0..part.width as usize {
            let (count, control) = self.column(part.left + x as i32);
            let [a, c] = affine(self.a, self.c, count);
            for (row, word) in [(0, a), (1, c)] {
                let at = (row * width + x) * 4;
                bytes[at..at + 4].copy_from_slice(&word.to_le_bytes());
            }
            let at = (3 * width + x) * 4;
            bytes[at..at + 4].copy_from_slice(&control);
        }
        for y in 0..part.height as usize {
            let [a, c] = affine(self.a, self.c, self.row(part.top + y as i32));
            let seed = self.seed.wrapping_mul(a).wrapping_add(c);
            let at = (2 * width + y) * 4;
            bytes[at..at + 4].copy_from_slice(&seed.to_le_bytes());
        }
    }
}
