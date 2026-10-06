//! An exclusive main-plane grant reuses one parameter buffer and command
//! encoder for fills, copies and blends. GPU objects stay on the host; the
//! protocol carries only commands, image leases and a prepaid budget permit.
use crate::{
    blend::{PARAMETER_BYTES, Parameters},
    copy::{ImageSource, copy},
    gpu::{Allocation, FORMAT, Gpu, Image, rgba},
};
use krkr_protocol::graphics::{
    Blend, DRAW_BATCH_CAPACITY, DrawFace, ImageRef, PreparedDraw, PreparedDraws, Rect,
};
use krkr_render::{Error, Result, blit};
use std::sync::{Arc, Weak};
use wgpu::util::DeviceExt;

pub struct DrawPreparation {
    lifetime: Weak<()>,
    target: Arc<Allocation>,
    source: Option<ImageSource>,
    backdrop: Option<Arc<Allocation>>,
}
impl DrawPreparation {
    pub fn active(&self) -> bool {
        self.lifetime.strong_count() != 0
    }
}

enum Encoded {
    Fill(Rect, usize),
    Copy(Rect, Rect, usize),
    Blend(Rect, Parameters),
}

impl Gpu {
    pub fn prepare_draws(
        &self,
        image: &Image,
        reference: ImageRef,
        source: Option<(&Image, ImageRef)>,
    ) -> Option<(PreparedDraws, DrawPreparation)> {
        if image.main_write_bytes() != 0 || Image::prefers_deferred(image.size) {
            return None;
        }
        let target = image.main.as_ref()?;
        // A different logical ID can still share this allocation. Such copies
        // require an alias snapshot and must go through ordinary admission.
        let source = source.filter(|(input, id)| {
            id.id != reference.id && input.main.as_ref().is_some_and(|s| !Arc::ptr_eq(s, target))
        });
        // Only reserve an optional backdrop when it fits now. Preparing an
        // optimization must not wait for GPU retirement or evict useful images.
        let backdrop = if source.is_some()
            && image.size.rgba_bytes()? <= self.scratch.available()
            && self.mixer.get().is_some()
        {
            self.temporary(image.size, FORMAT).ok()
        } else {
            None
        };
        let stride = PARAMETER_BYTES
            .next_multiple_of(self.device.limits().min_uniform_buffer_offset_alignment as usize);
        let bytes = DRAW_BATCH_CAPACITY * (stride * 2 + std::mem::size_of::<Option<Encoded>>());
        let batch = PreparedDraws::reserve(
            reference,
            image.size,
            source.as_ref().map(|(_, id)| id.clone()),
            backdrop.is_some(),
            bytes,
            &self.staging,
        )?;
        let preparation = DrawPreparation {
            lifetime: batch.lifetime(),
            target: target.clone(),
            source: source.map(|(image, _)| image.source()),
            backdrop,
        };
        Some((batch, preparation))
    }

