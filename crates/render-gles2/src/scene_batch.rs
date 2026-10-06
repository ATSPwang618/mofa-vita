//! Fuse consecutive alpha leaves with bounded sampling overhead. Each layer's
//! raster clip remains independent; group opacity and transitions stay separate.
use crate::{
    Gpu, Image, Result,
    drawing::{Draw, face, rgba},
    scene::raster::Raster,
    scene_batch_source::{self as source, Key},
    shader::Program,
};
use glow::HasContext;
use krkr_protocol::graphics::{Blend, BlendOptions, DrawFace, Rect, Size};
use std::rc::Rc;

pub(crate) struct Layer {
    pub image: Image,
    pub origin: (i64, i64),
    pub opacity: u8,
    /// Coverage in target-local raster coordinates, after ancestor clipping.
    pub coverage: Rect,
}
#[derive(Default)]
pub(crate) struct Programs(Vec<(Key, Rc<Program>)>);
struct PartitionedLayers<'a> {
    layers: &'a [Layer],
    parts: &'a [crate::scene_batch_tiles::Part],
    display: bool,
}
impl Programs {
    fn get(&mut self, gpu: &Gpu, key: Key) -> Result<Rc<Program>> {
        if let Some(i) = self.0.iter().rposition(|(old, _)| *old == key) {
            let entry = self.0.remove(i);
            let result = entry.1.clone();
            self.0.push(entry);
            return Ok(result);
        }
        if self.0.len() == 12 {
            self.0.remove(0);
        }
        let program = Rc::new(Program::new(
            gpu.device.clone(),
            include_str!("quad.vert"),
            &source::fragment(key),
        )?);
        self.0.push((key, program.clone()));
        Ok(program)
    }
}
impl Gpu {
    pub(crate) fn scene_alpha_batch(
        &self,
        target: &mut Image,
        layers: &[Layer],
        area: Rect,
        raster: Raster,
        origin: (i64, i64),
        output_face: DrawFace,
    ) -> Result<bool> {
        let display = raster.bitmap_filtered(&layers[0].image, origin, layers[0].origin)?;
        for layer in layers {
            if raster.bitmap_filtered(&layer.image, origin, layer.origin)? != display {
                return Ok(false);
            }
        }
        let Some(parts) =
            crate::scene_batch_tiles::partition(layers, area, raster, origin, display)?
        else {
            return Ok(false);
        };
        self.writable(target, area, false)?;
        self.scene_alpha_parts(
            target,
            PartitionedLayers {
                layers,
                parts: &parts,
                display,
            },
            area,
            raster,
            origin,
            output_face,
        )?;
        for part in parts {
            if part.tiles.is_none() {
                // Only ambiguous tile-boundary pixels need the original
                // per-layer sampler/discard; never guess a seam's texture.
                for layer in layers {
                    if let Some(area) = part.area.intersection(layer.coverage) {
                        self.scene_bitmap(
                            target,
                            &layer.image,
                            area,
                            raster,
                            origin,
                            layer.origin,
                            BlendOptions::for_composition(Blend::Alpha, output_face, layer.opacity),
                            false,
                        )?;
                    }
                }
            }
        }
        Ok(true)
    }
    fn scene_alpha_parts(
        &self,
        target: &Image,
        batch: PartitionedLayers<'_>,
        area: Rect,
        raster: Raster,
        origin: (i64, i64),
        output_face: DrawFace,
    ) -> Result<()> {
        let PartitionedLayers {
            layers,
            parts,
            display,
        } = batch;
        let mut sharpening = [0.; 4];
        for (i, layer) in layers.iter().enumerate() {
            let mapping = crate::scene::raster::stored_mapping(
                &layer.image,
                raster.mapping(origin, layer.origin),
            )?;
            sharpening[i] = f32::from(self.sharpen_bitmap(&layer.image, mapping));
        }
        let sharpen = display && sharpening.iter().any(|&value| value != 0.);
        let target = target.plane(false)?;
        let mut programs = [[None, None], [None, None]];
        for tile in &target.tiles {
            let Some(whole) = area.intersection(tile.rectangle) else {
                continue;
            };
            let local = Rect {
                left: whole.left - tile.rectangle.left,
                top: whole.top - tile.rectangle.top,
                ..whole
            };
            let color = tile.solid_region(local);
            // Every partition writes disjoint pixels. Freeze the original
            // destination once per target tile, then keep the surface active
            // across its parts instead of resolving/reloading each region.
            // Ambiguous seams run afterwards and only read their own pixels.
            let backing = if color.is_none() {
                self.device.backdrop(&tile.texture, local)?
            } else {
                None
            };
            let copy = if color.is_none() && backing.is_none() {
                let copy = self.device.texture(
                    Size {
                        width: whole.width,
                        height: whole.height,
                    },
                    &self.scratch,
                )?;
                self.device.copy_region(&tile.texture, local, &copy)?;
                Some(copy)
            } else {
                None
            };
            for partition in parts {
                let Some(tiles) = partition.tiles else {
                    continue;
                };
                let Some(part) = partition.area.intersection(whole) else {
                    continue;
                };
                let clipped = layers
                    .iter()
                    .any(|layer| layer.coverage.intersection(part) != Some(part));
                let slot = &mut programs[usize::from(color.is_some())][usize::from(clipped)];
                if slot.is_none() {
                    *slot = Some(self.scene_batch_programs.borrow_mut().get(
                        self,
                        Key {
                            layers: layers.len(),
                            face: face(output_face) as u8,
                            constant: color.is_some(),
                            clipped,
                            display,
                            sharpen,
                        },
                    )?);
                }
                let program = slot.as_ref().unwrap();
                self.bind_target(program, tile, part, &Draw::copy([0.; 6], [true; 4]), false)?;
                if let Some(color) = color {
                    program.four("u_backdrop_color", rgba(color));
                } else {
                    let previous = if let Some(backing) = backing {
                        unsafe {
                            self.device.gl.active_texture(glow::TEXTURE1);
                            self.device.gl.bind_texture(glow::TEXTURE_2D, Some(backing));
                        }
                        tile.rectangle
                    } else {
                        self.bind_texture(1, copy.as_deref().unwrap())?;
                        whole
                    };
                    program.two(
                        "u_backdrop_origin",
                        previous.left as f32,
                        previous.top as f32,
                    );
                    program.two(
                        "u_backdrop_size",
                        previous.width as f32,
                        previous.height as f32,
                    );
                }
                let mut opacity = [0.; 4];
                for (i, layer) in layers.iter().enumerate() {
                    let plane = layer.image.plane(false)?;
                    let tile = &plane.tiles[tiles[i]];
                    let sample = tile.sample_rectangle();
                    let map = raster.mapping(origin, layer.origin);
                    let map = if display {
                        crate::scene::raster::stored_mapping(&layer.image, map)?
                    } else {
                        map
                    };
                    let extent = if display {
                        plane.size
                    } else {
                        layer.image.size
                    };
                    self.bind_texture(source::UNITS[i], &tile.texture)?;
                    program.four(source::MAPS[i], [map[0], map[4], map[2], map[5]]);
                    program.four(
                        source::SIZES[i],
                        [
                            sample.width as f32,
                            sample.height as f32,
                            extent.width as f32,
                            extent.height as f32,
                        ],
                    );
                    program.four(
                        source::SCALES[i],
                        [
                            plane.size.width as f32 / layer.image.size.width as f32,
                            plane.size.height as f32 / layer.image.size.height as f32,
                            sample.left as f32,
                            sample.top as f32,
                        ],
                    );
                    opacity[i] = f32::from(layer.opacity);
                    if clipped {
                        let r = layer.coverage;
                        program.four(
                            source::CLIPS[i],
                            [
                                r.left as f32,
                                r.top as f32,
                                (i64::from(r.left) + i64::from(r.width)) as f32,
                                (i64::from(r.top) + i64::from(r.height)) as f32,
                            ],
                        );
                    }
                }
                program.four("u_batch_opacity", opacity);
                if sharpen {
                    program.four("u_batch_sharpen", sharpening);
                }
                unsafe {
                    self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                }
                self.device.check()?;
            }
        }
        Ok(())
    }
}
