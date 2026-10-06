//! Controls consumed by the reference playback engine. Physics-only metadata
//! remains available in File.root and the script object, without unused copies.
use super::{model::Fields, timeline::Timeline};
use crate::psb::decode::Node;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(super) struct Eye {
    pub label: String,
    pub begin: f32,
    pub end: f32,
    pub blink_frames: i32,
    pub interval_min: i32,
    pub interval_max: i32,
}
impl Eye {
    fn read(v: &Node) -> Self {
        let begin = v.get("beginFrame").integer(0);
        Self {
            label: v.get("label").text().into(),
            begin: begin as f32,
            end: v.get("endFrame").integer(begin) as f32,
            blink_frames: v.get("blinkFrameCount").integer(0) as i32,
            interval_min: v.get("blinkIntervalMin").integer(0) as i32,
            interval_max: v.get("blinkIntervalMax").integer(0) as i32,
        }
    }
}
#[derive(Clone, Debug)]
pub(super) struct OptionValue {
    pub label: String,
    pub off: f64,
    pub on: f64,
}
#[derive(Clone, Debug)]
pub(super) struct Selector {
    pub label: String,
    pub options: Vec<OptionValue>,
}
#[derive(Clone, Debug)]
pub(super) struct Removal {
    pub character: String,
    pub motion: String,
    pub layer: String,
    pub value: f64,
}
#[derive(Clone, Debug)]
pub(super) struct Attribute {
    pub removals: Vec<Removal>,
}
#[derive(Clone, Debug)]
pub(super) struct Metadata {
    pub character: String,
    pub motion: String,
    pub mirror: bool,
    pub variables: BTreeMap<String, f32>,
    pub timelines: Vec<Timeline>,
    pub selectors: Vec<Selector>,
    pub attributes: Vec<Attribute>,
    pub eyes: Vec<Eye>,
}
impl Metadata {
    pub fn read(v: &Node) -> Result<Self, &'static str> {
        Ok(Self {
            character: v.get("base").get("chara").text().into(),
            motion: v.get("base").get("motion").text().into(),
            mirror: v.get("mirror").integer(0) == 1,
            variables: v
                .get("variableList")
                .array()
                .iter()
                .filter(|v| v.present("label"))
                .map(|v| (v.get("label").text().into(), 0.))
                .collect(),
            timelines: v
                .get("timelineControl")
                .array()
                .iter()
                .map(Timeline::read)
                .collect(),
            selectors: v
                .get("selectorControl")
                .array()
                .iter()
                .map(|s| Selector {
                    label: s.get("label").text().into(),
                    options: s
                        .get("optionList")
                        .array()
                        .iter()
                        .filter(|v| matches!(v, Node::Object(_)))
                        .map(|v| OptionValue {
                            label: v.get("label").text().into(),
                            off: v.get("offValue").real(1.),
                            on: v.get("onValue").real(0.),
                        })
                        .collect(),
                })
                .collect(),
            attributes: v
                .get("attrcomp")
                .array()
                .iter()
                .map(|a| Attribute {
                    removals: a
                        .get("data")
                        .get("remove")
                        .array()
                        .iter()
                        .map(|r| Removal {
                            character: r.get("id").get("chara").text().into(),
                            motion: r.get("id").get("motion").text().into(),
                            layer: r.get("id").get("layer").text().into(),
                            value: r.get("value").real(0.),
                        })
                        .collect(),
                })
                .collect(),
            eyes: v.get("eyeControl").array().iter().map(Eye::read).collect(),
        })
    }
}
