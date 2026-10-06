use crate::{
    blend::{PARAMETER_BYTES, Parameters},
    copy::{ImageSource, copy},
    gpu::{FORMAT, Gpu, Image},
};
use krkr_protocol::{
    graphics::{Blend, BlendOptions, DrawFace, Fill, Rect, Size},
    transform::{Filter, ImageOperation, Sampling, Transform},
};
use krkr_render::{
    Error, Result,
    transform::{Mapping, validate_source},
};
use std::sync::Arc;
use wgpu::util::DeviceExt;

impl Parameters {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn affine(
        destination: Rect,
        mapping: &Mapping,
        rectangle: Rect,
        bounds: Rect,
        linear: bool,
        operation: ImageOperation,
        clear: Option<u32>,
    ) -> Self {
        let mut parameters = Self::operation(destination, operation);
        parameters.0[7] |= 4;
        if linear {
            parameters.0[7] |= 8;
        }
        if let Some(color) = clear {
            parameters.0[7] |= 16;
            parameters.0[8..12].copy_from_slice(&[
                (color >> 16 & 255) as i32,
                (color >> 8 & 255) as i32,
                (color & 255) as i32,
                (color >> 24) as i32,
            ]);
        }
        for (indices, values) in [
            ((12..15), &mapping.inverse[..3]),
            ((16..19), &mapping.inverse[3..]),
        ] {
            for (slot, value) in parameters.0[indices].iter_mut().zip(values) {
                *slot = value.to_bits() as i32;
            }
        }
        for (offset, rect) in [(20, rectangle), (24, bounds)] {
            parameters.0[offset..offset + 4].copy_from_slice(&[
                rect.left,
                rect.top,
                rect.left + rect.width as i32,
                rect.top + rect.height as i32,
            ]);
        }
        parameters
    }
    pub(crate) fn operation(rect: Rect, operation: ImageOperation) -> Self {
        let mut p = Self::new(
            (0, 0),
            rect,
            match operation {
                ImageOperation::Blend(options) => options,
                ImageOperation::Copy { hold_alpha } => BlendOptions {
                    mode: Blend::Opaque,
                    face: DrawFace::Opaque,
                    opacity: 255,
                    hold_alpha,
                },
            },
        );
        if let ImageOperation::Copy { hold_alpha } = operation {
            p.0[4] = if hold_alpha { -2 } else { -1 };
        }
        p
    }
}
impl Gpu {
    #[allow(clippy::too_many_arguments)]
    pub fn transform(
        &self,
        image: &mut Image,
        source: &ImageSource,
        rectangle: Rect,
        transform: Transform,
        sampling: Sampling,
        operation: ImageOperation,
        clip: Rect,
        clear: Option<u32>,
    ) -> Result<()> {
        if self.defer_transform(
            image, source, rectangle, transform, sampling, operation, clip, clear,
        )? {
            return Ok(());
        }
        if source.main.is_none() && source.deferred.is_none() {
            return Err(Error::Message("source has no main plane"));
        }
        if !image.has_main() {
            return Err(Error::Message("image has no main plane"));
        }
        if rectangle.width == 0 || rectangle.height == 0 {
            return Ok(());
        }
        let Some(clip) = clip.intersection(image.size.rect()) else {
            return Ok(());
        };
        if operation.is_noop() {
            return Ok(());
        }
        if let Transform::Stretch(dest) = transform
            && dest.width as i64 == rectangle.width as i64
            && dest.height as i64 == rectangle.height as i64
            && (Rect {
                left: dest.left,
                top: dest.top,
                width: rectangle.width,
                height: rectangle.height,
            })
            .intersection(clip)
                == Some(Rect {
                    left: dest.left,
                    top: dest.top,
                    width: rectangle.width,
                    height: rectangle.height,
                })
        {
            return match operation {
                ImageOperation::Copy { hold_alpha } => self.copy_rect(
                    image,
                    source,
                    rectangle,
                    dest.left,
                    dest.top,
                    clip,
                    if hold_alpha {
                        DrawFace::Opaque
                    } else {
                        DrawFace::Alpha
                    },
                    hold_alpha,
                ),
                ImageOperation::Blend(options) => {
                    self.operate_rect(image, source, rectangle, dest.left, dest.top, clip, options)
                }
            };
        }
        validate_source(rectangle, source.size)?;
        let mapping = Mapping::new(rectangle, transform, clip)?;
        // High-quality stretch filters are separable; affine sampling retains
        // the original nearest/linear dispatch and pixel-center convention.
        if let Transform::Stretch(dest) = transform
            && !matches!(sampling.filter, Filter::Nearest | Filter::FastLinear)
        {
            let resolved = self.materialized_source(source)?;
            let source = resolved.as_ref().unwrap_or(source);
            self.materialize(image)?;
            return self.resample(image, source, rectangle, dest, sampling, operation, clip);
        }
        if !operation.affine_supported() {
            return Ok(());
        }
        let Some(mapping) = mapping else {
            return if let Some(color) = clear {
                let hold_alpha = matches!(operation, ImageOperation::Copy { hold_alpha: true });
                self.fill(
                    image,
                    &[Fill {
                        rectangle: clip,
                        color,
                        face: if hold_alpha {
                            DrawFace::Opaque
                        } else {
                            DrawFace::Alpha
                        },
                        hold_alpha,
                    }],
                )
            } else {
                Ok(())
            };
        };
        let destination = if clear.is_some() {
            clip
        } else {
            mapping.bounds
        };
        let bounds = if sampling.no_clip {
            source.size.rect()
        } else {
            rectangle
        };
        let mut parameters = Parameters::affine(
            destination,
            &mapping,
            rectangle,
            bounds,
            sampling.filter != Filter::Nearest && operation.affine_linear(),
            operation,
            clear,
        );
        let footprint = mapping.source_region(rectangle, bounds, parameters.0[7] & 8 != 0);
        let resolved = source
            .deferred
            .as_ref()
            .map(|_| self.resolve_main(&source.snapshot_main(), footprint))
            .transpose()?;
        let source_plane = if let Some(region) = &resolved {
            parameters.0[0] = region.rectangle.left;
            parameters.0[1] = region.rectangle.top;
            &region.allocation
        } else {
            source.main.as_ref().expect("validated main source")
        };
        self.materialize(image)?;
        let mixer = self.mixer()?;
        let backdrop = parameters
            .needs_destination()
            .then(|| {
                self.temporary(
                    Size {
                        width: destination.width,
                        height: destination.height,
                    },
                    FORMAT,
                )
            })
            .transpose()?;
        let alias =
            Arc::strong_count(&image.main_owners) == 1 && Arc::ptr_eq(source_plane, image.main()?);
        // Snapshot only the visible source footprint and filter neighbors.
        let snapshot = alias
            .then(|| {
                self.temporary(
                    Size {
                        width: footprint.width,
                        height: footprint.height,
                    },
                    FORMAT,
                )
            })
            .transpose()?;
        let permit = self.staging.reserve(PARAMETER_BYTES * 2)?;
        if clear.is_some()
            && clip == image.size.rect()
            && matches!(operation, ImageOperation::Copy { hold_alpha: false })
        {
            // Every destination channel will be replaced, including pixels
            // outside the affine footprint. A detached version needs no copy.
            self.independ_image(image, false, false)?;
        } else {
            self.independent(image, true, false)?;
        }
        let target = image.main()?;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        if let Some(backdrop) = &backdrop {
            copy(&mut encoder, target, backdrop, destination, 0, 0);
        }
        let input = if let Some(snapshot) = &snapshot {
            copy(&mut encoder, source_plane, snapshot, footprint, 0, 0);
            parameters.0[0] = footprint.left;
            parameters.0[1] = footprint.top;
            snapshot
        } else {
            source_plane
        };
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("affine parameters"),
                contents: bytemuck::cast_slice(&parameters.0),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::STORAGE,
            });
        mixer.draw(
            self,
            &mut encoder,
            target,
            input,
            backdrop.as_deref(),
            &buffer,
            0,
            destination,
            parameters.copies_color(),
            None,
        );
        self.submit(
            encoder,
            (
                target.clone(),
                source_plane.clone(),
                snapshot,
                backdrop,
                permit,
            ),
        );
        self.check()
    }
}
