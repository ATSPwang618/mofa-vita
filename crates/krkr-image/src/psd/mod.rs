//! Indexed PSD v1 reader for the portable psd plugin. Layer channels remain in
//! the source; immutable plans open independent streams for every decode.
mod decode;
mod descriptor;
mod metadata;
mod reader;
use crate::{Error, Result};
pub use decode::{BmpStream, Decoder, Image};
pub use descriptor::Meta;
use descriptor::{int, map};
use krkr_assets::{Limits, ReadPlan, Stream};
use krkr_protocol::graphics::Size;
use reader::{Reader, slice};
use std::{collections::BTreeMap, sync::Arc};

pub const BLENDS: [(&str, &[u8; 4]); 28] = [
    ("normal", b"norm"),
    ("dissolve", b"diss"),
    ("darken", b"dark"),
    ("multiply", b"mul "),
    ("color_burn", b"idiv"),
    ("linear_burn", b"lbrn"),
    ("lighten", b"lite"),
    ("screen", b"scrn"),
    ("color_dodge", b"div "),
    ("linear_dodge", b"lddg"),
    ("overlay", b"over"),
    ("soft_light", b"sLit"),
    ("hard_light", b"hLit"),
    ("vivid_light", b"vLit"),
    ("linear_light", b"lLit"),
    ("pin_light", b"pLit"),
    ("hard_mix", b"hMix"),
    ("difference", b"diff"),
    ("exclusion", b"smud"),
    ("hue", b"hue "),
    ("saturation", b"sat "),
    ("color", b"colr"),
    ("luminosity", b"lum "),
    ("pass_through", b"pass"),
    ("darker_color", b"dkCl"),
    ("lighter_color", b"ltCl"),
    ("subtract", b"fsub"),
    ("divide", b"fdiv"),
];
fn blend(key: [u8; 4]) -> i32 {
    BLENDS
        .iter()
        .position(|(_, k)| **k == key)
        .map_or(-1, |i| i as i32)
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Bounds {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
}
impl Bounds {
    pub fn width(self) -> i64 {
        i64::from(self.right) - i64::from(self.left)
    }
    pub fn height(self) -> i64 {
        i64::from(self.bottom) - i64::from(self.top)
    }
    pub fn size(self) -> Result<Size> {
        let (w, h) = (self.width(), self.height());
        if !(1..=30000).contains(&w) || !(1..=30000).contains(&h) {
            return Err(Error::Message("invalid PSD image dimensions"));
        }
        Ok(Size {
            width: w as u32,
            height: h as u32,
        })
    }
}
#[derive(Clone, Default)]
pub struct Mask {
    pub bounds: Bounds,
    pub default_color: u8,
    pub real: Option<(Bounds, u8)>,
}
#[derive(Clone)]
struct Channel {
    id: i16,
    offset: u64,
    length: u32,
}
#[derive(Clone)]
pub struct Layer {
    pub bounds: Bounds,
    channels: Vec<Channel>,
    pub mask: Mask,
    pub name: Vec<u16>,
    pub id: i32,
    pub kind: i32,
    pub blend: i32,
    pub opacity: u8,
    pub fill_opacity: u8,
    pub clipping: u8,
    pub flags: u8,
    pub parent: Option<usize>,
    pub comps: BTreeMap<String, Meta>,
}
impl Layer {
    pub fn visible(&self) -> bool {
        self.flags & 2 == 0
    }
    pub fn mask_channel(&self) -> bool {
        self.channels.iter().any(|c| matches!(c.id, -2 | -3))
    }
    pub fn mask_bounds(&self) -> (Bounds, u8) {
        if self
            .channels
            .iter()
            .rev()
            .find(|c| matches!(c.id, -2 | -3))
            .is_some_and(|c| c.id == -3)
        {
            self.mask
                .real
                .unwrap_or((self.mask.bounds, self.mask.default_color))
        } else {
            (self.mask.bounds, self.mask.default_color)
        }
    }
    pub fn info(&self, doc: &Document, layer_type: i32) -> Meta {
        let b = self.bounds;
        let Meta::Map(mut m) = map([
            ("top", int(b.top)),
            ("left", int(b.left)),
            ("bottom", int(b.bottom)),
            ("right", int(b.right)),
            ("width", int(b.width())),
            ("height", int(b.height())),
            ("opacity", int(self.opacity)),
            ("fill_opacity", int(self.fill_opacity)),
            ("mask", int(i32::from(self.mask_channel()))),
            ("type", int(layer_type)),
            ("layer_type", int(self.kind)),
            ("blend_mode", int(self.blend)),
            ("visible", int(i32::from(self.visible()))),
            ("name", Meta::Text(self.name.clone())),
            ("clipping", int(self.clipping)),
            ("layer_id", int(self.id)),
            ("obsolete", int(i32::from(self.flags & 4 != 0))),
            (
                "transparency_protected",
                int(i32::from(self.flags & 1 != 0)),
            ),
            (
                "pixel_data_irrelevant",
                int(i32::from(self.flags & 16 != 0)),
            ),
        ]) else {
            unreachable!()
        };
        if let Some(parent) = self.parent {
            m.insert("group_layer_id".into(), int(doc.layers[parent].id));
        }
        if !self.comps.is_empty() {
            m.insert("layer_comp".into(), Meta::Map(self.comps.clone()));
        }
        Meta::Map(m)
    }
}
pub struct Document {
    plan: Arc<ReadPlan>,
    pub size: Size,
    pub channels: u16,
    pub depth: u16,
    pub color_mode: u16,
    pub layers: Vec<Layer>,
    pub guides: Meta,
    pub slices: Meta,
    pub comps: Meta,
    palette: [[u8; 4]; 256],
    merged: u64,
}
impl Document {
    pub fn load(plan: Arc<ReadPlan>, limits: Limits) -> Result<Self> {
        let mut loader = Loader::new(plan, limits)?;
        while !loader.advance()? {}
        Ok(loader.finish())
    }
    pub fn has_merged(&self) -> bool {
        self.merged < self.plan.bytes
    }
    pub fn storage_index(&self) -> (BTreeMap<i32, usize>, BTreeMap<Vec<u16>, usize>) {
        let mut ids = BTreeMap::new();
        let mut paths = BTreeMap::new();
        let clean = |name: &[u16]| {
            krkr_assets::name::fold(
                &name
                    .iter()
                    .map(|&c| if c == 47 { 95 } else { c })
                    .collect::<Vec<_>>(),
            )
        };
        for (i, layer) in self.layers.iter().enumerate().rev() {
            if layer.kind != 0 {
                continue;
            }
            ids.insert(layer.id, i);
            let mut parts = vec![clean(&layer.name)];
            let mut parent = layer.parent;
            while let Some(p) = parent {
                parts.push(clean(&self.layers[p].name));
                parent = self.layers[p].parent;
            }
            let mut path = krkr_assets::name::units("root");
            for p in parts.iter().rev() {
                path.push(47);
                path.extend(p);
            }
            path.extend(krkr_assets::name::units(".bmp"));
            paths.insert(path, i);
        }
        (ids, paths)
    }
}

enum Stage {
    Resources(u64),
    Sections,
    Records { end: u64, count: usize },
    Channels { end: u64, index: usize },
    Done,
}
/// Each advance reads one resource or layer record. No image channels are
/// decoded during load; native callers can yield between advances.
pub struct Loader {
    reader: Reader<Box<dyn Stream>>,
    doc: Document,
    stage: Stage,
    limits: Limits,
    metadata_bytes: usize,
}
impl Loader {
    pub fn new(plan: Arc<ReadPlan>, limits: Limits) -> Result<Self> {
        let mut r = Reader::new(plan.open()?, plan.bytes);
        if r.key()? != *b"8BPS" || r.u16()? != 1 {
            return Err(Error::Message("not a PSD v1 document"));
        }
        r.skip(6)?;
        let channels = r.u16()?;
        let height = r.u32()?;
        let width = r.u32()?;
        let depth = r.u16()?;
        let color_mode = r.u16()?;
        if channels == 0
            || channels > 56
            || !(1..=30000).contains(&width)
            || !(1..=30000).contains(&height)
            || !matches!(depth, 1 | 8 | 16 | 32)
            || !matches!(color_mode, 0 | 1 | 2 | 3 | 4 | 7 | 8 | 9)
        {
            return Err(Error::Message("invalid PSD header"));
        }
        let mut palette = [[0, 0, 0, 255]; 256];
        let color_end = r.block_end()?;
        if color_mode == 2 {
            if color_end - r.pos != 768 {
                return Err(Error::Message("invalid PSD indexed palette"));
            }
            for c in 0..3 {
                for p in &mut palette {
                    p[c] = r.u8()?;
                }
            }
        }
        r.seek(color_end)?;
        let resource_end = r.block_end()?;
        Ok(Self {
            reader: r,
            doc: Document {
                merged: plan.bytes,
                plan,
                size: Size { width, height },
                channels,
                depth,
                color_mode,
                layers: Vec::new(),
                guides: Meta::Void,
                slices: Meta::Void,
                comps: Meta::Void,
                palette,
            },
            stage: Stage::Resources(resource_end),
            limits,
            metadata_bytes: 0,
        })
    }
    fn metadata(&mut self, length: usize) -> Result<Vec<u8>> {
        self.metadata_bytes = self
            .metadata_bytes
            .checked_add(length)
            .ok_or(Error::Message("PSD metadata overflow"))?;
        if self.metadata_bytes > self.limits.max_index_bytes {
            return Err(Error::Message("PSD metadata budget exceeded"));
        }
        self.reader.bytes(length)
    }
    pub fn advance(&mut self) -> Result<bool> {
        match self.stage {
            Stage::Resources(end) => {
                if self.reader.pos == end {
                    self.stage = Stage::Sections;
                    return Ok(false);
                }
                if self.reader.key()? != *b"8BIM" {
                    return Err(Error::Message("invalid PSD resource signature"));
                }
                let id = self.reader.u16()?;
                let n = u64::from(self.reader.u8()?) + 1;
                self.reader.skip(n - 1 + (n & 1))?;
                let size = self.reader.u32()? as usize;
                if self.reader.pos + size as u64 + (size as u64 & 1) > end {
                    return Err(Error::Message("PSD resource crosses section"));
                }
                if matches!(id, 1032 | 1046 | 1047 | 1050 | 1065) {
                    let data = self.metadata(size)?;
                    metadata::resource(&mut self.doc, id, &data)?;
                } else {
                    self.reader.skip(size as u64)?;
                }
                self.reader.skip(size as u64 & 1)?;
            }
            Stage::Sections => {
                let end = self.reader.block_end()?;
                self.doc.merged = end;
                if self.reader.pos == end {
                    self.stage = Stage::Done;
                    return Ok(false);
                }
                let file_end = self.reader.end;
                self.reader.end = end;
                let primary_end = self.reader.block_end()?;
                let mut selected = (self.reader.pos, primary_end);
                self.reader.seek(primary_end)?;
                if self.reader.remaining() >= 4 {
                    let global_end = self.reader.block_end()?;
                    self.reader.seek(global_end)?;
                }
                let mut blocks = 0;
                while self.reader.remaining() >= 12 {
                    blocks += 1;
                    if blocks > self.limits.max_entries {
                        return Err(Error::Message("PSD additional block limit exceeded"));
                    }
                    let sig = self.reader.key()?;
                    if sig != *b"8BIM" && sig != *b"8B64" {
                        return Err(Error::Message("invalid PSD additional signature"));
                    }
                    let key = self.reader.key()?;
                    let block_end = self.reader.block_end()?;
                    let block_length = block_end - self.reader.pos;
                    if (key == *b"Lr16" && self.doc.depth == 16)
                        || (key == *b"Lr32" && self.doc.depth == 32)
                    {
                        selected = (self.reader.pos, block_end);
                    }
                    self.reader.seek(block_end)?;
                    // Global tagged blocks are padded to four bytes.
                    let pad = (4 - block_length % 4) % 4;
                    if pad <= self.reader.remaining() {
                        self.reader.skip(pad)?;
                    }
                }
                self.reader.end = file_end;
                self.reader.seek(selected.0)?;
                if selected.0 == selected.1 {
                    self.stage = Stage::Done;
                    return Ok(false);
                }
                self.reader.end = selected.1;
                let count = i32::from(self.reader.u16()? as i16).unsigned_abs() as usize;
                if count > self.limits.max_entries {
                    return Err(Error::Message("PSD layer count exceeds limit"));
                }
                self.stage = Stage::Records {
                    end: selected.1,
                    count,
                };
            }
            Stage::Records { end, count } => {
                if self.doc.layers.len() == count {
                    self.stage = Stage::Channels { end, index: 0 };
                    return Ok(false);
                }
                let bounds = read_bounds(&mut self.reader)?;
                let count = self.reader.u16()? as usize;
                if count > 56 {
                    return Err(Error::Message("PSD layer channel count exceeds limit"));
                }
                let mut channels = Vec::new();
                for _ in 0..count {
                    channels.push(Channel {
                        id: self.reader.u16()? as i16,
                        offset: 0,
                        length: self.reader.u32()?,
                    });
                }
                if self.reader.key()? != *b"8BIM" {
                    return Err(Error::Message("invalid PSD layer blend signature"));
                }
                let mode = blend(self.reader.key()?);
                let opacity = self.reader.u8()?;
                let clipping = self.reader.u8()?;
                let flags = self.reader.u8()?;
                self.reader.skip(1)?;
                let extra = self.reader.u32()? as usize;
                let data = self.metadata(extra)?;
                let mut layer = Layer {
                    bounds,
                    channels,
                    mask: Mask::default(),
                    name: Vec::new(),
                    id: -1,
                    kind: 0,
                    blend: mode,
                    opacity,
                    fill_opacity: 255,
                    clipping,
                    flags,
                    parent: None,
                    comps: BTreeMap::new(),
                };
                metadata::layer(&mut layer, &data)?;
                self.doc.layers.push(layer);
            }
            Stage::Channels { end, index } => {
                if index == self.doc.layers.len() {
                    self.stage = Stage::Done;
                    return Ok(false);
                }
                for channel in &mut self.doc.layers[index].channels {
                    channel.offset = self.reader.pos;
                    self.reader.skip(u64::from(channel.length))?;
                }
                if self.reader.pos > end {
                    return Err(Error::Message("PSD channels exceed layer section"));
                }
                self.stage = Stage::Channels {
                    end,
                    index: index + 1,
                };
            }
            Stage::Done => return Ok(true),
        }
        Ok(false)
    }
    pub fn finish(mut self) -> Document {
        let mut stack = Vec::new();
        for i in (0..self.doc.layers.len()).rev() {
            let layer = &mut self.doc.layers[i];
            layer.parent = stack.last().copied();
            match layer.kind {
                2 => stack.push(i),
                1 => {
                    stack.pop();
                }
                _ => {}
            }
        }
        self.doc
    }
}
fn read_bounds<R: std::io::Read + std::io::Seek>(r: &mut Reader<R>) -> Result<Bounds> {
    Ok(Bounds {
        top: r.i32()?,
        left: r.i32()?,
        bottom: r.i32()?,
        right: r.i32()?,
    })
}
