use crate::scene::raster::Raster;
use crate::{
    Error, Gpu, Image, Result,
    device::Texture,
    drawing::{face, rect, rgba},
    image::Tile,
    shader::Program,
};
use glow::HasContext;
use krkr_protocol::{
    graphics::{DrawFace, Rect, Size},
    pixels::Bytes,
    transition::{Direction, Effect, Frame, Stay, custom},
};
use std::{
    collections::HashMap,
    rc::Rc,
    sync::{Arc, Weak},
};

struct Table {
    source: Weak<Bytes>,
    texture: Rc<Texture>,
}
#[derive(Default)]
pub(crate) struct Transitions {
    normal: HashMap<u8, Program>,
    curve: Option<(u32, u32, Rc<Texture>)>,
    custom: HashMap<u8, Program>,
    tables: HashMap<usize, Table>,
}
impl Gpu {
    pub fn transition_kernels(&self) -> Vec<String> {
        vec!["krkr.extrans.v1".into(), "krkr.nagano.v1".into()]
    }
    pub fn transition(
        &self,
        target: &mut Image,
        first: &Image,
        second: &Image,
        rule: Option<&Image>,
        frame: Frame,
    ) -> Result<()> {
        self.transition_with_custom(target, first, second, rule, frame, None)
    }
    pub fn transition_with_custom(
        &self,
        target: &mut Image,
        first: &Image,
        second: &Image,
        rule: Option<&Image>,
        frame: Frame,
        custom: Option<&custom::Frame>,
    ) -> Result<()> {
        if target.size != frame.size {
            return Err(Error::Message(
                "transition images must have equal dimensions",
            ));
        }
        self.transition_raster(target, first, second, rule, frame, custom)
    }
    pub(crate) fn transition_raster(
        &self,
        target: &mut Image,
        first: &Image,
        second: &Image,
        rule: Option<&Image>,
        frame: Frame,
        custom: Option<&custom::Frame>,
    ) -> Result<()> {
        self.check_image(target)?;
        let raster = Raster::new(frame.size, target.size, (0, 0))?;
        for image in [first, second] {
            self.check_image(image)?;
            image.plane(false)?;
            if image.size != frame.size {
                return Err(Error::Message(
                    "transition images must have equal dimensions",
                ));
            }
        }
        if let Some(rule) = rule {
            self.check_image(rule)?;
        }
        if matches!(frame.effect,Effect::Universal{vague} if vague>i32::MAX as u32/255) {
            return Err(Error::Message(
                "transition vague exceeds integer kernel range",
            ));
        }
        if custom.is_none() && matches!(frame.effect, Effect::Universal { .. }) {
            let rule = rule.ok_or(Error::Message("universal transition requires a rule image"))?;
            if rule.size != frame.size {
                return Err(Error::Message("transition rule size mismatch"));
            }
            rule.plane(rule.has_province())?;
        }
        if (frame.phase == 0 && custom.is_none()) || frame.phase >= frame.effect.phases(frame.size)
        {
            let source = if frame.phase == 0 { first } else { second };
            return self.scene_bitmap(
                target,
                source,
                target.size.rect(),
                raster,
                (0, 0),
                (0, 0),
                krkr_protocol::graphics::BlendOptions::for_composition(
                    krkr_protocol::graphics::Blend::Opaque,
                    DrawFace::Alpha,
                    255,
                ),
                true,
            );
        }
        if let Some(custom) = custom {
            return self.custom_transition(target, first, second, frame, custom);
        }
        if let Effect::Scroll { from, stay } = frame.effect {
            let horizontal = matches!(from, Direction::Left | Direction::Right);
            let extent = if horizontal {
                frame.size.width
            } else {
                frame.size.height
            } as i32;
            let sign = if matches!(from, Direction::Left | Direction::Top) {
                1
            } else {
                -1
            };
            let a = if stay == Stay::Destination {
                0
            } else {
                sign * frame.phase as i32
            };
            let b = if stay == Stay::Source {
                0
            } else {
                sign * (frame.phase as i32 - extent)
            };
            let order = if stay == Stay::Source {
                [(second, b), (first, a)]
            } else {
                [(first, a), (second, b)]
            };
            for (source, offset) in order {
                let source_origin = if horizontal { (offset, 0) } else { (0, offset) };
                let Some(area) = raster.rect(Rect {
                    left: source_origin.0,
                    top: source_origin.1,
                    ..frame.size.rect()
                }) else {
                    continue;
                };
                self.scene_bitmap(
                    target,
                    source,
                    area,
                    raster,
                    (0, 0),
                    (i64::from(source_origin.0), i64::from(source_origin.1)),
                    krkr_protocol::graphics::BlendOptions::for_composition(
                        krkr_protocol::graphics::Blend::Opaque,
                        DrawFace::Alpha,
                        255,
                    ),
                    true,
                )?;
            }
            return Ok(());
        }
        let kind = match frame.effect {
            Effect::CrossFade => 0.,
            Effect::Universal { .. } => 1.,
            _ => {
                return Err(Error::Message(
                    "custom transition has no executable instance",
                ));
            }
        };
        let mut transitions = self.transitions.borrow_mut();
        if let Effect::Universal { vague } = frame.effect
            && transitions
                .curve
                .as_ref()
                .is_none_or(|(phase, old, _)| *phase != frame.phase || *old != vague)
        {
            let texture = match &transitions.curve {
                Some((_, _, texture)) => texture.clone(),
                None => self.device.sample_texture(
                    Size {
                        width: 256,
                        height: 1,
                    },
                    &self.resident,
                )?,
            };
            let _permit = self.staging.reserve(256 * 4)?;
            let mut curve = [0; 256 * 4];
            for (level, pixel) in curve.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                use krkr_render::transition::{RuleBlend, universal_opacity};
                let (opacity, select) = match universal_opacity(frame.phase, vague, level as u8) {
                    RuleBlend::First => (0, 1),
                    RuleBlend::Second => (255, 2),
                    RuleBlend::Opacity(opacity) => (opacity, 0),
                };
                pixel.copy_from_slice(&[opacity, select, 0, 0]);
            }
            self.device.upload(&texture, &curve)?;
            transitions.curve = Some((frame.phase, vague, texture));
        }
        self.writable(target, target.size.rect(), false)?;
        let a = first.plane(false)?;
        let b = second.plane(false)?;
        let rule_plane = if kind == 1. {
            let rule = rule.unwrap();
            Some(rule.plane(rule.has_province())?)
        } else {
            None
        };
        let direct = a.tiles.len() == 1
            && b.tiles.len() == 1
            && rule_plane.is_none_or(|p| p.tiles.len() == 1);
        let draw_face = face(frame.face) as u8;
        let key = draw_face * 4 + u8::from(kind == 1.) * 2 + u8::from(direct);
        if let std::collections::hash_map::Entry::Vacant(entry) = transitions.normal.entry(key) {
            // Specialize the pass before PVR compilation: crossfades need no
            // rule/curve fetch, and opaque/add-alpha faces need no alpha table.
            let fragment = crate::transition_source::fragment(kind == 1., draw_face, direct);
            entry.insert(Program::new(
                self.device.clone(),
                include_str!("quad.vert"),
                &fragment,
            )?);
        }
        let program = &transitions.normal[&key];
        let curve = transitions
            .curve
            .as_ref()
            .map_or(&self.lookup, |(_, _, texture)| texture);
        // Converted Vita scenes normally fit a texture. Sample those directly:
        // no three temporary gathers or work-surface switches for every 256px patch.
        if direct {
            let rule_texture = rule_plane.map_or(&self.lookup, |p| &p.tiles[0].texture);
            for tile in &target.plane(false)?.tiles {
                self.transition_target(program, tile, tile.rectangle, true)?;
                for (unit, name, texture) in [
                    (0, "u_source", &a.tiles[0].texture),
                    (1, "u_backdrop", &b.tiles[0].texture),
                    (2, "u_lookup", &self.lookup),
                    (3, "u_rule", rule_texture),
                    (4, "u_curve", curve),
                ] {
                    if program.uses(name) {
                        self.bind_texture(unit, texture)?;
                    }
                }
                program.one("u_direct", 1.);
                program.two(
                    "u_canvas",
                    frame.size.width as f32 / target.size.width as f32,
                    frame.size.height as f32 / target.size.height as f32,
                );
                program.two(
                    "u_extent",
                    frame.size.width as f32,
                    frame.size.height as f32,
                );
                program.two("u_source_size", a.size.width as f32, a.size.height as f32);
                program.two("u_backdrop_size", b.size.width as f32, b.size.height as f32);
                program.four("u_source_bounds", rect(a.tiles[0].rectangle));
                program.four("u_backdrop_bounds", rect(b.tiles[0].rectangle));
                program.two(
                    "u_rule_size",
                    rule_texture.size.width as f32,
                    rule_texture.size.height as f32,
                );
                program.four("u_frame", [kind, frame.phase as f32, 0., face(frame.face)]);
                program.four("u_patch", rect(tile.rectangle));
                unsafe {
                    self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                }
                self.device.check()?;
            }
            return Ok(());
        }
        // Each patch consumes the same two (or three) gathers before the next
        // patch overwrites them. Keep them alive for this transition frame.
        let mut gathers = [None, None, None];
        for tile in &target.plane(false)?.tiles {
            for part in parts(tile.rectangle, self.tile_edge.min(256)) {
                let a = self.gather_raster(first, part, raster, false, &mut gathers[0])?;
                let b = self.gather_raster(second, part, raster, false, &mut gathers[1])?;
                let rule = if kind == 1. {
                    Some(self.gather_raster(
                        rule.unwrap(),
                        part,
                        raster,
                        rule.unwrap().has_province(),
                        &mut gathers[2],
                    )?)
                } else {
                    None
                };
                self.transition_target(program, tile, part, true)?;
                self.bind_texture(0, &a.tiles[0].texture)?;
                self.bind_texture(1, &b.tiles[0].texture)?;
                if program.uses("u_lookup") {
                    self.bind_texture(2, &self.lookup)?;
                }
                if program.uses("u_rule") {
                    self.bind_texture(
                        3,
                        &rule.as_ref().expect("universal transition").tiles[0].texture,
                    )?;
                }
                if program.uses("u_curve") {
                    self.bind_texture(4, curve)?;
                }
                program.one("u_direct", 0.);
                program.four("u_frame", [kind, frame.phase as f32, 0., face(frame.face)]);
                program.four("u_patch", rect(a.tiles[0].rectangle));
                unsafe {
                    self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                }
                self.device.check()?;
            }
        }
        Ok(())
    }
    fn custom_transition(
        &self,
        target: &mut Image,
        first: &Image,
        second: &Image,
        frame: Frame,
        custom: &custom::Frame,
    ) -> Result<()> {
        if !matches!(
            custom.instance.kernel(),
            "krkr.extrans.v1" | "krkr.nagano.v1"
        ) {
            return Err(Error::Message("custom transition kernel is not registered"));
        }
        let payload = custom
            .instance
            .prepare(frame.size, custom.elapsed, custom.duration, &self.staging)
            .map_err(Error::Backend)?;
        let data = payload.table.as_slice();
        if data.is_empty() || !data.len().is_multiple_of(4) {
            return Err(Error::Message("invalid transition table size"));
        }
        let words = data.len() / 4;
        let max = self.device.max_texture as usize;
        if words > max * max {
            return Err(Error::Message(
                "transition table exceeds GLES texture limits",
            ));
        }
        let mut transitions = self.transitions.borrow_mut();
        transitions
            .tables
            .retain(|_, table| table.source.strong_count() != 0);
        let mode = if custom.instance.kernel() == "krkr.nagano.v1" {
            if payload.parameters[0] > 11 {
                return Err(Error::Message("invalid Nagano transition mode"));
            }
            16 + payload.parameters[0] as u8
        } else {
            payload.parameters[0].min(4) as u8
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = transitions.custom.entry(mode) {
            entry.insert(Program::new(
                self.device.clone(),
                include_str!("quad.vert"),
                &crate::transition_source::custom(mode),
            )?);
        }
        let key = Arc::as_ptr(&payload.table) as usize;
        if let std::collections::hash_map::Entry::Vacant(entry) = transitions.tables.entry(key) {
            let width = 256.max(words.div_ceil(max)).min(max).min(words);
            let size = Size {
                width: width as u32,
                height: words.div_ceil(width) as u32,
            };
            let texture = self.device.sample_texture(size, &self.resident)?;
            let mut padded = Bytes::zeroed(size.rgba_bytes().unwrap(), &self.staging)?;
            padded.as_mut_slice()[..data.len()].copy_from_slice(data);
            self.device.upload(&texture, padded.as_slice())?;
            entry.insert(Table {
                source: Arc::downgrade(&payload.table),
                texture,
            });
        }
        let table = &transitions.tables[&key].texture;
        let program = &transitions.custom[&mode];
        self.writable(target, target.size.rect(), false)?;
        let a = first.plane(false)?;
        let b = second.plane(false)?;
        let mut gathers = [None, None];
        for tile in &target.plane(false)?.tiles {
            for area in parts(
                tile.rectangle,
                if mode == 27 { 128 } else { self.tile_edge },
            ) {
                // Multi-tap blur must see neighbours across source tile boundaries.
                // Gather only a bounded output block plus its halo, at display density.
                let gathered = if mode == 27 && (a.tiles.len() > 1 || b.tiles.len() > 1) {
                    let rx = u64::from(payload.parameters[3].max(payload.parameters[5]));
                    let ry = u64::from(payload.parameters[4].max(payload.parameters[6]));
                    let factor = if payload.parameters[7] == 1 { 2 } else { 1 };
                    let hx = ((rx * factor + 1) * u64::from(target.size.width))
                        .div_ceil(u64::from(frame.size.width)) as i64
                        + 1;
                    let hy = ((ry * factor + 1) * u64::from(target.size.height))
                        .div_ceil(u64::from(frame.size.height)) as i64
                        + 1;
                    let left = (i64::from(area.left) - hx).max(0);
                    let top = (i64::from(area.top) - hy).max(0);
                    let right = (i64::from(area.left) + i64::from(area.width) + hx)
                        .min(i64::from(target.size.width));
                    let bottom = (i64::from(area.top) + i64::from(area.height) + hy)
                        .min(i64::from(target.size.height));
                    let region = Rect {
                        left: left as i32,
                        top: top as i32,
                        width: (right - left) as u32,
                        height: (bottom - top) as u32,
                    };
                    let raster = Raster::new(frame.size, target.size, (0, 0))?;
                    Some((
                        self.gather_raster(first, region, raster, false, &mut gathers[0])?,
                        self.gather_raster(second, region, raster, false, &mut gathers[1])?,
                    ))
                } else {
                    None
                };
                let (a, b) = gathered
                    .as_ref()
                    .map_or((a.as_ref(), b.as_ref()), |(a, b)| (a, b));
                self.transition_target(program, tile, area, false)?;
                if program.uses("u_lookup") {
                    self.bind_texture(2, &self.lookup)?;
                }
                if program.uses("u_rule") {
                    self.bind_texture(3, table)?;
                }
                program.two(
                    "u_canvas",
                    frame.size.width as f32 / target.size.width as f32,
                    frame.size.height as f32 / target.size.height as f32,
                );
                program.two(
                    "u_extent",
                    frame.size.width as f32,
                    frame.size.height as f32,
                );
                program.two(
                    "u_source_scale",
                    a.size.width as f32 / frame.size.width as f32,
                    a.size.height as f32 / frame.size.height as f32,
                );
                program.two(
                    "u_backdrop_scale",
                    b.size.width as f32 / frame.size.width as f32,
                    b.size.height as f32 / frame.size.height as f32,
                );
                program.two(
                    "u_table_size",
                    table.size.width as f32,
                    table.size.height as f32,
                );
                program.four("u_color", rgba(payload.parameters[2]));
                program.four("u_frame", [0., 0., 0., face(frame.face)]);
                for (name, values) in ["u_data0", "u_data1", "u_data2", "u_data3"]
                    .into_iter()
                    .zip(payload.parameters.as_chunks::<4>().0)
                {
                    program.four(name, values.map(|v| v as i32 as f32));
                }
                for left in &a.tiles {
                    for right in &b.tiles {
                        self.bind_texture(0, &left.texture)?;
                        self.bind_texture(1, &right.texture)?;
                        program.two(
                            "u_source_origin",
                            left.rectangle.left as f32,
                            left.rectangle.top as f32,
                        );
                        program.two(
                            "u_source_size",
                            left.rectangle.width as f32,
                            left.rectangle.height as f32,
                        );
                        program.two(
                            "u_backdrop_origin",
                            right.rectangle.left as f32,
                            right.rectangle.top as f32,
                        );
                        program.two(
                            "u_backdrop_size",
                            right.rectangle.width as f32,
                            right.rectangle.height as f32,
                        );
                        unsafe {
                            self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                        }
                    }
                }
                self.device.check()?;
            }
        }
        Ok(())
    }
    fn transition_target(
        &self,
        program: &Program,
        tile: &Tile,
        area: Rect,
        overwrite: bool,
    ) -> Result<()> {
        program.bind();
        let local = Rect {
            left: area.left - tile.rectangle.left,
            top: area.top - tile.rectangle.top,
            ..area
        };
        let framebuffer = if overwrite {
            tile.texture.overwrite_framebuffer(local)?
        } else {
            tile.texture.framebuffer_region(local)?
        };
        unsafe {
            let gl = &self.device.gl;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.viewport(
                0,
                0,
                tile.texture.size.width as i32,
                tile.texture.size.height as i32,
            );
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.color_mask(true, true, true, true);
        }
        program.four("u_target", rect(tile.rectangle));
        program.four("u_rectangle", rect(area));
        program.one("u_flip", 1.);
        Ok(())
    }
}
fn parts(area: Rect, edge: u32) -> impl Iterator<Item = Rect> {
    (0..area.height)
        .step_by(edge as usize)
        .flat_map(move |top| {
            (0..area.width)
                .step_by(edge as usize)
                .map(move |left| Rect {
                    left: area.left + left as i32,
                    top: area.top + top as i32,
                    width: (area.width - left).min(edge),
                    height: (area.height - top).min(edge),
                })
        })
}
