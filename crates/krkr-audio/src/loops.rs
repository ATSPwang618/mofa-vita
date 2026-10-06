use crate::{Frame, Result, decoder::Decoder};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
};

#[derive(Clone, Debug)]
pub struct Label {
    pub name: String,
    pub position: u64,
}
#[derive(Clone, Copy, Default)]
struct Link {
    from: u64,
    to: u64,
    smooth: bool,
    condition: u8,
    value: i32,
    variable: i32,
}
#[derive(Default)]
pub struct Information {
    links: Vec<Link>,
    pub labels: Vec<Label>,
}
fn number(value: &str) -> i64 {
    let value = value.trim_start();
    let negative = value.starts_with('-');
    let value = if negative {
        value[1..].trim_start()
    } else {
        value
    };
    let n = value
        .bytes()
        .take_while(u8::is_ascii_digit)
        .fold(0i64, |n, c| {
            n.wrapping_mul(10).wrapping_add((c - b'0') as i64)
        });
    if negative { n.wrapping_neg() } else { n }
}
impl Information {
    /// Check offline conversions without rewriting game loop conditions.
    pub fn validate_frames(&self, frames: u64) -> Result<()> {
        if self
            .links
            .iter()
            .any(|link| link.from > frames || link.to >= frames)
            || self.labels.iter().any(|label| label.position > frames)
        {
            return Err("loop/label position exceeds decoded audio sample count".into());
        }
        Ok(())
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 64 * 1024 {
            return Err("loop information exceeds budget".into());
        }
        let bytes = &bytes[..bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len())];
        let input = std::str::from_utf8(bytes).map_err(|_| "loop information must be UTF-8")?;
        let mut info = Self::default();
        if !input.starts_with('#') {
            let start = number(input.split_once("LoopStart=").ok_or("missing LoopStart")?.1);
            let length = number(
                input
                    .split_once("LoopLength=")
                    .ok_or("missing LoopLength")?
                    .1,
            );
            if start < 0 || length < 0 {
                return Err("invalid loop position".into());
            }
            info.links.push(Link {
                from: start.wrapping_add(length) as u64,
                to: start as u64,
                ..Default::default()
            });
            return Ok(info);
        }
        if input.as_bytes().get(..5).is_none_or(|v| v > b"#2.00") {
            return Err("unsupported loop information version".into());
        }
        let mut remaining = input;
        while !remaining.trim().is_empty() {
            remaining = remaining.trim_start();
            if remaining.starts_with('#') {
                remaining = remaining.split_once('\n').map_or("", |(_, r)| r);
                continue;
            }
            let (kind, rest) = remaining.split_once('{').ok_or("invalid loop block")?;
            let mut rest = rest;
            let mut entries = Vec::new();
            loop {
                rest = rest.trim_start();
                if let Some(after) = rest.strip_prefix('}') {
                    remaining = after;
                    break;
                }
                let (key, after) = rest.split_once('=').ok_or("invalid loop field")?;
                rest = after.trim_start();
                let value;
                if let Some(after) = rest.strip_prefix('\'') {
                    let (quoted, after) =
                        after.split_once('\'').ok_or("unterminated loop string")?;
                    value = quoted;
                    rest = after.trim_start();
                } else {
                    let end = rest
                        .find(|c: char| c == ';' || c.is_ascii_whitespace())
                        .ok_or("unterminated loop field")?;
                    value = &rest[..end];
                    rest = rest[end..].trim_start();
                }
                rest = rest
                    .strip_prefix(';')
                    .ok_or("missing loop field terminator")?;
                entries.push((key.trim().to_ascii_lowercase(), value));
            }
            match kind.trim().to_ascii_lowercase().as_str() {
                "link" => {
                    let mut link = Link::default();
                    for (key, value) in entries {
                        match key.as_str() {
                            "from" => {
                                link.from = u64::try_from(number(value))
                                    .map_err(|_| "negative loop position")?
                            }
                            "to" => {
                                link.to = u64::try_from(number(value))
                                    .map_err(|_| "negative loop position")?
                            }
                            "smooth" => {
                                link.smooth = match value.to_ascii_lowercase().as_str() {
                                    "true" | "yes" => true,
                                    "false" | "no" => false,
                                    _ => return Err("invalid loop smooth flag".into()),
                                }
                            }
                            "condition" => {
                                link.condition = match value.to_ascii_lowercase().as_str() {
                                    "no" => 0,
                                    "eq" => 1,
                                    "ne" => 2,
                                    "gt" => 3,
                                    "ge" => 4,
                                    "lt" => 5,
                                    "le" => 6,
                                    _ => return Err("invalid loop condition".into()),
                                }
                            }
                            "refvalue" => link.value = number(value) as i32,
                            "condvar" => link.variable = number(value) as i32,
                            _ => return Err("unknown loop field".into()),
                        }
                    }
                    if !(-1..16).contains(&link.variable) {
                        return Err("invalid loop flag index".into());
                    }
                    info.links.push(link);
                }
                "label" => {
                    let mut label = Label {
                        name: String::new(),
                        position: 0,
                    };
                    for (key, value) in entries {
                        match key.as_str() {
                            "name" => label.name = value.into(),
                            "position" => {
                                label.position = u64::try_from(number(value))
                                    .map_err(|_| "negative label position")?
                            }
                            _ => return Err("unknown label field".into()),
                        }
                    }
                    info.labels.push(label);
                }
                _ => return Err("unknown loop block".into()),
            }
            if info.links.len() + info.labels.len() > 1024 {
                return Err("loop event capacity reached".into());
            }
        }
        info.links.sort_by_key(|l| {
            (
                l.from,
                std::cmp::Reverse(l.condition),
                std::cmp::Reverse(l.variable),
            )
        });
        info.labels.sort_by_key(|l| l.position);
        Ok(info)
    }
    fn nearest(&self, position: u64, flags: &[AtomicI32; 16], conditions: bool) -> Option<Link> {
        self.links.iter().copied().find(|l| {
            l.from >= position
                && (!conditions || l.variable == -1 || {
                    let value = flags[l.variable as usize].load(Ordering::Relaxed);
                    match l.condition {
                        0 => true,
                        1 => value == l.value,
                        2 => value != l.value,
                        3 => value > l.value,
                        4 => value >= l.value,
                        5 => value < l.value,
                        6 => value <= l.value,
                        _ => false,
                    }
                })
        })
    }
}
pub(crate) struct Stream {
    pub decoder: Decoder,
    pub info: Arc<Information>,
    pub flags: Arc<[AtomicI32; 16]>,
    cross: VecDeque<Frame>,
    plain: bool,
    _cross_permit: crate::Permit,
}
impl Stream {
    pub fn new(decoder: Decoder, info: Information, budget: &crate::Budget) -> Result<Self> {
        // Ordinary sound effects and hard loops never need crossfade samples.
        let count = if info.links.iter().any(|link| link.smooth) {
            decoder.format.rate as usize / 20
        } else {
            0
        };
        let permit = budget
            .reserve(count * std::mem::size_of::<Frame>() * 2)
            .map_err(|e| e.to_string())?;
        let cross = VecDeque::with_capacity(count);
        let plain = info.labels.is_empty()
            && info
                .links
                .iter()
                .all(|link| !link.smooth && link.condition == 0);
        Ok(Self {
            decoder,
            info: Arc::new(info),
            flags: Arc::new(std::array::from_fn(|_| AtomicI32::new(0))),
            cross,
            plain,
            _cross_permit: permit,
        })
    }
    pub fn position(&self) -> u64 {
        self.cross
            .front()
            .map_or(self.decoder.position, |f| f.position)
    }
    pub fn seek(&mut self, position: u64) -> Result<()> {
        self.decoder.seek(position)?;
        self.cross.clear();
        Ok(())
    }
    pub fn read_plain(&mut self, output: &mut [Frame]) -> Option<Result<usize>> {
        // Labels and conditional/smooth links stay on the samplewise path.
        // Unconditional hard loops can decode a block up to the next boundary.
        if !self.plain {
            return None;
        }
        if output.is_empty() {
            return Some(Ok(0));
        }
        let position = self.decoder.position;
        let next = self.info.links.iter().find(|link| link.from >= position);
        if next.is_some_and(|link| link.from == position) {
            // Reuse seek chaining and zero-length-cycle checks at the exact
            // boundary, then resume bulk decoding on the following call.
            return Some(self.next().map(|frame| {
                if let Some(frame) = frame {
                    output[0] = frame;
                    1
                } else {
                    0
                }
            }));
        }
        let count = next.map_or(output.len(), |link| {
            (link.from - position).min(output.len() as u64) as usize
        });
        Some(self.decoder.read_frames(&mut output[..count]))
    }
    fn label(&self, mut frame: Frame) -> Frame {
        let start = self
            .info
            .labels
            .partition_point(|l| l.position < frame.position);
        let end = self
            .info
            .labels
            .partition_point(|l| l.position <= frame.position);
        frame.labels = [start as u16, end as u16];
        for label in &self.info.labels[start..end] {
            expression(&label.name, &self.flags);
        }
        frame
    }
    pub fn next(&mut self) -> Result<Option<Frame>> {
        if let Some(frame) = self.cross.pop_front() {
            return Ok(Some(self.label(frame)));
        }
        for _ in 0..10 {
            let position = self.decoder.position;
            let Some(link) = self.info.nearest(position, &self.flags, true) else {
                break;
            };
            if link.from == position {
                self.decoder.seek(link.to)?;
                continue;
            }
            if link.smooth {
                let half = self.decoder.format.rate as u64 * 25 / 1000;
                let before = half.min(link.from).min(link.to).min(link.from - position);
                if position == link.from - before {
                    let total = self.decoder.format.frames;
                    let mut after = half;
                    if total != 0 {
                        after = after
                            .min(total.saturating_sub(link.from))
                            .min(total.saturating_sub(link.to));
                    }
                    if let Some(next) = self.info.nearest(link.to, &self.flags, false) {
                        after = after.min(next.from - link.to);
                    }
                    let count = (before + after) as usize;
                    let mut old = Vec::with_capacity(count);
                    for _ in 0..count {
                        if let Some(frame) = self.decoder.next()? {
                            old.push(frame);
                        } else {
                            break;
                        }
                    }
                    self.decoder.seek(link.to - before)?;
                    for (i, a) in old.into_iter().enumerate() {
                        let Some(b) = self.decoder.next()? else {
                            break;
                        };
                        let t = if (i as u64) < before {
                            0.5 * i as f32 / before.max(1) as f32
                        } else {
                            0.5 + 0.5 * (i as u64 - before) as f32 / after.max(1) as f32
                        };
                        self.cross.push_back(Frame {
                            pcm: std::array::from_fn(|c| {
                                (a.pcm[c] as f32 * (1.0 - t) + b.pcm[c] as f32 * t)
                                    .round()
                                    .clamp(-32768., 32767.) as i16
                            }),
                            sample: [
                                a.sample[0] * (1.0 - t) + b.sample[0] * t,
                                a.sample[1] * (1.0 - t) + b.sample[1] * t,
                            ],
                            position: if (i as u64) < before {
                                position + i as u64
                            } else {
                                link.to + i as u64 - before
                            },
                            labels: [0, 0],
                        });
                    }
                    if let Some(frame) = self.cross.pop_front() {
                        return Ok(Some(self.label(frame)));
                    }
                }
            }
            break;
        }
        // A zero-length cycle cannot be rendered indefinitely.
        if self
            .info
            .nearest(self.decoder.position, &self.flags, true)
            .is_some_and(|l| l.from == self.decoder.position)
        {
            return Err("audio loop cycle makes no progress".into());
        }
        Ok(self.decoder.next()?.map(|f| self.label(f)))
    }
}
fn expression(name: &str, flags: &[AtomicI32; 16]) {
    let Some(rest) = name.strip_prefix(':') else {
        return;
    };
    let compact: String = rest.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let Some(rest) = compact.strip_prefix('[') else {
        return;
    };
    let Some((index, rest)) = rest.split_once(']') else {
        return;
    };
    let Ok(index) = index.parse::<usize>() else {
        return;
    };
    if index >= 16 {
        return;
    }
    let Some(op) = ["+=", "-=", "++", "--", "="]
        .into_iter()
        .find(|op| rest.starts_with(op))
    else {
        return;
    };
    let rhs = &rest[op.len()..];
    let value = if rhs.is_empty() && (op == "++" || op == "--") {
        1
    } else if let Some(rhs) = rhs.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        let Ok(i) = rhs.parse::<usize>() else { return };
        if i >= 16 {
            return;
        }
        flags[i].load(Ordering::Relaxed)
    } else {
        if rhs.is_empty() || !rhs.bytes().all(|c| c.is_ascii_digit()) {
            return;
        }
        number(rhs) as i32
    };
    let old = flags[index].load(Ordering::Relaxed);
    let value = match op {
        "=" => value,
        "+=" => old.wrapping_add(value),
        "-=" => old.wrapping_sub(value),
        "++" => old.wrapping_add(1),
        "--" => old.wrapping_sub(1),
        _ => return,
    };
    flags[index].store(value.clamp(0, 9999), Ordering::Relaxed);
}
