//! Mutable file controls belong to a player, independently of shared PSB data.
use super::{model::Motion, resource::File};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone)]
pub(super) struct State {
    pub file: Arc<File>,
    pub variables: BTreeMap<String, f32>,
    pub motion_variables: BTreeMap<(String, String), BTreeMap<String, f32>>,
    pub timelines: Vec<usize>,
    pub timeline_start: f32,
    pub removed: BTreeSet<(String, String, usize)>,
    pub eyes: Vec<super::eye::Blink>,
}
impl State {
    pub fn new(file: Arc<File>) -> Result<Self, &'static str> {
        let mut state = Self {
            variables: file.metadata.variables.clone(),
            eyes: vec![super::eye::Blink::default(); file.metadata.eyes.len()],
            file,
            motion_variables: BTreeMap::new(),
            timelines: Vec::new(),
            timeline_start: -1.,
            removed: BTreeSet::new(),
        };
        for attribute in &state.file.metadata.attributes {
            for removal in &attribute.removals {
                if removal.value > 0. {
                    continue;
                }
                if let Some(motion) = state
                    .file
                    .objects
                    .get(&removal.character)
                    .and_then(|o| o.motions.get(&removal.motion))
                    && let Some(&index) = motion
                        .order
                        .iter()
                        .find(|&&i| motion.layers[i].label == removal.layer)
                {
                    state.removed.insert((
                        removal.character.clone(),
                        removal.motion.clone(),
                        index,
                    ));
                }
            }
        }
        let selectors: Vec<_> = state
            .file
            .metadata
            .selectors
            .iter()
            .map(|s| s.label.clone())
            .collect();
        for name in selectors {
            state.set_variable(&name, 0.)?;
        }
        Ok(state)
    }
    pub fn set_variable(&mut self, name: &str, value: f64) -> Result<(), &'static str> {
        let mut pending = vec![(name.to_owned(), value, 0usize)];
        while let Some((name, value, depth)) = pending.pop() {
            if depth > 128 {
                return Err("cyclic E-mote selector definitions");
            }
            if self.file.motion {
                continue;
            }
            if let Some(selector) = self
                .file
                .metadata
                .selectors
                .iter()
                .find(|s| s.label == name)
            {
                let index = value as i32;
                if index < 0 || index as usize >= selector.options.len() {
                    continue;
                }
                for (i, option) in selector.options.iter().enumerate().rev() {
                    pending.push((
                        option.label.clone(),
                        if i == index as usize {
                            option.on
                        } else {
                            option.off
                        },
                        depth + 1,
                    ));
                }
            } else if let Some(current) = self.variables.get_mut(&name) {
                *current = value as f32;
            }
        }
        Ok(())
    }
    pub fn parameter_tick(
        &self,
        character: &str,
        name: &str,
        motion: &Motion,
        index: i32,
    ) -> Option<f32> {
        let parameter = motion.parameters.get(usize::try_from(index).ok()?)?;
        if let Some(value) = self.variables.get(&parameter.id) {
            return Some(parameter.tick(*value));
        }
        if self.file.motion
            && let Some(value) = self
                .motion_variables
                .get(&(character.into(), name.into()))
                .and_then(|v| v.get(&parameter.id))
        {
            return Some(if motion.last_time < 0. {
                *value
            } else {
                (*value as f64 * motion.last_time
                    / (i64::from(parameter.end) + 1 - i64::from(parameter.begin)) as f64)
                    as f32
            });
        }
        let value = 0.;
        Some(parameter.tick(value))
    }
    pub fn start_timeline(&mut self, name: &str) -> bool {
        if let Some(index) = self
            .file
            .metadata
            .timelines
            .iter()
            .position(|t| t.label == name)
        {
            self.timelines.push(index);
            self.timeline_start = -10000.;
            true
        } else {
            false
        }
    }
    pub fn stop_timeline(&mut self, name: &str) {
        if let Some(index) = self
            .file
            .metadata
            .timelines
            .iter()
            .position(|t| t.label == name)
        {
            self.timelines.retain(|&i| i != index);
            self.timeline_start = -1.;
        }
    }
    pub fn update_timelines(&mut self, tick: f32) {
        for &index in &self.timelines {
            let timeline = &self.file.metadata.timelines[index];
            let time = timeline.relative_time(tick, &mut self.timeline_start);
            for variable in &timeline.variables {
                if self
                    .file
                    .metadata
                    .selectors
                    .iter()
                    .any(|s| s.options.iter().any(|v| v.label == variable.label))
                {
                    continue;
                }
                if let Some(value) = variable.sample(time)
                    && let Some(target) = self.variables.get_mut(&variable.label)
                {
                    *target = value;
                }
            }
        }
    }
}
