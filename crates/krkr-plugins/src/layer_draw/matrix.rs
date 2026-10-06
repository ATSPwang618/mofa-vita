//! GdipMatrix behavior from LayerExDraw.hpp, with plutovg's row-vector product.
#[derive(Clone, Copy, Debug)]
pub struct Matrix {
    pub elements: [f32; 6],
    pub offset: [f64; 2],
}
impl Default for Matrix {
    fn default() -> Self {
        Self::new([1., 0., 0., 1., 0., 0.])
    }
}
impl Matrix {
    pub fn new(elements: [f32; 6]) -> Self {
        Self {
            offset: [f64::from(elements[4]), f64::from(elements[5])],
            elements,
        }
    }
    pub fn equals(&self, other: &Self) -> bool {
        self.elements
            .iter()
            .zip(other.elements)
            .all(|(a, b)| a.to_bits() == b.to_bits())
    }
    pub fn product(left: [f32; 6], right: [f32; 6]) -> [f32; 6] {
        let [a, b, c, d, e, f] = left;
        let [g, h, i, j, k, l] = right;
        [
            a * g + b * i,
            a * h + b * j,
            c * g + d * i,
            c * h + d * j,
            e * g + f * i + k,
            e * h + f * j + l,
        ]
    }
    fn update_offset(&mut self) {
        self.offset = [f64::from(self.elements[4]), f64::from(self.elements[5])];
    }
    pub fn multiply(&mut self, other: &Self, order: i32) {
        self.elements = if order == 0 {
            Self::product(self.elements, other.elements)
        } else {
            Self::product(other.elements, self.elements)
        };
        self.update_offset();
    }
    pub fn inverse(&self) -> Option<Self> {
        let [a, b, c, d, e, f] = self.elements;
        let determinant = a * d - b * c;
        if determinant == 0. {
            return None;
        }
        let inv = 1. / determinant;
        Some(Self::new([
            d * inv,
            -(b * inv),
            -(c * inv),
            a * inv,
            (c * f - d * e) * inv,
            (b * e - a * f) * inv,
        ]))
    }
    pub fn rotate(&mut self, degrees: f64, _order: i32) {
        let rad = degrees.to_radians() as f32;
        let (s, c) = rad.sin_cos();
        // Both source branches result in product(rotation, current).
        self.elements = Self::product([c, s, -s, c, 0., 0.], self.elements);
        self.update_offset();
    }
    pub fn scale(&mut self, x: f64, y: f64, _order: i32) {
        self.elements = Self::product([x as f32, 0., 0., y as f32, 0., 0.], self.elements);
        self.update_offset();
    }
    pub fn translate(&mut self, x: f64, y: f64, _order: i32) {
        // The wrapper exposes accumulated offsets here, not the composed e/f.
        // Rotate/Scale/Multiply subsequently refresh them from the matrix.
        self.offset[0] += x;
        self.offset[1] += y;
        self.elements = Self::product([1., 0., 0., 1., x as f32, y as f32], self.elements);
    }
    pub fn shear(&mut self, x: f64, y: f64, order: i32) {
        let (x, y) = if order == 0 {
            ((x as f32).tan(), (y as f32).tan())
        } else {
            (x as f32, y as f32)
        };
        self.elements = Self::product([1., y, x, 1., 0., 0.], self.elements);
        self.update_offset();
    }
    pub fn rotate_at(&mut self, degrees: f64, center: [f64; 2], order: i32) {
        if order == 0 {
            self.translate(-center[0], -center[1], 0);
            self.rotate(degrees, 0);
            self.translate(center[0], center[1], 0);
        } else {
            let mut transform = Self::default();
            transform.translate(-center[0], -center[1], 0);
            transform.rotate(degrees, 0);
            transform.translate(center[0], center[1], 0);
            self.elements = Self::product(transform.elements, self.elements);
        }
        self.update_offset();
    }
}
