use super::*;
pub(super) const PI: f64 = 3.141_592_635_897_932_3;
pub(super) fn option(
    size: Size,
    index: usize,
    previous: &[Value],
    value: Value,
) -> NativeResult<()> {
    let number = |v, default| match v {
        Value::Int(v) => v as f64,
        Value::Real(v) => v,
        _ => default,
    };
    let (default, message) = match index {
        1 => (
            f64::from(size.height / 2),
            "centerx and centery cannot be out of the image",
        ),
        2 => (128.0, "rwidth must be 16, 32, 64 or 128"),
        3 => (1.0, "roundness must be finite and positive"),
        4 => (6.0, "ripple speed exceeds float range"),
        5 => (
            24.0,
            "maxdrift must be 0..127 and less than both image dimensions",
        ),
        _ => return Ok(()),
    };
    let n = number(value, default);
    let valid = match index {
        1 => {
            let x = number(previous[0], f64::from(size.width / 2));
            x >= 0.0 && x < f64::from(size.width) && n >= 0.0 && n < f64::from(size.height)
        }
        2 => [16.0, 32.0, 64.0, 128.0].contains(&n),
        3 => n as f32 > 0.0 && (n as f32).is_finite(),
        4 => (n as f32).is_finite(),
        5 => (0.0..128.0).contains(&n) && n < f64::from(size.width.min(size.height)),
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(NativeError::Message(message))
    }
}
pub(super) fn validate(size: Size, v: &[f64; 7]) -> NativeResult<()> {
    if v[0] < 0.0 || v[1] < 0.0 || v[0] >= f64::from(size.width) || v[1] >= f64::from(size.height) {
        return Err(NativeError::Message(
            "centerx and centery cannot be out of the image",
        ));
    }
    if ![16.0, 32.0, 64.0, 128.0].contains(&v[2]) {
        return Err(NativeError::Message("rwidth must be 16, 32, 64 or 128"));
    }
    if v[3] as f32 <= 0.0 || !(v[3] as f32).is_finite() {
        return Err(NativeError::Message(
            "roundness must be finite and positive",
        ));
    }
    if !(v[4] as f32).is_finite() {
        return Err(NativeError::Message("ripple speed exceeds float range"));
    }
    if v[5] < 0.0 || v[5] >= 128.0 || v[5] >= f64::from(size.width.min(size.height)) {
        return Err(NativeError::Message(
            "maxdrift must be 0..127 and less than both image dimensions",
        ));
    }
    Ok(())
}
pub(super) fn table(size: Size, v: [f64; 7], budget: &Budget) -> Result<Arc<Bytes>, String> {
    let cx = v[0] as u32;
    let cy = v[1] as u32;
    let rw = v[2] as usize;
    let width = size.width.max(cx * 2) - cx;
    let height = size.height.max(cy * 2) - cy;
    let count = width as usize * height as usize;
    super::table((count + rw + 64) * 4, budget, |data| {
        for y in 0..height as usize {
            let yy = (y as f32 + 0.5) * v[3] as f32;
            let fac = 1.0 / yy;
            for x in 0..width as usize {
                let xx = x as f32 + 0.5;
                let dir = (f64::from(xx * fac).atan() * (1.0 / (PI / 2.0) * 32.0)) as i32;
                let dist = (f64::from(xx * xx + yy * yy).sqrt() as i32) & (rw as i32 - 1);
                put(data, y * width as usize + x, dist * 32 + dir);
            }
        }
        let reciprocal = 1.0 / rw as f32;
        for w in 0..rw {
            let rad = (f64::from(w as f32 * reciprocal) * (PI * -2.0)) as f32;
            let mut s =
                ((f64::from(rad).sin() + f64::from(rad * 2.0 - 2.0).sin() * 0.2) / 1.19) as f32;
            s *= s;
            s = s.clamp(-1.0, 1.0) * 2048.0;
            put(
                data,
                count + w,
                (s + if s < 0.0 { -0.5 } else { 0.5 }) as i32,
            );
        }
        for direction in 0..32 {
            let rad =
                (PI * 0.5 - f64::from(direction as f32 + 0.5) * ((1.0 / 32.0) * (PI / 2.0))) as f32;
            for (i, value) in [f64::from(rad).cos(), f64::from(rad).sin()]
                .into_iter()
                .enumerate()
            {
                let value = (value * 2048.0) as f32;
                put(
                    data,
                    count + rw + i * 32 + direction,
                    (value + if value < 0.0 { -0.5 } else { 0.5 }) as i32,
                );
            }
        }
    })
}
