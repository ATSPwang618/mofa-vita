//! One decoded PSB owns its byte ranges, animation definitions and texture atlas
//! descriptions. Resource managers can share it without copying animation trees.
use super::{
    metadata::Metadata,
    model::{self, Fields, Object},
};
use crate::psb::decode::{self, Node};
use std::{collections::BTreeMap, ops::Range, sync::Arc};

#[derive(Clone, Debug)]
pub(super) struct Icon {
    pub position: [f64; 2],
    pub origin: [f64; 2],
    pub size: [f64; 2],
    pub texture_size: [f64; 2],
    pub compression: String,
    pub format: String,
    pub pixels: Option<Range<usize>>,
    pub palette: Option<Range<usize>>,
}
fn bytes(value: &Node) -> Option<Range<usize>> {
    if let Node::Bytes(range) = value {
        Some(range.clone())
    } else {
        None
    }
}
impl Icon {
    fn read(value: &Node, atlas: &Node, krkr: bool) -> Self {
        let mut icon = Self {
            position: [0.; 2],
            origin: [value.get("originX").real(0.), value.get("originY").real(0.)],
            size: [value.get("width").real(0.), value.get("height").real(0.)],
            texture_size: [0.; 2],
            compression: String::new(),
            format: String::new(),
            pixels: None,
            palette: None,
        };
        if krkr {
            icon.compression = if value.present("compress") {
                value.get("compress").text().into()
            } else {
                "none".into()
            };
            icon.pixels = bytes(value.get("pixel"));
            if value.present("pal") {
                icon.format = "pal".into();
                icon.palette = bytes(value.get("pal"));
            }
        } else {
            icon.position = [value.get("left").real(0.), value.get("top").real(0.)];
            icon.texture_size = [atlas.get("width").real(0.), atlas.get("height").real(0.)];
            icon.format = atlas.get("type").text().into();
            icon.pixels = bytes(atlas.get("pixel"));
        }
        icon
    }
}
#[derive(Clone, Debug)]
pub(super) struct Source {
    pub icons: BTreeMap<String, Icon>,
}
pub(super) struct File {
    pub root: Node,
    pub permits: Vec<krkr_engine::protocol::budget::Permit>,
    pub bytes: Arc<[u8]>,
    pub metadata: Metadata,
    pub objects: BTreeMap<String, Object>,
    pub sources: BTreeMap<String, Source>,
    pub screen: [i32; 4],
    pub krkr: bool,
    pub motion: bool,
    pub rgba: bool,
    pub sync_time: f32,
    pub z_max: f32,
}
impl File {
    pub fn decode(bytes: Vec<u8>, cancelled: &dyn Fn() -> bool) -> Result<Self, &'static str> {
        let document = decode::decode(bytes, cancelled)?;
        let root = &document.root;
        if !matches!(root, Node::Object(_)) {
            return Err("E-mote PSB root is not an object");
        }
        let krkr = root.get("spec").text() == "krkr";
        let motion = !matches!(root.get("metadata"), Node::Object(_));
        let metadata = Metadata::read(root.get("metadata"))?;
        let objects = model::objects(root, cancelled)?;
        let mut sources = BTreeMap::new();
        for (name, value) in root.get("source").object() {
            if cancelled() {
                return Err("E-mote loading cancelled");
            }
            let atlas = value.get("texture");
            let icons = value
                .get("icon")
                .object()
                .iter()
                .map(|(name, value)| (name.clone(), Icon::read(value, atlas, krkr)))
                .collect();
            sources.insert(name.clone(), Source { icons });
        }
        let screen = root.get("screenSize");
        let mut sync_time = 0f32;
        let mut z_max = 0f32;
        for frame in objects
            .values()
            .flat_map(|o| o.motions.values())
            .flat_map(|m| &m.layers)
            .flat_map(|l| &l.frames)
        {
            if let Some(content) = &frame.content {
                if frame.time > sync_time as f64 {
                    sync_time = frame.time as f32;
                }
                if content.coord[2].abs() > z_max as f64 {
                    z_max = content.coord[2].abs() as f32;
                }
            }
        }
        Ok(Self {
            sync_time,
            z_max,
            metadata,
            objects,
            sources,
            krkr,
            motion,
            rgba: root.get("spec").text() == "common",
            screen: [
                screen.get("originX").integer(0) as i32,
                screen.get("originY").integer(0) as i32,
                screen.get("width").integer(0) as i32,
                screen.get("height").integer(0) as i32,
            ],
            bytes: document.bytes,
            root: document.root,
            permits: Vec::new(),
        })
    }
    pub fn data(&self, range: &Range<usize>) -> Result<&[u8], &'static str> {
        self.bytes
            .get(range.clone())
            .ok_or("E-mote resource range outside PSB")
    }
}
