//! Atlas sprites and copy variants share native texture operations. The CPU
//! prepares coordinates only; pixels stay on the GPU after decoder upload.
use crate::{
    Error, Gpu, Image, Result,
    drawing::{Draw, Sample, face},
};
use krkr_protocol::{
    graphics::{Blend, BlendOptions, DrawFace, Fill, Rect, Size},
    pixels::Pixels,
    sprites::Sprites,
    transform::Transform,
};
use krkr_render::transform::{Mapping, validate_source};

impl Gpu {
    #[allow(clippy::too_many_arguments)]
    pub fn copy_wrapped(
        &self,
        target: &mut Image,
        source: &Image,
        rectangle: Rect,
        destination: Rect,
        shift: (i32, i32),
        clip: Rect,
    ) -> Result<()> {
        self.check_image(target)?;
        self.check_image(source)?;
        let Some(area) = destination
            .intersection(clip)
            .and_then(|clip| clip.intersection(target.size.rect()))
        else {
            return Ok(());
        };
        if rectangle.width == 0
            || rectangle.height == 0
            || rectangle.intersection(source.size.rect()) != Some(rectangle)
        {
            return Err(Error::Message("wrapped source rectangle is outside image"));
        }
        let plane = source.plane(false)?;
        self.writable(target, area, false)?;
        // Legacy wrapping uses absolute destination coordinates, not its
        // rectangle origin. Reduce signed shifts before uploading float values.
        self.draw(
            target.plane(false)?,
            Some(plane),
            area,
            &Draw {
                kind: 7.,
                color: [rectangle.width as f32, rectangle.height as f32, 0., 0.],
                ..Draw::copy(
                    [
                        plane.size.width as f32 / source.size.width as f32,
                        shift.0.rem_euclid(rectangle.width as i32) as f32,
                        rectangle.left as f32,
                        shift.1.rem_euclid(rectangle.height as i32) as f32,
                        plane.size.height as f32 / source.size.height as f32,
                        rectangle.top as f32,
                    ],
                    [true; 4],
                )
            },
        )
    }
    pub fn copy_pixels(
        &self,
        target: &mut Image,
        pixels: &Pixels,
        split_alpha: bool,
        limit: Size,
    ) -> Result<()> {
        self.check_image(target)?;
        target.plane(false)?;
        if pixels.main.is_none() || pixels.province.is_some() {
            return Err(Error::Message("movie frame must contain only RGBA"));
        }
        let width = pixels.size.width / if split_alpha { 2 } else { 1 };
        if width == 0 {
            return Err(Error::Message("movie frame width is empty"));
        }
        let size = Size {
            width: width.min(target.size.width).min(limit.width),
            height: pixels.size.height.min(target.size.height).min(limit.height),
        };
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        if !split_alpha
            && size == pixels.size
            && size == target.size
            && target.plane(false)?.size == size
        {
            // A complete RGBA movie frame already has the target's raster.
            // Upload it directly; upload's COW path preserves snapshots and
            // the unspecified province plane without a temporary GPU image.
            return self.upload(target, pixels);
        }
        let mut source = self.create_surface_image(pixels.size)?;
        self.upload(&mut source, pixels)?;
        self.copy_rect(
            target,
            &source,
            size.rect(),
            0,
            0,
            size.rect(),
            DrawFace::Alpha,
            false,
        )?;
        if split_alpha {
            self.draw_image(
                target,
                false,
                Some(source.plane(false)?),
                size.rect(),
                &Draw {
                    kind: 8.,
                    ..Draw::copy(
                        [1., 0., width as f32, 0., 1., 0.],
                        [false, false, false, true],
                    )
                },
            )?;
        }
        Ok(())
    }
    pub fn draw_sprites(
        &self,
        target: &mut Image,
        source: &Image,
        batch: &Sprites,
        clip: Rect,
        options: BlendOptions,
    ) -> Result<()> {
        self.check_image(target)?;
        self.check_image(source)?;
        let Some(clip) = clip.intersection(target.size.rect()) else {
            return Ok(());
        };
        let _metadata = self.staging.reserve(
            batch
                .sprites
                .len()
                .checked_mul(std::mem::size_of::<Option<Mapping>>())
                .ok_or(Error::Message("sprite metadata overflow"))?,
        )?;
        let mappings: smallvec::SmallVec<[_; 4]> = batch
            .sprites
            .iter()
            .map(|sprite| {
                if sprite.opacity == 0 || sprite.source.width == 0 || sprite.source.height == 0 {
                    return Ok(None);
                }
                validate_source(sprite.source, source.size)?;
                Mapping::new(sprite.source, Transform::Affine(sprite.points), clip)
            })
            .collect::<Result<_>>()?;
        {
            let _clear_metadata = self.staging.reserve(
                batch
                    .clear
                    .len()
                    .checked_mul(std::mem::size_of::<Fill>())
                    .ok_or(Error::Message("sprite clear metadata overflow"))?,
            )?;
            // Particle trails are cleared before any new sprite is drawn.
            // Submit them together so each target tile is visited once.
            let fills: smallvec::SmallVec<[_; 4]> = batch
                .clear
                .iter()
                .filter_map(|rectangle| rectangle.intersection(clip))
                .map(|rectangle| Fill {
                    rectangle,
                    color: 0,
                    face: options.face,
                    hold_alpha: options.hold_alpha,
                })
                .collect();
            self.fill(target, &fills)?;
        }
        let plane = source.plane(false)?;
        for (sprite, mapping) in batch.sprites.iter().zip(mappings) {
            let Some(mapping) = mapping else {
                continue;
            };
            self.writable_compact(target, mapping.bounds, false)?;
            let copy = options.mode == Blend::Opaque && sprite.opacity == 255;
            self.draw_image(
                target,
                false,
                Some(plane),
                mapping.bounds,
                &Draw {
                    kind: if copy {
                        if options.face == DrawFace::Opaque {
                            0.
                        } else {
                            6.
                        }
                    } else {
                        1.
                    },
                    color: [0.; 4],
                    operation: [
                        options.mode as i32 as f32,
                        face(options.face),
                        f32::from(sprite.opacity),
                        f32::from(options.hold_alpha),
                    ],
                    mask: [
                        true,
                        true,
                        true,
                        !(copy && options.face == DrawFace::Opaque && options.hold_alpha),
                    ],
                    mapping: mapping.inverse,
                    sampling: Some(Sample {
                        region: sprite.source,
                        bounds: sprite.source,
                        linear: false,
                        display: false,
                        sharpen: false,
                        clear: false,
                        scale: Some([
                            plane.size.width as f32 / source.size.width as f32,
                            plane.size.height as f32 / source.size.height as f32,
                        ]),
                    }),
                },
            )?;
        }
        Ok(())
    }
}
