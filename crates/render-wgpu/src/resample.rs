use crate::{
    blend::{PARAMETER_BYTES, Parameters},
    copy::{ImageSource, copy},
    gpu::{FORMAT, Gpu, Image},
};
use krkr_protocol::{
    graphics::{Rect, Size},
    transform::{Filter, ImageOperation, Sampling, StretchRect, Transform},
};
use krkr_render::{Result, resample::Axis, transform::Mapping};
use wgpu::util::DeviceExt;

impl Gpu {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resample(
        &self,
        image: &mut Image,
        source: &ImageSource,
        rect: Rect,
        dest: StretchRect,
        sampling: Sampling,
        operation: ImageOperation,
        clip: Rect,
    ) -> Result<()> {
        if sampling.filter == Filter::Area
            && (dest.width.unsigned_abs() > rect.width || dest.height.unsigned_abs() > rect.height)
        {
            return Ok(()); // Native area averaging only downsamples.
        }
        let Some(mapping) = Mapping::new(rect, Transform::Stretch(dest), clip)? else {
            return Ok(());
        };
        let region = mapping.bounds;
        let x = self.filter_axes.lock().unwrap().get(
            rect.left,
            rect.width,
            dest.left,
            dest.width,
            region.left,
            region.width,
            sampling,
            &self.staging,
        )?;
        let y = self.filter_axes.lock().unwrap().get(
            rect.top,
            rect.height,
            dest.top,
            dest.height,
            region.top,
            region.height,
            sampling,
            &self.staging,
        )?;
        let intermediate_size = Size {
            width: (x.range.end - x.range.start) as u32,
            height: region.height,
        };
        let intermediate = self.temporary(intermediate_size, FORMAT)?;
        let mut horizontal = Parameters::operation(region, operation);
        horizontal.0[7] |= 64;
        horizontal.0[0] = -x.range.start;
        horizontal.0[1] = -region.top;
        horizontal.0[24] = region.left;
        let backdrop = horizontal
            .needs_destination()
            .then(|| {
                self.temporary(
                    Size {
                        width: region.width,
                        height: region.height,
                    },
                    FORMAT,
                )
            })
            .transpose()?;
        let mut vertical = Parameters::operation(
            intermediate_size.rect(),
            ImageOperation::Copy { hold_alpha: false },
        );
        vertical.0[7] |= 32;
        vertical.0[0] = x.range.start;
        let mixer = self.mixer()?;
        // Each submission owns its uploads even when CPU coefficients are
        // shared with another in-flight command. Release only on completion.
        let upload_permit = self
            .staging
            .reserve(PARAMETER_BYTES * 4 + (x.data.len() + y.data.len()) * size_of::<f32>())?;
        let make_parameters = |parameters: &Parameters| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("resample parameters"),
                    contents: bytemuck::cast_slice(&parameters.0),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::STORAGE,
                })
        };
        let vertical_parameters = make_parameters(&vertical);
        let horizontal_parameters = make_parameters(&horizontal);
        let make_weights = |axis: &Axis| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("resample coefficients"),
                    contents: bytemuck::cast_slice(&axis.data),
                    usage: wgpu::BufferUsages::STORAGE,
                })
        };
        let x_weights = make_weights(&x);
        let y_weights = make_weights(&y);
        self.independent(image, true, false)?;
        let target = image.main()?;
        let source = source.main.as_ref().expect("validated main plane");
        let mut encoder = self.device.create_command_encoder(&Default::default());
        if let Some(backdrop) = &backdrop {
            copy(&mut encoder, target, backdrop, region, 0, 0);
        }
        // The vertical pass captures every source dependency before the
        // horizontal pass writes the target, including self-assignment.
        mixer.draw(
            self,
            &mut encoder,
            &intermediate,
            source,
            None,
            &vertical_parameters,
            0,
            intermediate_size.rect(),
            false,
            Some(&y_weights),
        );
        mixer.draw(
            self,
            &mut encoder,
            target,
            &intermediate,
            backdrop.as_deref(),
            &horizontal_parameters,
            0,
            region,
            horizontal.copies_color(),
            Some(&x_weights),
        );
        self.submit(
            encoder,
            (
                source.clone(),
                target.clone(),
                intermediate,
                backdrop,
                upload_permit,
            ),
        );
        self.check()
    }
}
