use super::*;
#[derive(Default)]
pub(super) struct Mapping {
    pub d: [f64; 4],
    pub s: [i64; 4],
    origin: [i64; 2],
    pub start: [i64; 2],
    pub end: [i64; 2],
}
impl Mapping {
    pub fn clip(&mut self, dest: Size, source: Size) -> bool {
        for (i, (sl, dl)) in [(source.width, dest.width), (source.height, dest.height)]
            .into_iter()
            .enumerate()
        {
            let z = self.d[i + 2] / self.s[i + 2] as f64;
            if self.s[i] + self.s[i + 2] <= 0 || self.s[i] >= i64::from(sl) {
                return false;
            }
            if self.s[i] < 0 {
                let cut = z * (-self.s[i]) as f64;
                self.s[i + 2] += self.s[i];
                self.s[i] = 0;
                self.d[i + 2] -= cut;
                self.d[i] += cut;
            }
            let cut = self.s[i] + self.s[i + 2] - i64::from(sl);
            if cut > 0 {
                self.s[i + 2] -= cut;
                self.d[i + 2] -= z * cut as f64;
            }
            self.origin[i] = self.d[i] as i64;
            let edge = self.d[i] + self.d[i + 2];
            let extent = edge.ceil() as i64 - self.origin[i];
            self.start[i] = (-self.origin[i]).max(0);
            self.end[i] = extent.min(i64::from(dl) - self.origin[i]);
            if self.start[i] >= self.end[i] {
                return false;
            }
        }
        true
    }
    pub fn row(&self, out: &mut [u8], input: &Pixels, same: bool, dest: Size, row: usize) {
        let y = self.start[1] + row as i64;
        let vi = self.axis(1, y);
        for x in self.start[0]..self.end[0] {
            let hi = self.axis(0, x);
            let mut sum = [0u64; 4];
            for (sy, va, vc) in vi.points() {
                for (sx, ha, hc) in hi.points() {
                    let ma = ha.wrapping_mul(va);
                    if ma == 0 {
                        continue;
                    }
                    // Native code could form an out-of-bounds pointer for a
                    // fractional enlargement; never dereference outside pixels.
                    if sx < 0
                        || sy < 0
                        || sx >= i64::from(input.size.width)
                        || sy >= i64::from(input.size.height)
                    {
                        continue;
                    }
                    let i = (sy as usize * input.size.width as usize + sx as usize) * 4;
                    let p = if same {
                        &out[i..i + 4]
                    } else {
                        &input.main.as_ref().unwrap().as_slice()[i..i + 4]
                    };
                    let mc = hc.wrapping_mul(vc);
                    for j in 0..4 {
                        sum[j] = sum[j].wrapping_add(u64::from(p[j]).wrapping_mul(if j == 3 {
                            ma
                        } else {
                            mc
                        }));
                    }
                }
            }
            let div = hi.total.wrapping_mul(vi.total);
            if div == 0 {
                continue;
            }
            let i = ((self.origin[1] + y) as usize * dest.width as usize
                + (self.origin[0] + x) as usize)
                * 4;
            for j in 0..4 {
                out[i + j] = (sum[j] / div) as u8;
            }
        }
    }
    fn axis(&self, i: usize, pos: i64) -> Axis {
        let ratio = self.s[i + 2] as f64 / self.d[i + 2];
        let unit = if ratio <= 1. / 16. {
            (256 / ((2. / 16.) / ratio) as u64).max(1)
        } else {
            256
        };
        let r1 = (pos as f64 - (self.d[i] - self.origin[i] as f64)) * ratio;
        let r2 = r1 + ratio;
        let (t1, t2) = (r1 as i64, r2 as i64);
        let tc = unit.wrapping_sub(((r1 - t1 as f64) * unit as f64) as i64 as u64);
        let bc = ((r2 - t2 as f64) * unit as f64) as i64 as u64;
        let total = tc
            .wrapping_add(bc)
            .wrapping_add(((t2 - t1 - 1) as u64).wrapping_mul(unit));
        let (f1, f2) = if pos == self.start[i] || pos == self.end[i] - 1 {
            (r1.max(0.), r2.min(self.s[i + 2] as f64))
        } else {
            (r1, r2)
        };
        let (u1, u2) = (f1 as i64, f2 as i64);
        Axis {
            offset: self.s[i] + u1 + 1,
            step: u2 - u1 - 1,
            unit,
            total,
            tc,
            bc,
            ta: unit.wrapping_sub(((f1 - u1 as f64) * unit as f64) as i64 as u64),
            ba: ((f2 - u2 as f64) * unit as f64) as i64 as u64,
        }
    }
}
struct Axis {
    offset: i64,
    step: i64,
    unit: u64,
    total: u64,
    ta: u64,
    tc: u64,
    ba: u64,
    bc: u64,
}
impl Axis {
    fn points(&self) -> impl Iterator<Item = (i64, u64, u64)> + '_ {
        std::iter::once((self.offset - 1, self.ta, self.tc))
            .chain((0..self.step.max(0)).map(|n| (self.offset + n, self.unit, self.unit)))
            .chain(std::iter::once((self.offset + self.step, self.ba, self.bc)))
    }
}
pub(super) fn fast_row(out: &mut [u8], input: &Pixels, sx: usize, sy: usize, dest: Size, y: usize) {
    let data = input.main.as_ref().unwrap().as_slice();
    let w = input.size.width as usize;
    let h = input.size.height as usize;
    let top = y * sy;
    let bottom = (top + sy).min(h);
    for x in 0..dest.width as usize {
        let left = x * sx;
        let right = (left + sx).min(w);
        let mut sum = [0u64; 3];
        for ay in top..bottom {
            let mut line = [0u64; 3];
            for ax in left..right {
                for j in 0..3 {
                    line[j] += u64::from(data[(ay * w + ax) * 4 + j]);
                }
            }
            // Two separate truncations, not a single rectangular average.
            for j in 0..3 {
                sum[j] += line[j] / (right - left) as u64;
            }
        }
        let i = (y * dest.width as usize + x) * 4;
        for j in 0..3 {
            out[i + j] = (sum[j] / (bottom - top) as u64) as u8;
        }
        out[i + 3] = 255;
    }
}
