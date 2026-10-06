use super::*;
#[cfg(test)]
#[path = "../tests/internal/tiles.rs"]
mod tests;
pub fn color_key(
    data: &mut [u8],
    size: Size,
    key: u32,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<()> {
    let selected = if key == 0x01ffffff {
        let _permit = reserve(budget, size.width as usize * 4)?;
        let mut row: Vec<u32> = data[..size.width as usize * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| rgb(pixel))
            .collect();
        row.sort_unstable();
        // Preserve the legacy adaptive-key scan, including its run-count and
        // tie behavior, after sorting the first scanline by RGB.
        let (mut previous, mut run, mut longest, mut color) = (u32::MAX, 0, 0, 0);
        for value in row.into_iter().chain([u32::MAX]) {
            if value != previous {
                if longest < run {
                    longest = run;
                    color = previous;
                    run = 0;
                }
            } else {
                run += 1;
            }
            previous = value;
        }
        Some(if color == u32::MAX { 0 } else { color })
    } else if key >> 24 == 0 {
        Some(key)
    } else {
        None
    };
    if let Some(key) = selected {
        for row in data.chunks_mut(size.width as usize * 4) {
            check(cancelled)?;
            for pixel in row.as_chunks_mut::<4>().0.iter_mut() {
                pixel[3] = if rgb(pixel) == key { 0 } else { 255 };
            }
        }
    }
    Ok(())
}
fn rgb(p: &[u8]) -> u32 {
    u32::from_be_bytes([0, p[0], p[1], p[2]])
}
pub fn matte(data: &mut [u8], key: u32, cancelled: &AtomicBool) -> Result<()> {
    if key & 0xff000000 != 0x04000000 {
        return Ok(());
    }
    let base = [
        (key >> 16 & 255) as i32,
        (key >> 8 & 255) as i32,
        (key & 255) as i32,
    ];
    for block in data.chunks_mut(64 * 1024) {
        check(cancelled)?;
        for pixel in block.as_chunks_mut::<4>().0.iter_mut() {
            for c in 0..3 {
                pixel[c] = (base[c]
                    + (((i32::from(pixel[c]) - base[c]) * i32::from(pixel[3])) >> 8))
                    as u8;
            }
            pixel[3] = 255;
        }
    }
    Ok(())
}
pub fn tile(
    source: Bytes,
    from: Size,
    to: Size,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<Bytes> {
    if from.width == 0 || from.height == 0 {
        return Err(Error::Message("empty province tile"));
    }
    if from == to {
        return Ok(source);
    }
    let mut output = Bytes::zeroed(
        to.rgba_bytes()
            .ok_or(Error::Message("province size overflow"))?
            / 4,
        budget,
    )?;
    let width = to.width as usize;
    for y in 0..to.height as usize {
        check(cancelled)?;
        let start = y * width;
        if y >= from.height as usize {
            let previous = (y - from.height as usize) * width;
            output
                .as_mut_slice()
                .copy_within(previous..previous + width, start);
            continue;
        }
        let row = &mut output.as_mut_slice()[start..start + width];
        let offset = y * from.width as usize;
        let seed = width.min(from.width as usize);
        row[..seed].copy_from_slice(&source.as_slice()[offset..offset + seed]);
        let mut filled = seed;
        while filled < width {
            let count = filled.min(width - filled);
            row.copy_within(..count, filled);
            filled += count;
        }
    }
    Ok(output)
}
pub fn gray(r: u8, g: u8, b: u8) -> u8 {
    ((u32::from(r) * 54 + u32::from(g) * 183 + u32::from(b) * 19) >> 8) as u8
}
