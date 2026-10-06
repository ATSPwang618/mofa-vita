//! Resolve animation nodes into portable drawing packets. Priority order is
//! independent of the transform hierarchy and is flattened before depth sorting.
use super::{
    mesh::{self, Matrix, Surface},
    model::Motion,
    playback::{Playback, Selection},
    render::Paint,
    render::{Frame, Packet, Textures},
    sample::Bounds,
    state::State,
    transform::Transform,
};
use krkr_engine::protocol::budget::Budget;
use std::collections::BTreeMap;
use tjs_core::{NativeError, NativeResult};

type Key = (usize, usize);
struct Builder<'a> {
    playback: &'a Playback,
    textures: &'a mut Textures,
    budget: &'a Budget,
    cancelled: &'a dyn Fn() -> bool,
    frame: Frame,
    next_instance: usize,
    nodes: BTreeMap<Key, usize>,
    masks: Vec<(usize, Vec<Key>)>,
    viewport: [f32; 2],
    visited: usize,
    geometry_only: bool,
}
struct Context<'a> {
    state: &'a State,
    motion: &'a Motion,
    character: &'a str,
    name: &'a str,
    instance: usize,
}
struct Parent<'a> {
    chain: &'a [Surface],
    bounds: Bounds,
    masks: &'a [Key],
}

pub(super) fn build(
    playback: &Playback,
    transform: &Transform,
    textures: &mut Textures,
    budget: &Budget,
    cancelled: &dyn Fn() -> bool,
) -> NativeResult<Frame> {
    build_mode(playback, transform, textures, budget, cancelled, false)
}
pub(super) fn geometry(
    playback: &Playback,
    transform: &Transform,
    cancelled: &dyn Fn() -> bool,
) -> NativeResult<Frame> {
    build_mode(
        playback,
        transform,
        &mut Textures::default(),
        &Budget::new(0),
        cancelled,
        true,
    )
}
fn build_mode(
    playback: &Playback,
    transform: &Transform,
    textures: &mut Textures,
    budget: &Budget,
    cancelled: &dyn Fn() -> bool,
    geometry_only: bool,
) -> NativeResult<Frame> {
    let (Some(selection), Some(root)) = (&playback.selected, &transform.root) else {
        return Ok(Frame::default());
    };
    textures.begin_frame();
    let mut builder = Builder {
        playback,
        textures,
        budget,
        cancelled,
        frame: Frame::default(),
        next_instance: 0,
        nodes: BTreeMap::new(),
        masks: Vec::new(),
        viewport: transform.viewport.unwrap_or(transform.size),
        visited: 0,
        geometry_only,
    };
    let order = builder.motion(
        selection,
        playback.sample_tick,
        std::slice::from_ref(root),
        Bounds {
            size: transform.size.map(f64::from),
            origin: transform.origin.map(f64::from),
        },
        &[],
        0,
    )?;
    builder.frame.order = order;
    for (packet, keys) in builder.masks {
        builder.frame.packets[packet].masks = keys
            .into_iter()
            .filter_map(|key| builder.nodes.get(&key).copied())
            .collect();
    }
    builder.textures.end_frame();
    Ok(builder.frame)
}

