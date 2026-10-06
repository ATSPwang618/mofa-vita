//! Variable timeline sampling from emoteengine::updateTimelineControl.
use super::model::Fields;
use crate::psb::decode::Node;

#[derive(Clone, Debug)]
pub(super) struct Frame {
    pub time: f64,
    pub kind: u8,
    pub content: Option<(f64, f64)>,
}
#[derive(Clone, Debug)]
pub(super) struct Variable {
    pub label: String,
    pub frames: Vec<Frame>,
}
impl Variable {
    pub fn sample(&self, time: f32) -> Option<f32> {
        let mut current = None;
        for (index, frame) in self.frames.iter().enumerate() {
            if frame.time <= time as f64 {
                current = Some(index);
            } else {
                break;
            }
        }
        let index = current?;
        let frame = &self.frames[index];
        let (value, _) = frame.content?;
        if let Some(next) = self.frames.get(index + 1)
            && let Some((target, _)) = next.content
            && (frame.kind != 2 || next.kind == 2)
        {
            return Some(
                (value + (target - value) / (next.time - frame.time) * (time as f64 - frame.time))
                    as f32,
            );
        }
        Some(value as f32)
    }
}
#[derive(Clone, Debug)]
pub(super) struct Timeline {
    pub label: String,
    pub diff: i8,
    pub last_time: i32,
    pub loop_begin: i32,
    pub loop_end: i32,
    pub variables: Vec<Variable>,
}
impl Timeline {
    pub fn read(value: &Node) -> Self {
        Self {
            label: value.get("label").text().into(),
            diff: value.get("diff").integer(0) as i8,
            last_time: value.get("lastTime").integer(0) as i32,
            loop_begin: value.get("loopBegin").integer(0) as i32,
            loop_end: value.get("loopEnd").integer(0) as i32,
            variables: value
                .get("variableList")
                .array()
                .iter()
                .map(|v| Variable {
                    label: v.get("label").text().into(),
                    frames: v
                        .get("frameList")
                        .array()
                        .iter()
                        .map(|f| Frame {
                            time: f.get("time").real(0.),
                            kind: f.get("type").integer(0) as u8,
                            content: matches!(f.get("content"), Node::Object(_)).then(|| {
                                let content = f.get("content");
                                (
                                    content.get("value").real(0.),
                                    content.get("easing").real(0.),
                                )
                            }),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
    pub fn relative_time(&self, tick: f32, start: &mut f32) -> f32 {
        // The original resets at strictly greater than loopEnd and discards
        // overshoot. All active timelines share the engine's start timestamp.
        if self.loop_end > 0 && tick - *start + self.loop_begin as f32 > self.loop_end as f32 {
            *start = tick;
        }
        tick - *start + self.loop_begin as f32
    }
}
