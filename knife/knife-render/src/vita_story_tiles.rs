pub(crate) type Corners = [[f32; 2]; 4];

/// Reject a tile only when an edge separates it from the stage clip. A tile
/// crossing the clip still goes through the normal UV polygon clipper.
pub(crate) fn outside_clip(positions: Corners, clip: Corners) -> bool {
    if !clip.iter().flatten().all(|v| v.is_finite()) { return false; }
    let twice_area: f32 = clip.iter().zip(clip.iter().cycle().skip(1))
        .map(|(a, b)| a[0] * b[1] - a[1] * b[0]).sum();
    if !twice_area.is_finite() || twice_area.abs() < 0.001 { return false; }
    let winding = twice_area.signum();
    clip.iter().zip(clip.iter().cycle().skip(1)).any(|(a, b)| {
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let tolerance = 0.01 * (dx.abs() + dy.abs());
        positions.iter().all(|p|
            winding * (dx * (p[1] - a[1]) - dy * (p[0] - a[0])) < -tolerance)
    })
}

pub(crate) fn rectangular_uv([a, b, c, d]: Corners) -> bool {
    [a, b, c, d].iter().flatten().all(|v| (0.0..=1.0).contains(v))
        && a[1] == b[1] && b[0] == c[0] && c[1] == d[1] && d[0] == a[0]
        && a[0] != b[0] && a[1] != d[1]
}

/// Restrict an ordinary quad to a tile's interior in source UV space. The tile
/// texture itself includes neighboring blocks so bilinear taps cross the seam.
pub(crate) fn tile_quad(
    pos: Corners, uv: Corners, interior: [f32; 4], crop: [f32; 4], target: [f32; 2],
) -> Option<(Corners, Corners)> {
    let edge = |axis: usize, end: usize| {
        let a = uv[0][axis];
        let b = uv[end][axis];
        let from = ((interior[axis] - a) / (b - a)).clamp(0., 1.);
        let to = ((interior[axis + 2] - a) / (b - a)).clamp(0., 1.);
        (from.min(to), from.max(to))
    };
    let (left, right) = edge(0, 1);
    let (top, bottom) = edge(1, 3);
    if left >= right || top >= bottom { return None; }

    let lerp = |a: f32, b: f32, t: f32| if t == 1.0 { b } else { a + (b - a) * t };
    let params = [[left, top], [right, top], [right, bottom], [left, bottom]];
    let positions = params.map(|[x, y]| std::array::from_fn(|axis|
        lerp(lerp(pos[0][axis], pos[1][axis], x), lerp(pos[3][axis], pos[2][axis], x), y)));
    for (axis, extent) in target.into_iter().enumerate() {
        if positions.iter().all(|p| p[axis] <= 0.) || positions.iter().all(|p| p[axis] >= extent) {
            return None;
        }
    }
    let coords = params.map(|[x, y]| [
        (lerp(uv[0][0], uv[1][0], x) - crop[0]) / (crop[2] - crop[0]),
        (lerp(uv[0][1], uv[3][1], y) - crop[1]) / (crop[3] - crop[1]),
    ]);
    Some((positions, coords))
}
