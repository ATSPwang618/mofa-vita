//! Small legacy lookup data, shared by GPU backends. Channel order is soft
//! light, PS dodge, PS burn, then source-over opacity factor (destination x,
//! source y). The float evaluation follows TVP's table construction.
/// Legacy source-over color weight for a 65- or 256-level glyph mask.
/// Keep the same floating-point evaluation as the GPU lookup table.
#[inline]
pub fn opacity_factor(destination: u8, coverage: u8, levels: u16) -> u8 {
    if destination == 0 {
        return 255;
    }
    let at = (f64::from(destination) / 255.0) as f32;
    let bt = (f64::from(coverage) / if levels == 65 { 64.0 } else { 255.0 }) as f32;
    let mut c = bt / at;
    c /= (1.0 - f64::from(bt) + f64::from(c)) as f32;
    (c * 255.0) as u8
}

pub fn lookup_table() -> Vec<u8> {
    let mut table = vec![0; 256 * (256 + 65) * 4];
    for s in 0..256u32 {
        for d in 0..256u32 {
            let i = ((s * 256 + d) * 4) as usize;
            let power = if s >= 128 {
                128.0 / s as f64
            } else {
                (1.0 - s as f64 / 255.0) / 0.5
            };
            table[i] = ((d as f64 / 255.0).powf(power) * 255.0) as u8;
            table[i + 1] = if 255 - s <= d {
                255
            } else {
                (d * 255 / (255 - s)) as u8
            };
            table[i + 2] = if s <= 255 - d {
                0
            } else {
                (255 - (255 - d) * 255 / s) as u8
            };
            table[i + 3] = opacity_factor(d as u8, s as u8, 256);
        }
    }
    for s in 0..65usize {
        for d in 0..256usize {
            table[((256 + s) * 256 + d) * 4 + 3] = opacity_factor(d as u8, s as u8, 65);
        }
    }
    table
}
