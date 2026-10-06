//! Owned vector geometry for LayerExDraw. Point arithmetic retains TJS precision; rasterization converts the resulting
//! coordinates to the reference backend's f32 representation.
use std::f32::consts::{FRAC_PI_2, TAU};
use tjs_core::{NativeError, NativeResult};

pub type Point = [f64; 2];
#[derive(Clone, Copy, Debug)]
pub enum Segment {
    Move(Point),
    Line(Point),
    Cubic(Point, Point, Point),
    Close,
}
#[derive(Clone, Default, Debug)]
pub struct Path {
    pub segments: Vec<Segment>,
    figure_started: bool,
}
impl Path {
    pub fn start_figure(&mut self) {
        // This flag behavior is intentional: startFigure in the SDL source
        // does not insert a move or clear an existing figure.
        self.figure_started = true;
    }
    pub fn close_figure(&mut self) {
        self.segments.push(Segment::Close);
    }
    pub fn line(&mut self, from: Point, to: Point) {
        self.segments.push(if self.figure_started {
            Segment::Line(from)
        } else {
            Segment::Move(from)
        });
        self.figure_started = true;
        self.segments.push(Segment::Line(to));
    }
    pub fn lines(&mut self, points: &[Point]) {
        let Some(&first) = points.first() else { return };
        if !self.figure_started {
            self.segments.push(Segment::Move(first));
            self.figure_started = true;
        }
        self.segments
            .extend(points[1..].iter().copied().map(Segment::Line));
    }
    pub fn polygon(&mut self, points: &[Point]) {
        let Some(&first) = points.first() else { return };
        self.segments.push(Segment::Move(first));
        self.figure_started = true;
        self.segments
            .extend(points[1..].iter().copied().map(Segment::Line));
        self.close_figure();
    }
    pub fn bezier(&mut self, points: [Point; 4]) {
        if !self.figure_started {
            self.segments.push(Segment::Move(points[0]));
            self.figure_started = true;
        }
        self.segments
            .push(Segment::Cubic(points[1], points[2], points[3]));
    }
    pub fn beziers(&mut self, points: &[Point]) {
        if points.len() < 4 || !(points.len() - 1).is_multiple_of(3) {
            return;
        }
        if !self.figure_started {
            self.segments.push(Segment::Move(points[0]));
            self.figure_started = true;
        }
        for p in points[1..].as_chunks::<3>().0.iter() {
            self.segments.push(Segment::Cubic(p[0], p[1], p[2]));
        }
    }
    fn cardinal(&mut self, p: [Point; 4], tension: f64) {
        // PointF arithmetic precedes conversion to the backend's float type.
        let mut a = [0.; 2];
        let mut b = [0.; 2];
        for axis in 0..2 {
            a[axis] = p[1][axis] + (p[2][axis] - p[0][axis]) * tension / 3.;
            b[axis] = p[2][axis] - (p[3][axis] - p[1][axis]) * tension / 3.;
        }
        self.segments.push(Segment::Cubic(a, b, p[2]));
    }
    pub fn curve(&mut self, points: &[Point], offset: i32, count: i32, tension: f64) {
        if points.len() < 2 || offset < 0 {
            return;
        }
        let count = if count < 0 {
            points.len() - 1
        } else {
            count as usize
        };
        let offset = offset as usize;
        let Some(end) = offset.checked_add(count) else {
            return;
        };
        if end >= points.len() {
            return;
        }
        if !self.figure_started {
            self.segments.push(Segment::Move(points[offset]));
            self.figure_started = true;
        }
        for i in offset..end {
            self.cardinal(
                [
                    points[i.saturating_sub(1)],
                    points[i],
                    points[i + 1],
                    points[(i + 2).min(points.len() - 1)],
                ],
                tension,
            );
        }
    }
    pub fn closed_curve(&mut self, points: &[Point], tension: f64) {
        let n = points.len();
        if n < 2 {
            return;
        }
        self.segments.push(Segment::Move(points[0]));
        self.figure_started = true;
        for i in 0..n {
            self.cardinal(
                [
                    points[(i + n - 1) % n],
                    points[i],
                    points[(i + 1) % n],
                    points[(i + 2) % n],
                ],
                tension,
            );
        }
        self.close_figure();
    }
    pub fn rectangle(&mut self, [x, y, w, h]: [f64; 4]) {
        self.segments.extend([
            Segment::Move([x, y]),
            Segment::Line([x + w, y]),
            Segment::Line([x + w, y + h]),
            Segment::Line([x, y + h]),
            Segment::Close,
        ]);
        self.figure_started = false;
    }
    pub fn rectangles(&mut self, rects: &[[f64; 4]]) {
        for &rect in rects {
            self.rectangle(rect);
        }
        self.figure_started = false;
    }
    pub fn arc(&mut self, rect: [f64; 4], start: f64, sweep: f64) -> NativeResult<()> {
        self.elliptic_arc(
            rect.map(|v| v as f32),
            start.to_radians() as f32,
            sweep.to_radians() as f32,
            !self.figure_started,
        )?;
        self.figure_started = true;
        Ok(())
    }
    pub fn pie(&mut self, rect: [f64; 4], start: f64, sweep: f64) -> NativeResult<()> {
        let [x, y, w, h] = rect.map(|v| v as f32);
        let (rx, ry) = (w / 2., h / 2.);
        let center = [x + rx, y + ry];
        let start = start.to_radians() as f32;
        self.segments.push(Segment::Move(center.map(f64::from)));
        self.segments.push(Segment::Line(
            [center[0] + rx * start.cos(), center[1] + ry * start.sin()].map(f64::from),
        ));
        self.figure_started = true;
        self.elliptic_arc(
            rect.map(|v| v as f32),
            start,
            sweep.to_radians() as f32,
            false,
        )?;
        self.segments.push(Segment::Line(center.map(f64::from)));
        self.close_figure();
        Ok(())
    }
    pub fn ellipse(&mut self, rect: [f64; 4]) -> NativeResult<()> {
        self.elliptic_arc(rect.map(|v| v as f32), 0., TAU, true)?;
        self.close_figure();
        self.figure_started = false;
        Ok(())
    }
    fn elliptic_arc(
        &mut self,
        [x, y, w, h]: [f32; 4],
        start: f32,
        sweep: f32,
        move_to: bool,
    ) -> NativeResult<()> {
        if sweep.abs() < 1e-6 {
            return Ok(());
        }
        let count = (sweep.abs() / FRAC_PI_2).ceil().max(1.);
        // Bound expansion rather than silently changing the requested sweep.
        if !count.is_finite() || count > 1_000_000. {
            return Err(NativeError::Message("vector arc exceeds segment budget"));
        }
        let count = count as usize;
        self.segments
            .try_reserve(count + usize::from(move_to))
            .map_err(|e| NativeError::Detail(e.to_string()))?;
        let (rx, ry) = (w / 2., h / 2.);
        let (cx, cy) = (x + rx, y + ry);
        if move_to {
            self.segments.push(Segment::Move(
                [cx + rx * start.cos(), cy + ry * start.sin()].map(f64::from),
            ));
        }
        let delta = sweep / count as f32;
        let k = 4. / 3. * (delta / 4.).tan();
        let mut angle = start;
        for _ in 0..count {
            let end = angle + delta;
            let (s1, c1) = angle.sin_cos();
            let (s2, c2) = end.sin_cos();
            self.segments.push(Segment::Cubic(
                [cx + rx * c1 - k * rx * s1, cy + ry * s1 + k * ry * c1].map(f64::from),
                [cx + rx * c2 + k * rx * s2, cy + ry * s2 - k * ry * c2].map(f64::from),
                [cx + rx * c2, cy + ry * s2].map(f64::from),
            ));
            angle = end;
        }
        Ok(())
    }
}
