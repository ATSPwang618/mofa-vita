//! Resolve sampling from source SPS geometry without guessing native crop units.
use crate::movie_geometry::VideoGeometry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeFrameLayout { Coded, Visible }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoPresentation {
    pub visible_width: u32,
    pub visible_height: u32,
    pub uv: [f32; 4],
    pub display_aspect: f32,
    pub native_layout: NativeFrameLayout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewError { Geometry, Storage, Aspect }
impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "movie source/native geometry: {self:?}")
    }
}
impl std::error::Error for ViewError {}

/// Width/height here describe the actual bound native texture. The encoded
/// source supplies pixel cropping and sample aspect; native Ex values remain
/// raw observations and are never silently treated as SPS crop syntax or DAR.
pub fn resolve(source: VideoGeometry, storage: [u32; 2]) -> Result<VideoPresentation, ViewError> {
    let VideoGeometry { coded_width, coded_height, crop_left, crop_top,
        crop_right, crop_bottom, visible_width, visible_height, sar } = source;
    if coded_width == 0 || coded_height == 0 || coded_width > 960 || coded_height > 544
        || [coded_width, coded_height, crop_left, crop_top, crop_right, crop_bottom]
            .iter().any(|value| value % 2 != 0)
        || coded_width.checked_sub(crop_left).and_then(|n| n.checked_sub(crop_right)) != Some(visible_width)
        || coded_height.checked_sub(crop_top).and_then(|n| n.checked_sub(crop_bottom)) != Some(visible_height)
        || visible_width == 0 || visible_height == 0
    {
        return Err(ViewError::Geometry);
    }
    let sar = sar.filter(|n| n.numerator != 0 && n.denominator != 0)
        .ok_or(ViewError::Aspect)?;
    let display_aspect = ((visible_width as f64 * sar.numerator as f64)
        / (visible_height as f64 * sar.denominator as f64)) as f32;
    if !display_aspect.is_finite() || display_aspect <= 0.0 {
        return Err(ViewError::Aspect);
    }
    let (native_layout, uv) = if storage == [coded_width, coded_height] {
        (NativeFrameLayout::Coded, [crop_left as f32 / coded_width as f32,
            crop_top as f32 / coded_height as f32,
            (coded_width - crop_right) as f32 / coded_width as f32,
            (coded_height - crop_bottom) as f32 / coded_height as f32])
    } else if storage == [visible_width, visible_height] {
        (NativeFrameLayout::Visible, [0.0, 0.0, 1.0, 1.0])
    } else {
        return Err(ViewError::Storage);
    };
    Ok(VideoPresentation { visible_width, visible_height, uv, display_aspect, native_layout })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movie_geometry::SampleAspectRatio;

    fn source() -> VideoGeometry {
        VideoGeometry { coded_width: 960, coded_height: 544, crop_left: 0, crop_top: 0,
            crop_right: 0, crop_bottom: 4, visible_width: 960, visible_height: 540,
            sar: Some(SampleAspectRatio { numerator: 1, denominator: 1 }) }
    }

    #[test]
    fn coded_texture_excludes_alignment_rows_and_preserves_display_aspect() {
        let view = resolve(source(), [960, 544]).unwrap();
        assert_eq!(view.native_layout, NativeFrameLayout::Coded);
        assert_eq!([view.visible_width, view.visible_height], [960, 540]);
        assert_eq!(view.uv, [0.0, 0.0, 1.0, 540.0 / 544.0]);
        assert!((view.display_aspect - 16.0 / 9.0).abs() < 0.00001);
        // The last visible pixel centre is sampled below the first padding row.
        let last_sample_row = ((539.5 / 540.0) * view.uv[3]) * 544.0;
        assert!((last_sample_row - 539.5).abs() < 0.0001);
    }

    #[test]
    fn decoder_already_returning_visible_pixels_is_not_cropped_twice() {
        let view = resolve(source(), [960, 540]).unwrap();
        assert_eq!(view.native_layout, NativeFrameLayout::Visible);
        assert_eq!(view.uv, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn nonzero_origin_and_anamorphic_sar_have_independent_meanings() {
        let mut input = source();
        input.crop_left = 8;
        input.crop_right = 12;
        input.crop_top = 2;
        input.crop_bottom = 6;
        input.visible_width = 940;
        input.visible_height = 536;
        input.sar = Some(SampleAspectRatio { numerator: 4, denominator: 3 });
        let view = resolve(input, [960, 544]).unwrap();
        assert_eq!(view.uv, [8.0 / 960.0, 2.0 / 544.0, 948.0 / 960.0, 538.0 / 544.0]);
        assert!((view.display_aspect - (940.0 * 4.0 / (536.0 * 3.0))).abs() < 0.00001);
    }

    #[test]
    fn unexpected_native_geometry_and_inconsistent_sources_fail_closed() {
        for storage in [[0, 0], [960, 542], [1920, 1088], [959, 544]] {
            assert_eq!(resolve(source(), storage), Err(ViewError::Storage));
        }
        for (left, bottom, visible) in [(u32::MAX, 4, 540), (960, 4, 540), (0, 544, 0), (0, 2, 540)] {
            let mut input = source();
            input.crop_left = left;
            input.crop_bottom = bottom;
            input.visible_height = visible;
            assert_eq!(resolve(input, [960, 544]), Err(ViewError::Geometry));
        }
    }

    #[test]
    fn unspecified_or_invalid_sample_aspect_never_becomes_a_guessed_dar() {
        for sar in [None, Some(SampleAspectRatio { numerator: 0, denominator: 1 }),
            Some(SampleAspectRatio { numerator: 1, denominator: 0 })] {
            let mut input = source();
            input.sar = sar;
            assert_eq!(resolve(input, [960, 544]), Err(ViewError::Aspect));
        }
    }
}
