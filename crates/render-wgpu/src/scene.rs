use crate::gpu::{Allocation, FORMAT, Gpu, Image};
use krkr_protocol::graphics::{Blend, BlendOptions, DrawFace, ImageId, Rect, Scene, Size};
use krkr_render::{Error, Result, scene::Children};
use std::{collections::HashMap, sync::Arc};
use wgpu::util::DeviceExt;

pub(super) enum Pass {
    Transition {
        target: Arc<Allocation>,
        inputs: [Arc<Allocation>; 3],
        frame: krkr_protocol::transition::Frame,
        custom: Option<krkr_protocol::transition::custom::Frame>,
    },
    Clear(Arc<Allocation>, [f32; 4]),
    Draw {
        target: Arc<Allocation>,
        source: Arc<Allocation>,
        clip: Rect,
        parameters: crate::blend::Parameters,
    },
}
struct Frame<'a> {
    gpu: &'a Gpu,
    scene: &'a Scene,
    images: &'a HashMap<ImageId, Image>,
    children: Children,
    passes: Vec<Pass>,
    transitions: Vec<Option<usize>>,
    active: Vec<usize>,
    reusable_groups: Vec<Arc<Allocation>>,
    group_size: Option<Size>,
    cache_entries: Vec<crate::scene_cache::Entry>,
    cache_allowance: usize,
}
impl Frame<'_> {
    fn allocation_error(&self, error: Error, purpose: &str, size: Size) -> Error {
        let mut surfaces = HashMap::new();
        for pass in &self.passes {
            if let Pass::Clear(image, _) | Pass::Transition { target: image, .. } = pass {
                surfaces.insert(
                    Arc::as_ptr(image),
                    (image.texture.width(), image.texture.height()),
                );
            }
        }
        let mut sizes: Vec<_> = surfaces.into_values().collect();
        sizes.sort_unstable();
        Error::Backend(format!(
            "{error}; {purpose} requests {}x{} ({} bytes), frame surfaces {sizes:?}, band={:?}",
            size.width,
            size.height,
            size.rgba_bytes().unwrap_or(usize::MAX),
            self.group_size,
        ))
    }

    fn group(&mut self, size: Size) -> Result<Arc<Allocation>> {
        // Under pressure, uniform slots avoid retaining a different allocation
        // for every sibling's dimensions. All reads precede reuse in the encoder.
        let size = self.group_size.unwrap_or(size);
        let available = self
            .reusable_groups
            .iter()
            .enumerate()
            .filter(|(_, image)| {
                image.texture.width() >= size.width && image.texture.height() >= size.height
            })
            .min_by_key(|(_, image)| {
                u64::from(image.texture.width()) * u64::from(image.texture.height())
            })
            .map(|(index, _)| index);
        if let Some(index) = available {
            Ok(self.reusable_groups.swap_remove(index))
        } else {
            self.gpu
                .temporary(size, FORMAT)
                .map_err(|error| self.allocation_error(error, "group", size))
        }
    }

    fn node(
        &mut self,
        index: usize,
        target: &Arc<Allocation>,
        origin: (i64, i64),
        parent: (i64, i64),
        clip: Rect,
        face: DrawFace,
    ) -> Result<()> {
        let node = &self.scene.nodes[index];
        if !node.visible || node.opacity == 0 {
            return Ok(());
        }
        let left = parent.0 + i64::from(node.rectangle.left);
        let top = parent.1 + i64::from(node.rectangle.top);
        // Coordinates remain wide until the visible intersection is known.
        let x = left.max(i64::from(clip.left));
        let y = top.max(i64::from(clip.top));
        let right = (left + i64::from(node.rectangle.width))
            .min(i64::from(clip.left) + i64::from(clip.width));
        let bottom = (top + i64::from(node.rectangle.height))
            .min(i64::from(clip.top) + i64::from(clip.height));
        if x >= right || y >= bottom {
            return Ok(());
        }
        let clip = Rect {
            left: x as i32,
            top: y as i32,
            width: (right - x) as u32,
            height: (bottom - y) as u32,
        };
        let mut cached_group = None;
        if self.scene.transitions.is_empty()
            && self.group_size.is_none()
            && !self.children[index].is_empty()
            && let Some(owner) = &node.cache
            && let Some(signature) = crate::scene_cache::Signature::capture(
                self.scene,
                self.images,
                &self.children,
                index,
                Rect {
                    left: (x - left) as i32,
                    top: (y - top) as i32,
                    ..clip
                },
            )
        {
            let image = self.gpu.scene_cache.lock().unwrap().get(owner, &signature);
            if let Some((image, region)) = image {
                self.draw_group(
                    index,
                    target,
                    (image, (region.left, region.top)),
                    clip,
                    origin,
                    face,
                );
                return Ok(());
            }
            let size = Size {
                width: clip.width,
                height: clip.height,
            };
            let metadata = signature.bytes();
            if size.rgba_bytes().is_some_and(|bytes| {
                bytes.saturating_add(metadata)
                    <= self.gpu.resident.available().min(self.cache_allowance)
            }) {
                let permit = self.gpu.resident.reserve(metadata)?;
                let image = self.gpu.allocation(size, FORMAT, &self.gpu.resident)?;
                self.cache_allowance -= size.rgba_bytes().expect("admitted cache") + metadata;
                cached_group = Some(crate::scene_cache::Entry {
                    owner: Arc::downgrade(owner),
                    signature,
                    image,
                    _metadata: permit,
                });
            }
        }
        let transition = self.transitions[index].filter(|_| !self.active.contains(&index));
        let with_children = transition.is_some_and(|i| self.scene.transitions[i].with_children);
        let transitioned = transition.map(|i| self.transition_bitmap(i)).transpose()?;
        let grouped = cached_group.is_some()
            || (!with_children
                && !self.children[index].is_empty()
                && (node.opacity != 255 || node.blend != Blend::Opaque));
        let group = if let Some(entry) = &cached_group {
            Some(entry.image.clone())
        } else if grouped {
            Some(self.group(Size {
                width: clip.width,
                height: clip.height,
            })?)
        } else {
            None
        };
        let (output, output_origin) = if let Some(group) = &group {
            self.passes.push(Pass::Clear(
                group.clone(),
                crate::gpu::rgba(node.blend.neutral()),
            ));
            (group, (x, y))
        } else {
            (target, origin)
        };
        let solid = transitioned.is_none() && node.image.is_none() && node.blend == Blend::Opaque;
        let mut bitmap_origin = (0i32, 0i32);
        let mut bitmap_size = None;
        let bitmap = if solid {
            Some(self.gpu.mixer()?.solid_source())
        } else if let Some(image) = &transitioned {
            Some(image.clone())
        } else {
            node.image
                .as_ref()
                .map(|image| -> Result<Option<Arc<Allocation>>> {
                    let image = self
                        .images
                        .get(&image.id)
                        .ok_or(Error::Message("scene image is no longer available"))?;
                    let sx = x - left - i64::from(node.image_left);
                    let sy = y - top - i64::from(node.image_top);
                    let l = sx.max(0);
                    let t = sy.max(0);
                    let r = (sx + i64::from(clip.width)).min(i64::from(image.size.width));
                    let b = (sy + i64::from(clip.height)).min(i64::from(image.size.height));
                    if l >= r || t >= b {
                        return Ok(None);
                    }
                    let region = self.gpu.resolve_main(
                        image,
                        Rect {
                            left: l as i32,
                            top: t as i32,
                            width: (r - l) as u32,
                            height: (b - t) as u32,
                        },
                    )?;
                    bitmap_origin = (region.rectangle.left, region.rectangle.top);
                    bitmap_size = Some(Size {
                        width: region.rectangle.width,
                        height: region.rectangle.height,
                    });
                    Ok(Some(region.allocation))
                })
                .transpose()?
                .flatten()
        };
        if let Some(image) = bitmap {
            let offset_x = if solid {
                output_origin.0 - x
            } else {
                output_origin.0
                    - left
                    - if with_children {
                        0
                    } else {
                        i64::from(node.image_left)
                    }
            };
            let offset_y = if solid {
                output_origin.1 - y
            } else {
                output_origin.1
                    - top
                    - if with_children {
                        0
                    } else {
                        i64::from(node.image_top)
                    }
            };
            let mut parameters = crate::blend::Parameters::new(
                (
                    (offset_x - i64::from(bitmap_origin.0)) as i32,
                    (offset_y - i64::from(bitmap_origin.1)) as i32,
                ),
                local(clip, output_origin),
                BlendOptions::for_composition(node.blend, face, node.opacity),
            );
            if !solid {
                let size = if let Some(transition) = transition {
                    self.scene.transitions[transition].frame.size
                } else {
                    self.images[&node.image.as_ref().expect("bitmap").id].size
                };
                let size = bitmap_size.unwrap_or(size);
                parameters.0[26] = size.width as i32;
                parameters.0[27] = size.height as i32;
            }
            // A group's own bitmap is its starting content. Blend the complete
            // subtree against its parent only after its children are drawn.
            if grouped {
                parameters.0[4] = -1;
            }
            if solid {
                parameters.0[7] |= 512;
                let rgb = node.neutral_color;
                parameters.0[8..12].copy_from_slice(&[
                    ((rgb >> 16) & 255) as i32,
                    ((rgb >> 8) & 255) as i32,
                    (rgb & 255) as i32,
                    255,
                ]);
            }
            self.passes.push(Pass::Draw {
                target: output.clone(),
                source: image,
                clip: local(clip, output_origin),
                parameters,
            });
        }
        for n in 0..if with_children {
            0
        } else {
            self.children[index].len()
        } {
            self.node(
                self.children[index][n],
                output,
                output_origin,
                (left, top),
                clip,
                node.blend.face(),
            )?;
        }
        if let Some(group) = group {
            self.draw_group(index, target, (group.clone(), (0, 0)), clip, origin, face);
            // Its final read precedes later passes in the same encoder. Reuse
            // the allocation for subsequent siblings while preserving nesting;
            // keeping every finished sibling alive needlessly exhausts scratch.
            if let Some(entry) = cached_group {
                self.cache_entries.push(entry);
            } else {
                self.reusable_groups.push(group);
            }
        }
        Ok(())
    }
    fn draw_group(
        &mut self,
        index: usize,
        target: &Arc<Allocation>,
        source: (Arc<Allocation>, (i32, i32)),
        clip: Rect,
        origin: (i64, i64),
        face: DrawFace,
    ) {
        let (image, source_origin) = source;
        let node = &self.scene.nodes[index];
        self.passes.push(Pass::Draw {
            target: target.clone(),
            source: image,
            clip: local(clip, origin),
            parameters: crate::blend::Parameters::new(
                (
                    (origin.0 - i64::from(clip.left) + i64::from(source_origin.0)) as i32,
                    (origin.1 - i64::from(clip.top) + i64::from(source_origin.1)) as i32,
                ),
                local(clip, origin),
                BlendOptions::for_composition(node.blend, face, node.opacity),
            ),
        });
    }
    // Root opacity, visibility and position belong to presentation. Transition
    // inputs are the layer's own pixels, optionally composed with its children.
    fn content(
        &mut self,
        index: usize,
        with_children: bool,
        size: Size,
    ) -> Result<Arc<Allocation>> {
        let node = &self.scene.nodes[index];
        // Complete() includes a source's own transition; a transition without
        // children reads raw MainImage instead. The destination is already on
        // the active stack, so its starting content cannot recursively use itself.
        let nested = with_children
            .then_some(self.transitions[index])
            .flatten()
            .filter(|_| !self.active.contains(&index));
        let nested_children = nested.is_some_and(|i| self.scene.transitions[i].with_children);
        let bitmap = if let Some(nested) = nested {
            Some(self.transition_bitmap(nested)?)
        } else {
            node.image
                .as_ref()
                .map(|image| {
                    let image = self
                        .images
                        .get(&image.id)
                        .ok_or(Error::Message("transition image is no longer available"))?;
                    self.gpu
                        .resolve_main(image, image.size.rect())
                        .map(|r| r.allocation)
                })
                .transpose()?
        };
        let source_size = if let Some(nested) = nested {
            self.scene.transitions[nested].frame.size
        } else {
            node.image
                .as_ref()
                .map_or(size, |image| self.images[&image.id].size)
        };
        let offset = if with_children && !nested_children {
            (node.image_left, node.image_top)
        } else {
            (0, 0)
        };
        let children = with_children && !nested_children;
        if (!children || self.children[index].is_empty())
            && offset == (0, 0)
            && let Some(image) = &bitmap
            && source_size == size
        {
            return Ok(image.clone());
        }
        let output = self.gpu.temporary(size, FORMAT)?;
        self.passes.push(Pass::Clear(
            output.clone(),
            crate::gpu::rgba(if bitmap.is_none() && node.blend == Blend::Opaque {
                node.neutral_color | 0xff000000
            } else {
                node.blend.neutral()
            }),
        ));
        if let Some(image) = bitmap
            && let Some((source, clip)) = krkr_render::blit::region(
                source_size,
                size,
                size.rect(),
                source_size.rect(),
                offset.0,
                offset.1,
            )
        {
            let mut parameters = crate::blend::Parameters::new(
                (source.left - clip.left, source.top - clip.top),
                clip,
                BlendOptions {
                    mode: node.blend,
                    face: node.blend.face(),
                    opacity: 255,
                    hold_alpha: false,
                },
            );
            parameters.0[4] = -1;
            parameters.0[26] = source_size.width as i32;
            parameters.0[27] = source_size.height as i32;
            self.passes.push(Pass::Draw {
                target: output.clone(),
                source: image,
                clip,
                parameters,
            });
        }
        if children {
            for child in 0..self.children[index].len() {
                self.node(
                    self.children[index][child],
                    &output,
                    (0, 0),
                    (0, 0),
                    size.rect(),
                    node.blend.face(),
                )?;
            }
        }
        Ok(output)
    }
    fn transition_bitmap(&mut self, index: usize) -> Result<Arc<Allocation>> {
        let transition = &self.scene.transitions[index];
        if self.active.len() >= 128 {
            return Err(Error::Message(
                "transition composition nesting exceeds renderer limit",
            ));
        }
        self.active.push(transition.destination);
        let size = transition.frame.size;
        let phase = transition.frame.phase;
        if (phase == 0 && transition.custom.is_none())
            || phase >= transition.frame.effect.phases(size)
        {
            let node = if phase == 0 {
                transition.destination
            } else {
                transition.source
            };
            let image = self.content(node, transition.with_children, size)?;
            self.active.pop();
            return Ok(image);
        }
        let first = self.content(transition.destination, transition.with_children, size)?;
        let second = self.content(transition.source, transition.with_children, size)?;
        let rule = if let Some(rule) = &transition.rule {
            let image = self
                .images
                .get(&rule.id)
                .ok_or(Error::Message("transition rule is no longer available"))?;
            if image.size != size {
                return Err(Error::Message("transition rule size mismatch"));
            }
            if let Some(province) = &image.province {
                province.clone()
            } else {
                self.gpu.resolve_main(image, image.size.rect())?.allocation
            }
        } else if matches!(
            transition.frame.effect,
            krkr_protocol::transition::Effect::Universal { .. }
        ) {
            return Err(Error::Message("universal transition requires a rule image"));
        } else {
            first.clone()
        };
        let output = self.gpu.temporary(size, FORMAT)?;
        self.passes.push(Pass::Transition {
            target: output.clone(),
            inputs: [first, second, rule],
            frame: transition.frame,
            custom: transition.custom.clone(),
        });
        self.active.pop();
        Ok(output)
    }
}
fn local(rect: Rect, origin: (i64, i64)) -> Rect {
    Rect {
        left: (i64::from(rect.left) - origin.0) as i32,
        top: (i64::from(rect.top) - origin.1) as i32,
        ..rect
    }
}
impl Gpu {
    pub fn create_surface_image(&self, size: Size) -> Result<Image> {
        Ok(Image::new(Some(self.temporary(size, FORMAT)?), None, size))
    }
    pub fn compose(
        &self,
        target: &mut Image,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
    ) -> Result<()> {
        self.compose_region(target, scene, images, (0, 0))
    }
    /// Compose only the target-sized region beginning at a scene coordinate.
    /// Preserve scene coordinates, including spatial transition inputs.
    pub fn compose_region(
        &self,
        target: &mut Image,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
        origin: (i32, i32),
    ) -> Result<()> {
        self.scene_cache.lock().unwrap().trim();
        self.materialize(target)?;
        self.independ_image(target, false, false)?;
        let children = Children::new(&scene.nodes, 128)?;
        let mut transitions = vec![None; scene.nodes.len()];
        for (index, transition) in scene.transitions.iter().enumerate() {
            if transition.destination >= scene.nodes.len() || transition.source >= scene.nodes.len()
            {
                return Err(Error::Message("transition references a missing scene node"));
            }
            if matches!(transition.frame.effect,krkr_protocol::transition::Effect::Universal{vague} if vague > i32::MAX as u32/255)
            {
                return Err(Error::Message(
                    "transition vague exceeds integer kernel range",
                ));
            }
            if transitions[transition.destination].replace(index).is_some() {
                return Err(Error::Message("scene node has multiple transitions"));
            }
        }
        let mut frame = Frame {
            gpu: self,
            scene,
            images,
            children,
            transitions,
            active: Vec::new(),
            reusable_groups: Vec::new(),
            group_size: None,
            cache_entries: Vec::new(),
            cache_allowance: 0,
            passes: vec![Pass::Clear(target.main()?.clone(), [0.0; 4])],
        };
        let mixer = self.mixer()?;
        let mut band_height = target.size.height;
        // Ordinary composition is pointwise, including legacy integer blends.
        // Complete each horizontal band in tree order before reusing its group
        // surfaces. Spatial transitions still require their complete inputs.
        if scene.transitions.is_empty() {
            let mut group_depths = vec![None; scene.nodes.len()];
            let mut groups = 0;
            for (index, node) in scene.nodes.iter().enumerate() {
                if node.visible && node.opacity != 0 {
                    group_depths[index] = node
                        .parent
                        .map_or(Some(0usize), |parent| group_depths[parent])
                        .map(|depth| {
                            depth
                                + usize::from(
                                    !frame.children[index].is_empty()
                                        && (node.opacity != 255 || node.blend != Blend::Opaque),
                                )
                        });
                    groups = groups.max(group_depths[index].unwrap_or(0));
                }
            }
            // One surface for each active ancestor group, plus one backdrop.
            let row_bytes = target.size.width as usize * 4 * (groups + 1);
            let full_bytes = row_bytes.saturating_mul(target.size.height as usize);
            // Reclaim optional retained rasters before deciding that a frame
            // cannot fit. Allocation's pressure handler runs too late for the
            // one-row / band-count admission checks below.
            while full_bytes
                > self
                    .scratch
                    .available()
                    .saturating_add(self.reusable_scratch_bytes())
                && self.scene_cache.lock().unwrap().evict_oldest()
            {}
            if full_bytes
                > self
                    .scratch
                    .available()
                    .saturating_add(self.reusable_scratch_bytes())
            {
                self.trim_resident_pool();
            }
            // Recompute after eviction; caches and scratch may share a parent.
            frame.cache_allowance = self.resident.available().saturating_sub(full_bytes);
            let capacity = self
                .scratch
                .available()
                .saturating_add(self.reusable_scratch_bytes());
            if full_bytes > capacity {
                let rows = capacity / row_bytes;
                if rows == 0 {
                    return Err(Error::Backend(format!(
                        "composition cannot fit one band row: requested={row_bytes}, available={}",
                        self.scratch.available(),
                    )));
                }
                band_height = rows.min(target.size.height as usize) as u32;
                // Stable size classes keep small occupancy changes from
                // reallocating the entire band pool on consecutive frames.
                if band_height >= 64 {
                    band_height = band_height / 64 * 64;
                }
                if target.size.height.div_ceil(band_height) > 64 {
                    return Err(Error::Message(
                        "composition band count exceeds the renderer limit",
                    ));
                }
                frame.group_size = Some(Size {
                    width: target.size.width,
                    height: band_height,
                });
                self.trim_scratch_except(frame.group_size);
            }
        }
        for top in (0..target.size.height).step_by(band_height as usize) {
            let clip = Rect {
                left: origin.0,
                top: (i64::from(origin.1) + i64::from(top)) as i32,
                height: band_height.min(target.size.height - top),
                ..target.size.rect()
            };
            for (index, node) in scene.nodes.iter().enumerate() {
                if node.parent.is_none() {
                    frame.node(
                        index,
                        target.main()?,
                        (i64::from(origin.0), i64::from(origin.1)),
                        (0, 0),
                        clip,
                        DrawFace::AddAlpha,
                    )?;
                }
            }
        }
        // Copies and draws are ordered in this encoder, so every draw can
        // reuse the same backdrop allocation after the preceding pass ends.
        let mut backdrop_size = Size {
            width: 0,
            height: 0,
        };
        let batches = crate::scene_batch::plan(&frame.passes, band_height);
        for batch in &batches {
            if let Some(clip) = batch.backdrop {
                backdrop_size.width = backdrop_size.width.max(clip.width);
                backdrop_size.height = backdrop_size.height.max(clip.height);
            }
        }
        let backdrop = if backdrop_size.width != 0 {
            Some(
                self.temporary(backdrop_size, FORMAT)
                    .map_err(|error| frame.allocation_error(error, "backdrop", backdrop_size))?,
            )
        } else {
            None
        };
        let stride = self.device.limits().min_uniform_buffer_offset_alignment as usize;
        let bytes = frame
            .passes
            .len()
            .checked_mul(stride)
            .filter(|&n| n <= u32::MAX as usize)
            .ok_or(Error::Message("composition batch is too large"))?;
        let permit = self.staging.reserve(
            bytes
                .checked_mul(2)
                .ok_or(Error::Message("composition byte size overflow"))?,
        )?;
        let mut data = vec![0u8; bytes];
        for batch in &batches {
            for index in batch.range.clone() {
                if let Pass::Draw { parameters, .. } = &frame.passes[index] {
                    let mut parameters = *parameters;
                    if let Some(backdrop) = batch.backdrop {
                        parameters.0[2] = backdrop.left;
                        parameters.0[3] = backdrop.top;
                    }
                    data[index * stride..index * stride + crate::blend::PARAMETER_BYTES]
                        .copy_from_slice(bytemuck::cast_slice(&parameters.0));
                }
            }
        }
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("composition parameters"),
                contents: &data,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::STORAGE,
            });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let mut transition_parameters = Vec::new();
        for batch in &batches {
            match &frame.passes[batch.range.start] {
                Pass::Transition {
                    target,
                    inputs,
                    frame,
                    custom,
                } => {
                    transition_parameters.push(self.transitions().draw(
                        self,
                        &mut encoder,
                        target,
                        [&inputs[0], &inputs[1], &inputs[2]],
                        *frame,
                        custom.as_ref(),
                    )?);
                }
                Pass::Clear(target, color) => self.clear(&mut encoder, target, *color),
                Pass::Draw {
                    target,
                    source,
                    clip,
                    parameters,
                } => {
                    let destination = if let Some(clip) = batch.backdrop {
                        let backdrop = backdrop.as_ref().expect("frame backdrop");
                        crate::copy::copy(&mut encoder, target, backdrop, clip, 0, 0);
                        Some(backdrop.as_ref())
                    } else {
                        None
                    };
                    if batch.range.len() == 1 {
                        mixer.draw(
                            self,
                            &mut encoder,
                            target,
                            source,
                            destination,
                            &buffer,
                            (batch.range.start * stride) as u32,
                            *clip,
                            parameters.copies_color(),
                            None,
                        );
                        continue;
                    }
                    // These clips cannot overlap, so every draw observes the
                    // same pre-batch destination without changing blend order.
                    let binds: Vec<_> = batch
                        .range
                        .clone()
                        .map(|index| {
                            let Pass::Draw { source, .. } = &frame.passes[index] else {
                                unreachable!()
                            };
                            mixer.bind(self, source, destination, &buffer, None)
                        })
                        .collect();
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("scene draw batch"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &target.view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    });
                    for (index, bind) in batch.range.clone().zip(&binds) {
                        let Pass::Draw {
                            clip, parameters, ..
                        } = &frame.passes[index]
                        else {
                            unreachable!()
                        };
                        pass.set_pipeline(mixer.pipeline(parameters.copies_color()));
                        pass.set_bind_group(0, bind, &[(index * stride) as u32]);
                        pass.set_scissor_rect(
                            clip.left as u32,
                            clip.top as u32,
                            clip.width,
                            clip.height,
                        );
                        pass.draw(0..3, 0..1);
                    }
                }
            }
        }
        // All reads of these scratch surfaces are already encoded on the same
        // ordered queue. They may be reused by the next frame; GPU completion
        // must retain byte charges, not make the pool mistake them for active
        // CPU composition dependencies. wgpu retains submitted GPU resources.
        let mut pins = Vec::new();
        for pass in &frame.passes {
            match pass {
                Pass::Clear(target, _) => pins.push(target.permit.clone()),
                Pass::Draw { target, source, .. } => {
                    pins.push(target.permit.clone());
                    pins.push(source.permit.clone());
                }
                Pass::Transition { target, inputs, .. } => {
                    pins.push(target.permit.clone());
                    pins.extend(inputs.iter().map(|image| image.permit.clone()));
                }
            }
        }
        if let Some(backdrop) = &backdrop {
            pins.push(backdrop.permit.clone());
        }
        self.submit(encoder, (pins, permit, transition_parameters));
        self.check()?;
        let mut cache = self.scene_cache.lock().unwrap();
        for entry in frame.cache_entries {
            cache.insert(entry);
        }
        Ok(())
    }
    pub fn present_to(
        &self,
        image: &Image,
        blitter: &wgpu::util::TextureBlitter,
        target: &wgpu::TextureView,
    ) -> Result<()> {
        self.present_to_with(image, blitter, target, |_| {})
    }
    /// Encode host overlays in the same submission as presentation. The host
    /// draws on the swapchain view, leaving the retained game canvas intact.
    pub fn present_to_with(
        &self,
        image: &Image,
        blitter: &wgpu::util::TextureBlitter,
        target: &wgpu::TextureView,
        finish: impl FnOnce(&mut wgpu::CommandEncoder),
    ) -> Result<()> {
        let region = self.resolve_main(image, image.size.rect())?;
        let source = &region.allocation;
        // TextureBlitter maps the complete texture. Keep that API correct for
        // padded images too; desktop presentation targets are already exact.
        let cropped = (source.texture.width() != image.size.width
            || source.texture.height() != image.size.height)
            .then(|| self.temporary(image.size, FORMAT))
            .transpose()?;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        if let Some(cropped) = &cropped {
            crate::copy::copy(&mut encoder, source, cropped, image.size.rect(), 0, 0);
        }
        blitter.copy(
            &self.device,
            &mut encoder,
            &cropped.as_ref().unwrap_or(source).view,
            target,
        );
        finish(&mut encoder);
        self.submit(encoder, (source.clone(), cropped));
        self.check()
    }
}
