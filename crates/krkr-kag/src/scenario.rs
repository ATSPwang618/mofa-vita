use crate::{Error, Result, Text, units};
use std::{collections::BTreeMap, ops::Range, sync::OnceLock};

pub struct Scenario {
    source: Box<[u16]>,
    lines: Vec<Range<usize>>,
    labels: OnceLock<Result<Labels>>,
}
pub struct Labels {
    pub by_name: BTreeMap<Text, usize>,
    pub aliases: Vec<Text>,
    lines: Vec<usize>,
}
impl Labels {
    pub(crate) fn before(&self, line: usize) -> Option<(usize, &Text)> {
        let at = self
            .lines
            .partition_point(|&index| index < line)
            .checked_sub(1)?;
        let index = self.lines[at];
        Some((index, &self.aliases[index]))
    }
}
impl Scenario {
    pub fn new(mut source: Text) -> Result<Self> {
        source.truncate(source.iter().position(|&u| u == 0).unwrap_or(source.len()));
        let mut lines = Vec::new();
        let mut start = 0;
        while start < source.len() {
            let mut end = start;
            while end < source.len() && !matches!(source[end], 10 | 13) {
                end += 1;
            }
            let next = end
                + if source.get(end) == Some(&13) && source.get(end + 1) == Some(&10) {
                    2
                } else {
                    1
                };
            while source.get(start) == Some(&9) && start < end {
                start += 1;
            }
            if end < source.len() || start < end {
                lines.push(start..end);
            }
            start = next;
        }
        if lines.is_empty() {
            return Err(Error::new(0, 0, "empty scenario"));
        }
        Ok(Self {
            source: source.into_boxed_slice(),
            lines,
            labels: OnceLock::new(),
        })
    }
    pub fn line(&self, index: usize) -> Option<&[u16]> {
        self.lines
            .get(index)
            .map(|range| &self.source[range.clone()])
    }
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    pub fn source_bytes(&self) -> usize {
        self.source.len() * 2
    }
    pub fn labels(&self) -> Result<&Labels> {
        self.labels
            .get_or_init(|| {
                let mut by_name = BTreeMap::new();
                let mut aliases = vec![Vec::new(); self.lines.len()];
                let mut counts: BTreeMap<&[u16], usize> = BTreeMap::new();
                let mut previous: &[u16] = &[];
                let mut lines = Vec::new();
                for (line, alias) in aliases.iter_mut().enumerate() {
                    let text = self.line(line).expect("indexed line");
                    if text.first() != Some(&42) || text.len() < 2 {
                        continue;
                    }
                    let mut label = text.split(|&u| u == 124).next().unwrap();
                    if label.len() == 1 {
                        if previous.is_empty() {
                            return Err(Error::new(line, 0, "first label name cannot be omitted"));
                        }
                        label = previous;
                    }
                    previous = label;
                    let count = counts.entry(label).or_default();
                    *count += 1;
                    let mut label = label.to_vec();
                    if *count > 1 {
                        label.extend(units(&format!(":{count}")));
                    }
                    by_name.insert(label.clone(), line);
                    *alias = label;
                    lines.push(line);
                }
                Ok(Labels {
                    by_name,
                    aliases,
                    lines,
                })
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}
