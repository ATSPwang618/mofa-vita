//! Original rotatebase scan conversion: integer vertices, inclusive edges and
//! 16.16 source steps. Both sources share this path, including rotateswap order.
use super::*;
type Point = [i64; 2];
type Corners = [Point; 3];
fn point(x: f32, y: f32) -> Result<Point, String> {
    if !x.is_finite() || !y.is_finite() || x.abs() > 32767.0 || y.abs() > 32767.0 {
        return Err("rotation coordinates exceed 16.16 range".into());
    }
    Ok([x as i64, y as i64])
}
fn ease(mut t: f32, accel: f64) -> f32 {
    if accel < 0.0 {
        t = 1.0 - t;
        t = f64::from(t).powf(-accel) as f32;
        t = 1.0 - t;
    } else if accel > 0.0 {
        t = f64::from(t).powf(accel) as f32;
    }
    t
}
pub(super) fn points(
    kind: Kind,
    size: Size,
    t: u64,
    duration: u64,
    v: [f64; 7],
) -> Result<([Corners; 2], u32), String> {
    let w = size.width as f32;
    let h = size.height as f32;
    let sx = (size.width / 2) as f32;
    let sy = (size.height / 2) as f32;
    // C++ float CurTime/Time, with defined conversion instead of signed wrap.
    let z = t as f32 / duration as f32;
    let identity = [
        [0, 0],
        [i64::from(size.width) - 1, 0],
        [0, i64::from(size.height) - 1],
    ];
    let mut out = [identity; 2];
    if matches!(kind, Kind::Swap) {
        let twist = (v[1] * 3.14159265368979 * 2.0) as f32;
        for (i, shape) in out.iter_mut().enumerate() {
            let tm = if i == 0 {
                z * z
            } else {
                1.0 - (1.0 - z) * (1.0 - z)
            };
            let (cx, cy, rad, scale) = if i == 0 {
                (
                    (f64::from(-sx * tm + sx)
                        + (f64::from(tm) * 3.14159265368979).sin() * f64::from(sx) * 1.5)
                        as i32 as f32,
                    (-sy * tm + sy) as i32 as f32,
                    tm * twist,
                    1.0 - tm,
                )
            } else {
                (
                    (f64::from((sx - (w - 1.0)) * tm + (w - 1.0))
                        - (f64::from(tm) * 3.14159265368979).sin() * f64::from(sx) * 1.5)
                        as i32 as f32,
                    ((sy - (h - 1.0)) * tm + h - 1.0) as i32 as f32,
                    (-1.0 + tm) * twist,
                    tm,
                )
            };
            let s = (f64::from(rad).sin() * f64::from(scale)) as f32;
            let c = (f64::from(rad).cos() * f64::from(scale)) as f32;
            *shape = [
                point(-sx * c - sy * s + cx, (-sx * -s - sy * c) * scale + cy)?,
                point(
                    (w - 1.0 - sx) * c - sy * s + cx,
                    ((w - 1.0 - sx) * -s - sy * c) * scale + cy,
                )?,
                point(
                    -sx * c + (h - 1.0 - sy) * s + cx,
                    (-sx * -s + (h - 1.0 - sy) * c) * scale + cy,
                )?,
            ];
        }
        return Ok((out, if t >= duration / 2 { 2 } else { 1 }));
    }
    let vanish = matches!(kind, Kind::Vanish);
    let (factor, target, accel, twist, twist_accel, cx, cy) = if vanish {
        (1.0, 0.0, v[0], v[1], v[2], v[3], v[4])
    } else {
        (v[0], 1.0, v[1], v[2], v[3], v[4], v[5])
    };
    let zoom = ease(z, accel);
    let cx = ((sx - cx as f32) * zoom + cx as f32) as i32 as f32;
    let cy = ((sy - cy as f32) * zoom + cy as f32) as i32 as f32;
    let rad = if t == duration {
        0.0
    } else {
        (2.0 * 3.14159265368979 * twist * f64::from(ease(z, twist_accel))) as f32
    };
    let zoom = ((target - factor) * f64::from(zoom) + factor) as f32;
    let s = (f64::from(rad).sin() * f64::from(zoom)) as f32;
    let c = (f64::from(rad).cos() * f64::from(zoom)) as f32;
    out[usize::from(!vanish)] = [
        point(-cx * c - cy * s + sx, -cx * -s - cy * c + sy)?,
        point(
            (w - 1.0 - cx) * c - cy * s + sx,
            (w - 1.0 - cx) * -s - cy * c + sy,
        )?,
        point(
            -cx * c + (h - 1.0 - cy) * s + sx,
            -cx * -s + (h - 1.0 - cy) * c + sy,
        )?,
    ];
    Ok((out, if vanish { 1 } else { 2 }))
}
struct Edge {
    at: usize,
    next: usize,
    direction: i32,
    x: i64,
    step: i64,
    s: i64,
    sstep: i64,
}
impl Edge {
    fn next_index(&self, index: usize) -> usize {
        ((index as i32 + self.direction) & 3) as usize
    }
    fn reset(&mut self, p: &[Point; 4], size: Size) {
        let delta = p[self.next][1] - p[self.at][1] + 1;
        self.step = if delta != 0 {
            65536 * (p[self.next][0] - p[self.at][0]) / delta
        } else {
            65536
        };
        let w = i64::from(size.width);
        let h = i64::from(size.height);
        let extent = if self.direction < 0 {
            [h, -w, -h, w][self.at]
        } else {
            [w, h, -w, -h][self.at]
        };
        self.sstep = if delta != 0 {
            65536 * extent / delta
        } else {
            65536
        };
        self.s = if self.direction < 0 {
            [0, w * 65536 - 1, h * 65536 - 1, 0][self.at]
        } else {
            [0, 0, w * 65536 - 1, h * 65536 - 1][self.at]
        };
        self.x = p[self.at][0] * 65536;
    }
    fn new(p: &[Point; 4], size: Size, top: usize, bottom: usize, y: i64, direction: i32) -> Self {
        let mut e = Self {
            at: top,
            next: ((top as i32 + direction) & 3) as usize,
            direction,
            x: 0,
            step: 0,
            s: 0,
            sstep: 0,
        };
        while e.next != bottom && p[e.next][1] < y {
            e.at = e.next;
            e.next = e.next_index(e.next);
        }
        while e.next != bottom && p[e.next][1] == y {
            e.at = e.next;
            e.next = e.next_index(e.next);
        }
        e.reset(p, size);
        if p[e.at][1] < y {
            let d = y - p[e.at][1];
            e.x += e.step * d;
            e.s += e.sstep * d;
        }
        e
    }
    fn source(&self, size: Size) -> Point {
        let w = i64::from(size.width) * 65536 - 1;
        let h = i64::from(size.height) * 65536 - 1;
        if self.direction < 0 {
            [[0, self.s], [self.s, 0], [w, self.s], [self.s, h]][self.at]
        } else {
            [[self.s, 0], [w, self.s], [self.s, h], [0, self.s]][self.at]
        }
    }
    fn advance(&mut self, p: &[Point; 4], size: Size, bottom: usize, y: i64) {
        if p[self.next][1] == y {
            loop {
                self.at = self.next;
                self.next = self.next_index(self.next);
                if self.next == bottom || p[self.next][1] != y {
                    break;
                }
            }
            self.reset(p, size);
        }
        self.x += self.step;
        self.s += self.sstep;
    }
}
pub(super) fn rows(
    size: Size,
    points: [Corners; 2],
    budget: &Budget,
) -> Result<Arc<Bytes>, String> {
    let mut result = Bytes::zeroed(size.height as usize * 64, budget).map_err(|e| e.to_string())?;
    for (source, points) in points.into_iter().enumerate() {
        let p = [
            points[0],
            points[1],
            [
                points[1][0] - points[0][0] + points[2][0],
                points[1][1] - points[0][1] + points[2][1],
            ],
            points[2],
        ];
        let top = (0..4).min_by_key(|&i| p[i][1]).unwrap();
        // Keep the first point on ties, as the reference's strict comparisons.
        let bottom = (0..4).min_by_key(|&i| -p[i][1]).unwrap();
        let y0 = p[top][1].max(0);
        let y1 = p[bottom][1].min(i64::from(size.height) - 1);
        if y0 > y1 || p.iter().all(|p| p[0] < 0) || p.iter().all(|p| p[0] >= i64::from(size.width))
        {
            continue;
        }
        let mut down = Edge::new(&p, size, top, bottom, y0, -1);
        let mut up = Edge::new(&p, size, top, bottom, y0, 1);
        for y in y0..=y1 {
            let (mut left, mut right) = (down.x >> 16, up.x >> 16);
            let (mut a, mut b) = (down.source(size), up.source(size));
            if right < left {
                std::mem::swap(&mut left, &mut right);
                std::mem::swap(&mut a, &mut b);
            }
            let step = if right != left {
                [
                    (b[0] - a[0] + 1) / (right - left + 1),
                    (b[1] - a[1] + 1) / (right - left + 1),
                ]
            } else {
                [65536, 65536]
            };
            for (i, v) in [left, right + 1, a[0], a[1], step[0], step[1], 0, 0]
                .into_iter()
                .enumerate()
            {
                let v = i32::try_from(v).map_err(|_| "rotation scanline exceeds 16.16 range")?;
                put(result.as_mut_slice(), y as usize * 16 + source * 8 + i, v);
            }
            down.advance(&p, size, bottom, y);
            up.advance(&p, size, bottom, y);
        }
    }
    Ok(Arc::new(result))
}
