//! Conservative destination coverage of one nearest-sampled source tile.
//! Keep the fragment shader's exact rejection at the edges; submit geometry
//! only near pixels that can survive it, instead of a full quad per texture.
use krkr_protocol::graphics::Rect;

pub(crate) fn tile_area(
    area: Rect,
    mapping: [f32; 6],
    scale: Option<[f32; 2]>,
    tile: Rect,
) -> Option<Rect> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let [a, b, tx, c, d, ty] = mapping.map(f64::from);
    let determinant = a * d - b * c;
    // Degenerate/ill-conditioned maps stay on the shader's existing path.
    // NaNs must never authorize dropping destination pixels.
    if mapping.iter().any(|v| !v.is_finite())
        || determinant.abs() <= (a.abs() * d.abs() + b.abs() * c.abs()).max(1.) * 1e-12
    {
        return Some(area);
    }
    let scale = scale.map(|s| s.map(f64::from));
    if scale.is_some_and(|s| s.iter().any(|v| !v.is_finite() || *v <= 0.)) {
        return Some(area);
    }
    let max_x = f64::from(area.left)
        .abs()
        .max((f64::from(area.left) + f64::from(area.width)).abs());
    let max_y = f64::from(area.top)
        .abs()
        .max((f64::from(area.top) + f64::from(area.height)).abs());
    let source_axis = |axis: usize, start: i32, length: u32| {
        let mut lo = f64::from(start);
        let mut hi = lo + f64::from(length);
        if let Some(scale) = scale {
            // floor((floor(q + .5) + .5) * scale) belongs to this tile.
            // Relax both floors before inversion, preserving fractional grids.
            lo /= scale[axis];
            hi /= scale[axis];
        }
        let coefficients = if axis == 0 { [a, b, tx] } else { [c, d, ty] };
        let magnitude = coefficients[0].abs() * max_x
            + coefficients[1].abs() * max_y
            + coefficients[2].abs()
            + lo.abs().max(hi.abs());
        // Includes nearest rounding, logical-to-storage rounding and the GPU
        // dot-product error, including cancellation in large translations.
        let guard = 2. + magnitude * 16. * f64::from(f32::EPSILON);
        [lo - guard, hi + guard]
    };
    let [x0, x1] = source_axis(0, tile.left, tile.width);
    let [y0, y1] = source_axis(1, tile.top, tile.height);
    let inverse_determinant = determinant.recip();
    let points = [[x0, y0], [x1, y0], [x0, y1], [x1, y1]].map(|[x, y]| {
        let x = x - tx;
        let y = y - ty;
        [
            (d * x - b * y) * inverse_determinant,
            (a * y - c * x) * inverse_determinant,
        ]
    });
    let axis = |axis: usize, start: i32, length: u32| {
        let end = f64::from(start) + f64::from(length);
        let lo = points
            .iter()
            .map(|p| p[axis])
            .fold(f64::INFINITY, f64::min)
            .floor()
            - 1.;
        let hi = points
            .iter()
            .map(|p| p[axis])
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil()
            + 1.;
        (
            lo.clamp(f64::from(start), end) as i64,
            hi.clamp(f64::from(start), end) as i64,
        )
    };
    let (left, right) = axis(0, area.left, area.width);
    let (top, bottom) = axis(1, area.top, area.height);
    if left >= right || top >= bottom {
        return None;
    }
    let (Ok(left), Ok(top)) = (i32::try_from(left), i32::try_from(top)) else {
        return Some(area);
    };
    Some(Rect {
        left,
        top,
        width: (right - i64::from(left)) as u32,
        height: (bottom - i64::from(top)) as u32,
    })
}
