//! Owned animation data decoded from PSB. Reference: krkrsdl3 emotefile.cpp.
//! Node indices replace native parent pointers; no script handles or GPU objects.
use crate::psb::decode::Node as Value;
use std::collections::BTreeMap;

pub(super) trait Fields {
    fn get(&self, name: &str) -> &Value;
    fn present(&self, name: &str) -> bool;
    fn array(&self) -> &[Value];
    fn object(&self) -> &[(String, Value)];
    fn text(&self) -> &str;
    fn real(&self, default: f64) -> f64;
    fn integer(&self, default: i64) -> i64;
    fn numbers<const N: usize>(&self, default: [f64; N]) -> [f64; N];
}
impl Fields for Value {
    fn get(&self, name: &str) -> &Value {
        self.object()
            .iter()
            .find(|(key, _)| key == name)
            .map_or(&Value::Void, |(_, value)| value)
    }
    fn present(&self, name: &str) -> bool {
        self.object().iter().any(|(key, _)| key == name)
    }
    fn array(&self) -> &[Value] {
        if let Self::Array(v) = self { v } else { &[] }
    }
    fn object(&self) -> &[(String, Value)] {
        if let Self::Object(v) = self { v } else { &[] }
    }
    fn text(&self) -> &str {
        if let Self::String(v) = self { v } else { "" }
    }
    fn real(&self, default: f64) -> f64 {
        match self {
            Self::Int(v) => *v as f64,
            Self::Real(v) => *v,
            _ => default,
        }
    }
    fn integer(&self, default: i64) -> i64 {
        if let Self::Int(v) = self { *v } else { default }
    }
    fn numbers<const N: usize>(&self, default: [f64; N]) -> [f64; N] {
        let a = self.array();
        if a.len() != N {
            return default;
        }
        std::array::from_fn(|i| a[i].real(default[i]))
    }
}
#[derive(Clone, Copy)]
pub(super) struct Format {
    pub krkr: bool,
    pub motion: bool,
}
#[derive(Clone, Debug)]
pub(super) struct Content {
    pub coord: [f64; 3],
    pub angle: f64,
    pub slant: [f64; 2],
    pub zoom: [f64; 2],
    pub origin: [f64; 2],
    pub opacity: f64,
    pub source: String,
    pub time_offset: f64,
    pub blend: i64,
    pub color: Option<i64>,
    pub mesh: Option<[f64; 32]>,
}
#[derive(Clone, Debug)]
pub(super) struct Frame {
    pub time: f64,
    pub kind: u8,
    pub content: Option<Content>,
}
impl Frame {
    fn read(value: &Value, format: Format) -> Self {
        let content = value.present("content").then(|| {
            let c = value.get("content");
            let mut time_offset = 0.;
            let source = if format.krkr {
                c.get("src").text().to_owned()
            } else if !c.present("src") {
                "layout".into()
            } else {
                let prefix = if c.present("motion") {
                    time_offset = c.get("motion").get("timeOffset").real(0.);
                    "motion/"
                } else {
                    "src/"
                };
                let name = c.get("src").text();
                let mut source = format!("{}{name}", if name == "blank" { "" } else { prefix });
                let icon = c.get("icon").text();
                if !icon.is_empty() {
                    source.push('/');
                    source.push_str(icon);
                }
                source.retain(|c| c != '\0');
                source
            };
            let opacity = c.get("opa").real(1.);
            let opacity = if format.motion && matches!(c.get("opa"), Value::Int(_) | Value::Real(_))
            {
                opacity / 255.
            } else {
                opacity
            };
            let mesh = c.get("mesh");
            // The default net uses the source's six-decimal thirds.
            let grid = std::array::from_fn(|i| {
                [0., 0.333333, 0.666667, 1.][if i % 2 == 0 { (i / 2) % 4 } else { i / 8 }]
            });
            Content {
                coord: c.get("coord").numbers([0.; 3]),
                angle: c.get("angle").real(0.),
                slant: [c.get("sx").real(0.), c.get("sy").real(0.)],
                zoom: [c.get("zx").real(1.), c.get("zy").real(1.)],
                origin: [c.get("ox").real(0.), c.get("oy").real(0.)],
                opacity,
                source,
                time_offset,
                blend: c.get("bm").integer(0),
                color: if let Value::Int(v) = c.get("color") {
                    Some(*v)
                } else {
                    None
                },
                mesh: (mesh.get("bp").array().len() == 32).then(|| mesh.get("bp").numbers(grid)),
            }
        });
        Self {
            time: value.get("time").real(0.),
            kind: value.get("type").integer(0) as u8,
            content,
        }
    }
}
#[derive(Clone, Debug)]
pub(super) struct Layer {
    pub children: Vec<usize>,
    pub label: String,
    pub kind: u8,
    pub mesh_division: u8,
    pub inherit_mask: u32,
    pub parameter: Option<i32>,
    pub stencil_layers: Vec<String>,
    pub frames: Vec<Frame>,
    pub ordered_frames: bool,
}
#[derive(Clone, Debug)]
pub(super) struct Parameter {
    pub id: String,
    pub begin: i32,
    pub end: i32,
    pub division: i32,
}
impl Parameter {
    fn read(v: &Value, division: &str) -> Self {
        Self {
            id: v.get("id").text().into(),
            begin: v.get("rangeBegin").integer(0) as i32,
            end: v.get("rangeEnd").integer(1) as i32,
            division: v.get(division).integer(0) as i32,
        }
    }
    pub fn tick(&self, value: f32) -> f32 {
        self.division as f32 * ((value - self.begin as f32) / (self.end as f32 - self.begin as f32))
    }
}
#[derive(Clone, Debug)]
pub(super) struct Motion {
    pub last_time: f64,
    pub loop_time: f64,
    pub self_sync_time: f32,
    pub roots: Vec<usize>,
    pub layers: Vec<Layer>,
    pub order: Vec<usize>,
    pub parameters: Vec<Parameter>,
    pub parameter: Option<i32>,
}
impl Motion {
    pub fn read(
        value: &Value,
        format: Format,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, &'static str> {
        let mut motion = Self {
            last_time: value.get("lastTime").real(0.),
            loop_time: value.get("loopTime").real(0.),
            self_sync_time: 0.,
            roots: Vec::new(),
            layers: Vec::new(),
            order: Vec::new(),
            parameters: value
                .get("parameter")
                .array()
                .iter()
                .map(|v| Parameter::read(v, "division"))
                .collect(),
            parameter: if let Value::Int(v) = value.get("parameterize") {
                Some(*v as i32)
            } else {
                None
            },
        };
        if format.motion && matches!(value.get("parameterize"), Value::Object(_)) {
            motion.parameter = Some(0);
            motion
                .parameters
                .push(Parameter::read(value.get("parameterize"), "discretization"));
        }
        for child in value.get("layer").array() {
            let index = motion.layer(child, format, 0, cancelled)?;
            motion.roots.push(index);
        }
        if let Some(first) = value.get("priority").array().first() {
            for index in first.get("content").array() {
                let index = index.integer(-1) as i32;
                if index >= 0 && (index as usize) < motion.layers.len() {
                    motion.order.push(index as usize);
                }
            }
        }
        // The source traverses the explicit priority list, including duplicates.
        for &index in &motion.order {
            let node = &mut motion.layers[index];
            if node.parameter.is_none() {
                node.parameter = motion.parameter;
            }
            if format.motion && motion.parameter.is_none() {
                for frame in &node.frames {
                    if frame.content.is_some() && frame.time > motion.self_sync_time as f64 {
                        motion.self_sync_time = frame.time as f32;
                    }
                }
            }
        }
        Ok(motion)
    }
    fn layer(
        &mut self,
        value: &Value,
        format: Format,
        depth: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<usize, &'static str> {
        if cancelled() {
            return Err("E-mote loading cancelled");
        }
        if depth > 128 || self.layers.len() >= 1_000_000 {
            return Err("E-mote layer limit exceeded");
        }
        let index = self.layers.len();
        let frames: Vec<_> = value
            .get("frameList")
            .array()
            .iter()
            .map(|v| Frame::read(v, format))
            .collect();
        // Validate once at load time. Unusual/unsorted files retain the legacy
        // first-future-frame behavior instead of changing animation semantics.
        let ordered_frames = frames.iter().all(|frame| frame.time.is_finite())
            && frames.windows(2).all(|pair| pair[0].time <= pair[1].time);
        self.layers.push(Layer {
            children: Vec::new(),
            label: value.get("label").text().into(),
            kind: value.get("type").integer(0) as u8,
            mesh_division: value.get("meshDivision").integer(0) as u8,
            inherit_mask: value.get("inheritMask").integer(0x20007fc) as u32,
            parameter: if let Value::Int(v) = value.get("parameterize") {
                Some(*v as i32)
            } else {
                None
            },
            stencil_layers: value
                .get("stencilCompositeMaskLayerList")
                .array()
                .iter()
                .map(|v| v.text().into())
                .collect(),
            frames,
            ordered_frames,
        });
        for child in value.get("children").array() {
            let child = self.layer(child, format, depth + 1, cancelled)?;
            self.layers[index].children.push(child);
        }
        Ok(index)
    }
}
#[derive(Clone, Debug)]
pub(super) struct Object {
    pub motions: BTreeMap<String, Motion>,
}
pub(super) fn objects(
    root: &Value,
    cancelled: &dyn Fn() -> bool,
) -> Result<BTreeMap<String, Object>, &'static str> {
    let format = Format {
        krkr: root.get("spec").text() == "krkr",
        motion: !matches!(root.get("metadata"), Value::Object(_)),
    };
    root.get("object")
        .object()
        .iter()
        .map(|(name, value)| {
            let motions = value
                .get("motion")
                .object()
                .iter()
                .map(|(name, value)| Ok((name.clone(), Motion::read(value, format, cancelled)?)))
                .collect::<Result<_, &'static str>>()?;
            Ok((name.clone(), Object { motions }))
        })
        .collect()
}
