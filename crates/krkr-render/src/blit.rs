use crate::{Rect, Size};

/// Clip both sides together. Trimming the source moves the destination by the
/// same amount, and trimming the destination advances the source correspondingly.
pub fn region(
    source: Size,
    destination: Size,
    clip: Rect,
    rectangle: Rect,
    x: i32,
    y: i32,
) -> Option<(Rect, Rect)> {
    let source_clip = rectangle.intersection(source.rect())?;
    let destination_clip = clip.intersection(destination.rect())?;
    let dx = i64::from(x) + i64::from(source_clip.left) - i64::from(rectangle.left);
    let dy = i64::from(y) + i64::from(source_clip.top) - i64::from(rectangle.top);
    let left = dx.max(i64::from(destination_clip.left));
    let top = dy.max(i64::from(destination_clip.top));
    let right = (dx + i64::from(source_clip.width))
        .min(i64::from(destination_clip.left) + i64::from(destination_clip.width));
    let bottom = (dy + i64::from(source_clip.height))
        .min(i64::from(destination_clip.top) + i64::from(destination_clip.height));
    if left >= right || top >= bottom {
        return None;
    }
    let (width, height) = ((right - left) as u32, (bottom - top) as u32);
    Some((
        Rect {
            left: (i64::from(source_clip.left) + left - dx) as i32,
            top: (i64::from(source_clip.top) + top - dy) as i32,
            width,
            height,
        },
        Rect {
            left: left as i32,
            top: top as i32,
            width,
            height,
        },
    ))
}
