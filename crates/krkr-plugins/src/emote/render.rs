//! Owned render packets can leave the VM thread. Texture pixels are shared
//! across packets and frames; their byte permits stay alive with the cache.
use super::{
    mesh::{self, Mesh, Surface},
    resource::{File, Icon},
};
use krkr_engine::protocol::{
    budget::Budget,
    mesh::{Batch, Blend, Draw, Texture},
    pixels::Pixels,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};
use tjs_core::{NativeError, NativeResult};

pub(super) struct Packet {
    pub mesh: Mesh,
    pub texture: Arc<Pixels>,
    pub paint: Paint,
    pub depth: f32,
    pub masks: Vec<usize>,
    pub visible: bool,
}
#[derive(Default)]
pub(super) struct Frame {
    pub packets: Vec<Packet>,
    /// Reference priority traversal order, before the stable depth sort.
    pub order: Vec<usize>,
    pub shapes: Vec<mesh::HitArea>,
    pub icon_bounds: Vec<[f32; 4]>,
    pub motions: Vec<MotionGeometry>,
}
#[derive(Default)]
pub(super) struct MotionGeometry {
    pub children: Vec<(String, usize)>,
    pub shapes: Vec<usize>,
    pub icons: std::ops::Range<usize>,
}
struct Cached {
    owner: Weak<File>,
    pixels: Arc<Pixels>,
    used: bool,
}
#[derive(Default)]
pub(super) struct Textures {
    // Weak ownership prevents keeping unloaded source files alive. Pointer
    // reuse is checked with Weak::upgrade and Arc::ptr_eq before any cache hit.
    entries: BTreeMap<(usize, usize), Cached>,
}
impl Textures {
    pub fn begin_frame(&mut self) {
        self.entries.retain(|_, v| {
            v.used = false;
            v.owner.strong_count() > 0
        });
    }
    pub fn end_frame(&mut self) {
        self.entries.retain(|_, v| v.used);
    }
    pub fn get(
        &mut self,
        file: &Arc<File>,
        icon: &Icon,
        budget: &Budget,
        cancelled: &dyn Fn() -> bool,
    ) -> NativeResult<Arc<Pixels>> {
        // Definitions remain immutable inside File. The icon address identifies
        // it without allocating two name strings for every packet of a frame;
        // the weak File identity still protects against address reuse.
        let key = (
            Arc::as_ptr(file) as usize,
            std::ptr::from_ref(icon) as usize,
        );
        if let Some(entry) = self.entries.get_mut(&key)
            && entry
                .owner
                .upgrade()
                .is_some_and(|owner| Arc::ptr_eq(&owner, file))
        {
            entry.used = true;
            return Ok(entry.pixels.clone());
        }
        let pixels = Arc::new(file.decode_icon(icon, budget, cancelled)?);
        self.entries.insert(
            key,
            Cached {
                owner: Arc::downgrade(file),
                pixels: pixels.clone(),
                used: true,
            },
        );
        Ok(pixels)
    }
}
impl Packet {
    pub fn new(
        chain: &[Surface],
        division: u8,
        texture: Arc<Pixels>,
        paint: Paint,
        budget: &Budget,
        cancelled: &dyn Fn() -> bool,
    ) -> NativeResult<Self> {
        let divisions = if chain.iter().any(|s| s.kind == 1) {
            if division < 2 { 8 } else { u32::from(division) }
        } else {
            1
        };
        let mesh = mesh::subdivide(chain, [divisions; 2], budget, cancelled)?;
        Ok(Self {
            mesh,
            texture,
            paint,
            depth: chain.first().map_or(0., |s| s.attach.0[14]),
            masks: Vec::new(),
            visible: true,
        })
    }
}
pub(crate) struct Paint {
    pub opacity: f32,
    pub blend: i64,
    pub color: Option<i64>,
}
impl Frame {
    pub fn into_batch(mut self, clear: bool) -> NativeResult<(Batch, Vec<mesh::HitArea>)> {
        if self.order.iter().any(|&i| i >= self.packets.len()) {
            return Err(NativeError::Message("invalid E-mote drawing order"));
        }
        self.order
            .sort_by(|&a, &b| self.packets[a].depth.total_cmp(&self.packets[b].depth));
        let draws = self
            .packets
            .into_iter()
            .map(|packet| {
                let p = packet.paint;
                let color = if p.blend == 21 {
                    p.color.unwrap_or(0) as u32
                } else {
                    p.color
                        .filter(|&c| c as u32 != 0xff808080)
                        .unwrap_or(0xffffffff) as u32
                };
                Draw {
                    geometry: packet.mesh,
                    texture: Texture::Pixels(packet.texture),
                    blend: match p.blend {
                        1 | 4 => Blend::MultiplyAdd,
                        21 => Blend::Alpha,
                        _ => Blend::AlphaMax,
                    },
                    opacity: p.opacity,
                    color: std::array::from_fn(|i| ((color >> (i * 8)) & 255) as f32 / 255.),
                    solid_color: p.blend == 21,
                    masks: packet.masks,
                    visible: packet.visible && p.blend != 6,
                }
            })
            .collect();
        Ok((
            Batch {
                draws,
                order: self.order,
                clear: clear.then_some([0.; 4]),
            },
            self.shapes,
        ))
    }
}
