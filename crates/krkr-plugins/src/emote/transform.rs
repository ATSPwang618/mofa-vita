use super::{
    mesh::{Matrix, Surface},
    sample::Sample,
};
#[derive(Clone)]
pub(super) struct Transform {
    pub coord: [f32; 3],
    pub camera: [f32; 2],
    pub angle: f32,
    pub zoom: [f32; 2],
    pub affine: Matrix,
    pub size: [f32; 2],
    pub viewport: Option<[f32; 2]>,
    pub origin: [f32; 2],
    pub depth: f32,
    pub root: Option<Surface>,
}
impl Default for Transform {
    fn default() -> Self {
        Self {
            coord: [0.; 3],
            camera: [0.; 2],
            angle: 0.,
            zoom: [1.; 2],
            affine: Matrix::default(),
            size: [0.; 2],
            viewport: None,
            origin: [0.; 2],
            depth: 30.,
            root: None,
        }
    }
}
impl Transform {
    pub fn update(&mut self) {
        let [w, h] = self.size;
        if w == 0. || h == 0. {
            self.root = None;
            return;
        }
        let mut projection = Matrix::default();
        projection.0[0] = 2. / w;
        projection.0[5] = -2. / h;
        projection.0[10] = 1. / self.depth;
        projection.0[12] = 2. * self.origin[0] / w - 1.;
        projection.0[13] = 1. - 2. * self.origin[1] / h;
        self.root = Some(Surface {
            kind: 3,
            inherit: 0x0fffffff,
            attach: projection.multiply(self.affine),
            size: self.size,
            origin: self.origin,
            sample: Sample {
                frame: 0,
                coord: [
                    (self.coord[0] + self.camera[0]) as f64,
                    (self.coord[1] + self.camera[1]) as f64,
                    0.,
                ],
                opacity: 1.,
                angle: self.angle as f64,
                slant: [0.; 2],
                zoom: self.zoom.map(f64::from),
                origin: [0.; 2],
                time_offset: 0.,
                mesh: None,
            },
        });
    }
    pub fn resize(&mut self, size: [f32; 2], depth: f32) {
        if self.size != size {
            self.size = size;
            self.origin = [0.; 2];
            self.depth = depth.max(30.);
            self.update();
        }
    }
    pub fn serialized(&self) -> [f64; 6] {
        [
            self.coord[0] as f64,
            self.coord[1] as f64,
            self.coord[2] as f64,
            self.angle as f64,
            self.zoom[0] as f64,
            self.zoom[1] as f64,
        ]
    }
    pub fn restore_field(&mut self, index: usize, value: f64) {
        match index {
            0..=2 => self.coord[index] = value as f32,
            3 => self.angle = value as f32,
            4..=5 => self.zoom[index - 4] = value as f32,
            _ => unreachable!(),
        }
        // unserialize in the reference restores fields without rebuilding the matrix.
    }
}
pub(super) const FIELDS: [&str; 6] = [
    "currCoordx",
    "currCoordy",
    "currCoordz",
    "currAngle",
    "currZx",
    "currZy",
];