    pub fn draw_prepared(
        &self,
        image: &Image,
        batch: &PreparedDraws,
        prepared: DrawPreparation,
    ) -> Result<()> {
        if !Weak::ptr_eq(&prepared.lifetime, &batch.lifetime())
            || image.size != batch.size()
            || image.main_write_bytes() != 0
            || !Arc::ptr_eq(image.main()?, &prepared.target)
        {
            return Err(Error::Message(
                "prepared draw target changed before its fence",
            ));
        }
        if !batch.draws().is_empty() {
            prepared.target.changed();
        }
        let stride = PARAMETER_BYTES
            .next_multiple_of(self.device.limits().min_uniform_buffer_offset_alignment as usize);
        let mut data = vec![0u8; batch.draws().len() * stride];
        let mut draws = Vec::with_capacity(batch.draws().len());
        for (index, draw) in batch.draws().iter().enumerate() {
            let parameters = &mut data[index * stride..(index + 1) * stride];
            let encoded = match *draw {
                PreparedDraw::Color(_) => {
                    return Err(Error::Message("WGPU did not issue a color draw grant"));
                }
                PreparedDraw::Fill(fill) => {
                    let Some(rect) = image.size.rect().intersection(fill.rectangle) else {
                        draws.push(None);
                        continue;
                    };
                    let (pipeline, color) = match fill.face {
                        DrawFace::Mask => (2, [0.0, 0.0, 0.0, (fill.color & 255) as f32 / 255.0]),
                        DrawFace::Opaque if fill.hold_alpha => (1, rgba(fill.color)),
                        _ => (0, rgba(fill.color)),
                    };
                    parameters[..16].copy_from_slice(bytemuck::cast_slice(&color));
                    Encoded::Fill(rect, pipeline)
                }
                PreparedDraw::Copy {
                    rectangle,
                    x,
                    y,
                    clip,
                    face,
                    hold_alpha,
                } => {
                    let source = prepared.source.as_ref().expect("admitted copy source");
                    let Some((src, dst)) =
                        blit::region(source.size, image.size, clip, rectangle, x, y)
                    else {
                        draws.push(None);
                        continue;
                    };
                    let pipeline = match face {
                        DrawFace::Mask => 2,
                        DrawFace::Opaque if hold_alpha => 1,
                        _ => 0,
                    };
                    parameters[..8].copy_from_slice(bytemuck::cast_slice(&[
                        src.left - dst.left,
                        src.top - dst.top,
                    ]));
                    Encoded::Copy(src, dst, pipeline)
                }
                PreparedDraw::Operate {
                    rectangle,
                    x,
                    y,
                    clip,
                    options,
                } => {
                    let source = prepared.source.as_ref().expect("admitted blend source");
                    let Some((src, dst)) =
                        blit::region(source.size, image.size, clip, rectangle, x, y)
                            .filter(|_| !options.is_noop())
                    else {
                        draws.push(None);
                        continue;
                    };
                    if options.mode == Blend::Opaque
                        && options.opacity == 255
                        && options.face == DrawFace::Opaque
                    {
                        parameters[..8].copy_from_slice(bytemuck::cast_slice(&[
                            src.left - dst.left,
                            src.top - dst.top,
                        ]));
                        Encoded::Copy(src, dst, usize::from(options.hold_alpha))
                    } else {
                        let values =
                            Parameters::new((src.left - dst.left, src.top - dst.top), dst, options);
                        parameters[..PARAMETER_BYTES]
                            .copy_from_slice(bytemuck::cast_slice(&values.0));
                        Encoded::Blend(dst, values)
                    }
                }
            };
            draws.push(Some(encoded));
        }
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("prepared drawing parameters"),
                contents: &data,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::STORAGE,
            });
        let fill_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("prepared fill"),
            layout: &self.fill_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(16),
                }),
            }],
        });
        let source = prepared.source.as_ref().and_then(|s| s.main.as_deref());
        let copy_bind = source.map(|source| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("prepared copy"),
                layout: &self.copier.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &buffer,
                            offset: 0,
                            size: wgpu::BufferSize::new(48),
                        }),
                    },
                ],
            })
        });
        let mixer = prepared
            .backdrop
            .as_ref()
            .map(|_| self.mixer.get().expect("prepared mixer"));
        let blend_bind = mixer.map(|mixer| {
            mixer.bind(
                self,
                source.expect("prepared source"),
                prepared.backdrop.as_deref(),
                &buffer,
                None,
            )
        });
        // Normal fills submitted before the grant must precede this encoder.
        self.flush_fills();
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let mut index = 0;
        while index < draws.len() {
            let Some(draw) = &draws[index] else {
                index += 1;
                continue;
            };
            if let Encoded::Copy(src, dst, 0) = draw {
                copy(
                    &mut encoder,
                    source.expect("prepared source"),
                    &prepared.target,
                    *src,
                    dst.left as u32,
                    dst.top as u32,
                );
                index += 1;
                continue;
            }
            if let Encoded::Blend(rect, params) = draw
                && params.needs_destination()
            {
                copy(
                    &mut encoder,
                    &prepared.target,
                    prepared.backdrop.as_ref().expect("prepared backdrop"),
                    *rect,
                    0,
                    0,
                );
            }
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("prepared drawing"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &prepared.target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            // Fills and masked copies need no backdrop, so adjacent operations
            // can share a pass. Blends close it before the next destination copy.
            loop {
                let Some(draw) = &draws[index] else {
                    unreachable!()
                };
                let (rect, pipeline, bind) = match draw {
                    Encoded::Fill(rect, pipeline) => {
                        (rect, &self.fill_pipelines[*pipeline], &fill_bind)
                    }
                    Encoded::Copy(_, rect, pipeline) => (
                        rect,
                        &self.copier.pipelines[*pipeline],
                        copy_bind.as_ref().unwrap(),
                    ),
                    Encoded::Blend(rect, params) => (
                        rect,
                        mixer.unwrap().pipeline(params.copies_color()),
                        blend_bind.as_ref().unwrap(),
                    ),
                };
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, bind, &[(index * stride) as u32]);
                pass.set_scissor_rect(rect.left as u32, rect.top as u32, rect.width, rect.height);
                pass.draw(0..3, 0..1);
                index += 1;
                if matches!(draw, Encoded::Blend(..))
                    || !matches!(
                        draws.get(index),
                        Some(Some(Encoded::Fill(..) | Encoded::Copy(_, _, 1 | 2)))
                    )
                {
                    break;
                }
            }
        }
        self.submit_direct(encoder, (prepared, batch.permit()));
        self.check()
    }
}
