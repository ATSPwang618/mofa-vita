//! Portable surface-chain evaluation; retains the reference's Y/U axis swap,
//! inheritance masks and shape-specific transform path.
use super::sample::Sample;
use krkr_engine::protocol::budget::Budget;
use tjs_core::{NativeError, NativeResult};

#[derive(Clone, Copy, Debug)]
pub(super) struct Matrix(pub [f32; 16]);
impl Default for Matrix {
    fn default() -> Self {
        Self([
            1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
        ])
    }
}
impl Matrix {
    pub fn multiply(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| {
            let row = i % 4;
            let col = i / 4;
            self.0[row] * other.0[col * 4]
                + self.0[row + 4] * other.0[col * 4 + 1]
                + self.0[row + 8] * other.0[col * 4 + 2]
                + self.0[row + 12] * other.0[col * 4 + 3]
        }))
    }
    pub fn point(self, p: [f32; 4]) -> [f32; 4] {
        std::array::from_fn(|row| {
            self.0[row] * p[0]
                + self.0[row + 4] * p[1]
                + self.0[row + 8] * p[2]
                + self.0[row + 12] * p[3]
        })
    }
    pub fn translation(x: f32, y: f32, z: f32) -> Self {
        let mut matrix = Self::default();
        matrix.0[12] = x;
        matrix.0[13] = y;
        matrix.0[14] = z;
        matrix
    }
    pub fn scale(x: f32, y: f32, z: f32) -> Self {
        let mut matrix = Self::default();
        matrix.0[0] = x;
        matrix.0[5] = y;
        matrix.0[10] = z;
        matrix
    }
    pub fn rotation(degrees: f32) -> Self {
        let (s, c) = degrees.to_radians().sin_cos();
        Self([c, s, 0., 0., -s, c, 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.])
    }
}
#[derive(Clone, Debug)]
pub(super) struct Surface {
    pub kind: u8,
    pub sample: Sample,
    pub inherit: u32,
    pub attach: Matrix,
    pub size: [f32; 2],
    pub origin: [f32; 2],
}
impl Surface {
    fn model(&self, mask: u32, texture: bool, translation_only: bool) -> Matrix {
        let s = &self.sample;
        let mut model = Matrix::translation(s.coord[0] as f32, s.coord[1] as f32, 0.);
        if !translation_only {
            if mask & 0x10 != 0 {
                model = model.multiply(Matrix::rotation(s.angle as f32));
            }
            if mask & 0x20 != 0 {
                model = model.multiply(Matrix::scale(s.zoom[0] as f32, 1., 1.));
            }
            if mask & 0x40 != 0 {
                model = model.multiply(Matrix::scale(1., s.zoom[1] as f32, 1.));
            }
            let mut shear = Matrix::default();
            if mask & 0x80 != 0 {
                shear.0[4] = s.slant[0] as f32;
            }
            if mask & 0x100 != 0 {
                shear.0[1] = s.slant[1] as f32;
            }
            model = shear.multiply(model);
        }
        if texture && (1..=2).contains(&self.kind) {
            model = model.multiply(Matrix::translation(
                -self.origin[0] - s.origin[0] as f32,
                -self.origin[1] - s.origin[1] as f32,
                0.,
            ));
            model = model.multiply(Matrix::scale(self.size[0], self.size[1], 1.));
        }
        self.attach.multiply(model)
    }
}
fn basis(t: f32) -> [f32; 4] {
    let s = 1. - t;
    [s * s * s, 3. * t * s * s, 3. * t * t * s, t * t * t]
}
fn bezier(points: &[f64; 32], u: f32, v: f32) -> [f32; 2] {
    let a = basis(u);
    let b = basis(v);
    let mut result = [0.; 2];
    for (row, &a) in a.iter().enumerate() {
        for (col, &b) in b.iter().enumerate() {
            let index = (row * 4 + col) * 2;
            let basis = a * b;
            result[0] += points[index] as f32 * basis;
            result[1] += points[index + 1] as f32 * basis;
        }
    }
    result
}
pub(super) fn point(chain: &[Surface], u: f32, v: f32) -> [f32; 2] {
    let mut transformed = [1.; 4];
    let mut inherited = 0x0fffffff;
    for (i, surface) in chain.iter().enumerate().rev() {
        inherited &= surface.inherit;
        let mask = if i > 0 && i + 1 < chain.len() {
            inherited
        } else {
            u32::MAX
        };
        let model = surface.model(mask, true, false);
        if surface.kind == 3 {
            transformed = model.point(transformed);
            continue;
        }
        let [u, v] = if i + 1 < chain.len() {
            [
                (transformed[1] + surface.origin[1]) / surface.size[1],
                (transformed[0] + surface.origin[0]) / surface.size[0],
            ]
        } else {
            [u, v]
        };
        let xy = if surface.kind == 1 {
            surface
                .sample
                .mesh
                .as_ref()
                .map_or([v, u], |p| bezier(p, u, v))
        } else {
            [v, u]
        };
        transformed = model.point([xy[0], xy[1], 0., 1.]);
    }
    [transformed[0], -transformed[1]]
}
pub(super) fn shape_point(chain: &[Surface], point: [f32; 2]) -> [f32; 2] {
    let mut transformed = [point[0], point[1], 0., 1.];
    let mut inherited = 0x0fffffff;
    for (i, surface) in chain.iter().enumerate().rev() {
        inherited &= surface.inherit;
        transformed = surface
            .model(inherited, false, i + 1 == chain.len())
            .point(transformed);
    }
    [transformed[0], -transformed[1]]
}
pub(crate) use krkr_engine::protocol::mesh::{Geometry as Mesh, Vertex};
pub(super) fn subdivide(
    chain: &[Surface],
    divisions: [u32; 2],
    budget: &Budget,
    cancelled: &dyn Fn() -> bool,
) -> NativeResult<Mesh> {
    let [x, y] = divisions.map(|v| v as usize);
    if x == 0 || y == 0 {
        return Err(NativeError::Message(
            "E-mote mesh division must be positive",
        ));
    }
    let vertices = x
        .checked_add(1)
        .and_then(|x| y.checked_add(1).and_then(|y| x.checked_mul(y)))
        .filter(|&n| n <= 65536)
        .ok_or(NativeError::Message(
            "E-mote mesh exceeds 16-bit vertex range",
        ))?;
    let indices = x * y * 6;
    let permit = budget
        .reserve(vertices * std::mem::size_of::<Vertex>() + indices * std::mem::size_of::<u16>())
        .map_err(|e| NativeError::Detail(e.to_string()))?;
    let mut mesh = Mesh {
        vertices: Vec::with_capacity(vertices),
        indices: Vec::with_capacity(indices),
        _permit: permit,
    };
    for row in 0..=y {
        if cancelled() {
            return Err(NativeError::Message("E-mote tessellation cancelled"));
        }
        let v = row as f32 / y as f32;
        for col in 0..=x {
            let u = col as f32 / x as f32;
            mesh.vertices.push(Vertex {
                position: point(chain, u, v),
                uv: [v, u],
            });
        }
    }
    for row in 0..y {
        for col in 0..x {
            let a = (row * (x + 1) + col) as u16;
            let b = a + 1;
            let c = ((row + 1) * (x + 1) + col) as u16;
            let d = c + 1;
            mesh.indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
    Ok(mesh)
}
#[derive(Clone, Debug)]
pub(super) struct HitArea {
    pub label: String,
    pub kind: u8,
    pub bounds: [f32; 4],
    pub corners: [[f32; 2]; 4],
}
impl HitArea {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let [left, top, width, height] = self.bounds;
        match self.kind {
            0 => (x - left) * (x - left) + (y - top) * (y - top) <= 1.,
            1 => {
                let dx = x - (left + width * 0.5);
                let dy = y - (top + height * 0.5);
                dx * dx + dy * dy <= (width * 0.5) * (width * 0.5)
            }
            _ => {
                let mut inside = false;
                let mut previous = 3;
                for i in 0..4 {
                    let [xi, yi] = self.corners[i];
                    let [xj, yj] = self.corners[previous];
                    if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                        inside = !inside;
                    }
                    previous = i;
                }
                inside
            }
        }
    }
}