impl Builder<'_> {
    fn motion_source(&self, source: &str) -> Option<Selection> {
        let mut parts = source.strip_prefix("motion/")?.split('/');
        let character = parts.next()?;
        let motion = parts.next()?;
        let main = self
            .playback
            .main
            .as_ref()
            .and_then(|name| self.playback.files.get_key_value(name));
        main.into_iter()
            .chain(self.playback.files.iter())
            .find_map(|(storage, state)| {
                state.file.objects.get(character)?.motions.get(motion)?;
                Some(Selection {
                    storage: storage.clone(),
                    character: character.into(),
                    motion: motion.into(),
                })
            })
    }
    fn motion(
        &mut self,
        selection: &Selection,
        tick: f32,
        chain: &[Surface],
        bounds: Bounds,
        masks: &[Key],
        depth: usize,
    ) -> NativeResult<Vec<usize>> {
        if depth > 128 {
            return Err(NativeError::Message("E-mote motion nesting exceeds limit"));
        }
        let Some(state) = self.playback.files.get(&selection.storage) else {
            return Ok(Vec::new());
        };
        let Some(motion) = state
            .file
            .objects
            .get(&selection.character)
            .and_then(|o| o.motions.get(&selection.motion))
        else {
            return Ok(Vec::new());
        };
        let instance = self.next_instance;
        self.next_instance += 1;
        self.frame.motions.push(Default::default());
        self.frame.motions[instance].icons.start = self.frame.icon_bounds.len();
        let cx = Context {
            state,
            motion,
            character: &selection.character,
            name: &selection.motion,
            instance,
        };
        let mut outputs = BTreeMap::new();
        let tick = if motion.loop_time >= 0. {
            let divisor = if motion.loop_time == 0. {
                motion.last_time
            } else {
                motion.loop_time
            } as f32;
            tick % divisor
        } else {
            tick
        };
        for &index in &motion.roots {
            if motion.order.contains(&index) {
                let masks = if motion.layers[index].kind == 2 {
                    &[]
                } else {
                    masks
                };
                self.node(
                    &cx,
                    index,
                    tick,
                    Parent {
                        chain,
                        bounds,
                        masks,
                    },
                    &mut outputs,
                    depth + 1,
                )?;
            }
        }
        let mut order = Vec::new();
        for index in motion.order.iter().rev() {
            if let Some(output) = outputs.get(index) {
                order.extend_from_slice(output);
            }
        }
        self.frame.motions[instance].icons.end = self.frame.icon_bounds.len();
        self.frame.motions[instance]
            .children
            .sort_by_key(|(label, _)| {
                motion
                    .order
                    .iter()
                    .position(|&i| motion.layers[i].label == *label)
                    .unwrap_or(usize::MAX)
            });
        Ok(order)
    }
    fn node(
        &mut self,
        cx: &Context<'_>,
        index: usize,
        tick: f32,
        parent: Parent<'_>,
        outputs: &mut BTreeMap<usize, Vec<usize>>,
        depth: usize,
    ) -> NativeResult<()> {
        let Parent {
            chain: parent,
            mut bounds,
            masks: inherited_masks,
        } = parent;
        self.visited += 1;
        if depth > 128 || self.visited > 1_000_000 {
            return Err(NativeError::Message("E-mote scene exceeds traversal limit"));
        }
        if (self.cancelled)() {
            return Err(NativeError::Message("E-mote scene evaluation cancelled"));
        }
        let layer = &cx.motion.layers[index];
        let parameter = layer
            .parameter
            .and_then(|p| cx.state.parameter_tick(cx.character, cx.name, cx.motion, p));
        let sample = layer.sample(tick, cx.motion, cx.state.file.motion, parameter, bounds);
        let mut chain = parent.to_vec();
        let mut masks = inherited_masks.to_vec();
        let mut nested = None;
        let mut offset = 0.;
        if let Some(mut sample) = sample {
            let content = layer.frames[sample.frame]
                .content
                .as_ref()
                .expect("sample has content");
            let source = content
                .source
                .strip_prefix("src/")
                .unwrap_or(&content.source);
            let icon = content.source.strip_prefix("src/").and_then(|s| {
                let mut parts = s.split('/');
                let source = parts.next()?;
                let name = parts.next()?;
                cx.state.file.sources.get(source)?.icons.get(name)
            });
            if icon.is_none() {
                nested = self.motion_source(&content.source);
            }
            let shape = source.starts_with("shape/") || (source == "layout" && layer.kind == 1);
            let layout =
                nested.is_some() || matches!(source, "layout" | "clip") || layer.kind == 12;
            if let Some(icon) = icon {
                bounds = Bounds {
                    size: icon.size,
                    origin: icon.origin,
                };
            } else if shape {
                bounds.size = content.zoom.map(|v| v * 16.);
                bounds.origin = bounds.size.map(|v| v / 2.);
                sample.zoom = [1.; 2];
            } else if let Some(dimensions) = source.strip_prefix("blank/") {
                let values: Vec<_> = dimensions
                    .split(':')
                    .map(str::parse::<i32>)
                    .collect::<Result<_, _>>()
                    .map_err(|_| NativeError::Message("invalid E-mote blank dimensions"))?;
                if values.len() != 4 {
                    return Err(NativeError::Message("invalid E-mote blank dimensions"));
                }
                bounds = Bounds {
                    size: [values[0] as f64, values[1] as f64],
                    origin: [values[2] as f64, values[3] as f64],
                };
            }
            if layer.kind != 7 {
                if let Some(root) = chain.first_mut() {
                    root.attach =
                        root.attach
                            .multiply(Matrix::translation(0., 0., sample.coord[2] as f32));
                }
                if layer.kind == 12 && !layer.stencil_layers.is_empty() {
                    masks.extend(layer.stencil_layers.iter().filter_map(|label| {
                        cx.motion
                            .order
                            .iter()
                            .find(|&&i| cx.motion.layers[i].label == *label)
                            .map(|&i| (cx.instance, i))
                    }));
                }
                let kind = if sample.mesh.is_some() {
                    1
                } else if layout && !shape {
                    3
                } else {
                    2
                };
                offset = sample.time_offset as f32;
                chain.push(Surface {
                    kind,
                    sample: sample.clone(),
                    inherit: layer.inherit_mask,
                    attach: Matrix::default(),
                    size: bounds.size.map(|v| v as f32),
                    origin: bounds.origin.map(|v| v as f32),
                });
            }
            if icon.is_some() {
                let a = mesh::point(&chain, 0., 0.);
                let b = mesh::point(&chain, 1., 1.);
                let screen = [
                    cx.state.file.screen[2] as f32,
                    cx.state.file.screen[3] as f32,
                ];
                self.frame.icon_bounds.push([
                    (a[0] * 0.5 + 0.5) * screen[0],
                    (a[1] * 0.5 + 0.5) * screen[1],
                    (b[0] - a[0]) * 0.5 * screen[0],
                    (b[1] - a[1]) * 0.5 * screen[1],
                ]);
            }
            if let Some(icon) = icon
                && !self.geometry_only
            {
                let texture =
                    self.textures
                        .get(&cx.state.file, icon, self.budget, self.cancelled)?;
                let opacity = chain
                    .iter()
                    .fold(sample.opacity, |v, s| v * s.sample.opacity)
                    as f32;
                let mut packet = Packet::new(
                    &chain,
                    layer.mesh_division,
                    texture,
                    Paint {
                        opacity,
                        blend: content.blend,
                        color: content.color,
                    },
                    self.budget,
                    self.cancelled,
                )?;
                packet.visible =
                    !cx.state
                        .removed
                        .contains(&(cx.character.into(), cx.name.into(), index));
                let packet_index = self.frame.packets.len();
                self.frame.packets.push(packet);
                self.nodes.insert((cx.instance, index), packet_index);
                self.masks.push((packet_index, masks.clone()));
                outputs.insert(index, vec![packet_index]);
            }
            if shape {
                let [w, h] = bounds.size.map(|v| v as f32 * 0.5);
                let corners = [[-w, -h], [w, -h], [w, h], [-w, h]].map(|p| {
                    let p = mesh::shape_point(&chain, p);
                    std::array::from_fn(|i| (p[i] * 0.5 + 0.5) * self.viewport[i])
                });
                let min: [f32; 2] = std::array::from_fn(|i| {
                    corners.iter().map(|p| p[i]).fold(f32::INFINITY, f32::min)
                });
                let max: [f32; 2] = std::array::from_fn(|i| {
                    corners
                        .iter()
                        .map(|p| p[i])
                        .fold(f32::NEG_INFINITY, f32::max)
                });
                self.frame.motions[cx.instance]
                    .shapes
                    .push(self.frame.shapes.len());
                self.frame.shapes.push(mesh::HitArea {
                    label: layer.label.clone(),
                    kind: match source.strip_prefix("shape/").unwrap_or("rect") {
                        "point" => 0,
                        "circle" => 1,
                        "quad" => 3,
                        _ => 2,
                    },
                    bounds: [min[0], min[1], max[0] - min[0], max[1] - min[1]],
                    corners,
                });
            }
        }
        for &child in &layer.children {
            if cx.motion.order.contains(&child) {
                self.node(
                    cx,
                    child,
                    tick,
                    Parent {
                        chain: &chain,
                        bounds,
                        masks: &masks,
                    },
                    outputs,
                    depth + 1,
                )?;
            }
        }
        if let Some(selection) = nested {
            let child_instance = self.next_instance;
            outputs.insert(
                index,
                self.motion(&selection, tick + offset, &chain, bounds, &masks, depth + 1)?,
            );
            self.frame.motions[cx.instance]
                .children
                .push((layer.label.clone(), child_instance));
        }
        Ok(())
    }
}
