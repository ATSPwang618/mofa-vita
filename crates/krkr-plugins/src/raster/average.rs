//! LayerExAreaAverage's 12-bit area integration (RGBA storage).
use super::*;
use krkr_engine::protocol::graphics::Size;

#[tjs_bind::function(resumable = true)]
pub(super) fn entry(
    cx: &mut NativeCx<'_>,
    args: tjs_bind::RestArgs<'_>,
) -> NativeResult<NativeStep> {
    begin(cx, args, true)
}
pub(super) fn clip(mut r: [i64; 8], dest: Size, source: Size) -> NativeResult<[i64; 8]> {
    if r[2] > r[6] || r[3] > r[7] {
        return Err(NativeError::Message("stretchCopyAA cannot enlarge images"));
    }
    // Negative coordinates and empty areas lead to invalid pointers/division in
    // C++; report them before accessing the managed buffer.
    if [0, 1, 4, 5].iter().any(|&i| r[i] < 0) || [2, 3, 6, 7].iter().any(|&i| r[i] <= 0) {
        return Err(NativeError::Message("invalid stretchCopyAA rectangle"));
    }
    for (d, s, dl, sl) in [
        (0, 4, dest.width, source.width),
        (1, 5, dest.height, source.height),
    ] {
        if r[d] >= i64::from(dl) || r[s] >= i64::from(sl) {
            return Err(NativeError::Message(
                "stretchCopyAA rectangle outside image",
            ));
        }
        if r[d] + r[d + 2] > i64::from(dl) {
            let n = i64::from(dl) - r[d];
            r[s + 2] = (r[s + 2] as f64 * (n as f64 / r[d + 2] as f64)) as i64;
            r[d + 2] = n;
        }
        if r[s] + r[s + 2] > i64::from(sl) {
            let n = i64::from(sl) - r[s];
            r[d + 2] = (r[d + 2] as f64 * (n as f64 / r[s + 2] as f64)) as i64;
            r[s + 2] = n;
        }
    }
    if [2, 3, 6, 7].iter().any(|&i| r[i] <= 0) {
        return Err(NativeError::Message(
            "empty stretchCopyAA rectangle after clipping",
        ));
    }
    Ok(r)
}
pub(super) fn row(
    out: &mut [u8],
    input: &[u8],
    same: bool,
    dest: Size,
    source: Size,
    r: [i64; 8],
    y: usize,
) {
    let [dl, dt, dw, dh, sl, st, sw, sh] = r;
    let rw = (sw as f64 / dw as f64 * 4096.) as i64;
    let rh = (sh as f64 / dh as f64 * 4096.) as i64;
    let y1 = st * 4096 + y as i64 * rh;
    let y2 = y1 + rh;
    let mut target = ((dt as usize + y) * dest.width as usize + dl as usize) * 4;
    for x in 0..dw {
        let x1 = sl * 4096 + x * rw;
        let x2 = x1 + rw;
        // Preserve the reference's exclusive size-1 bound, including its
        // omitted last row/column and empty-area output-pointer behavior.
        let ex = ((x2 + 4095) >> 12).min(i64::from(source.width) - 1);
        let ey = ((y2 + 4095) >> 12).min(i64::from(source.height) - 1);
        let mut sums = [[0i32; 3]; 2];
        let mut weights = [0i32; 2];
        let mut alpha = 0i32;
        let mut total = 0i32;
        for ay in (y1 >> 12)..ey {
            let ah = ((ay + 1) * 4096).min(y2) - (ay * 4096).max(y1);
            for ax in (x1 >> 12)..ex {
                let aw = ((ax + 1) * 4096).min(x2) - (ax * 4096).max(x1);
                let mut area = ((aw * ah) >> 12) as i32;
                let i = (ay as usize * source.width as usize + ax as usize) * 4;
                let p = if same {
                    &out[i..i + 4]
                } else {
                    &input[i..i + 4]
                };
                total = total.wrapping_add(area);
                alpha = alpha.wrapping_add(i32::from(p[3]).wrapping_mul(area));
                let bucket = usize::from(p[3] == 0);
                if bucket == 0 {
                    area = ((area as u32 * u32::from(p[3])) >> 8) as i32;
                }
                weights[bucket] = weights[bucket].wrapping_add(area);
                for (sum, &v) in sums[bucket].iter_mut().zip(&p[..3]) {
                    *sum = sum.wrapping_add(i32::from(v).wrapping_mul(area));
                }
            }
        }
        if total == 0 {
            continue;
        }
        let bucket = usize::from(weights[0] == 0);
        for (v, sum) in out[target..target + 3].iter_mut().zip(sums[bucket]) {
            *v = if weights[bucket] == 0 {
                0
            } else {
                sum.wrapping_div(weights[bucket]) as u8
            };
        }
        out[target + 3] = alpha.wrapping_div(total) as u8;
        target += 4;
    }
}
