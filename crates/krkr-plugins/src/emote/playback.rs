//! Player control state. File data can be shared, while variables, clocks and
//! timeline selection remain owned by the player using that data.
use super::{model::Motion, resource::File, state::State};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Debug)]
pub(super) struct Selection {
    pub storage: Vec<u16>,
    pub character: String,
    pub motion: String,
}
#[derive(Clone)]
pub(super) struct Playback {
    pub files: BTreeMap<Vec<u16>, State>,
    pub main: Option<Vec<u16>>,
    pub selected: Option<Selection>,
    pub motion_key: Vec<u16>,
    pub character: String,
    pub motion: String,
    pub old_motion: bool,
    pub clock: f64,
    pub speed: f64,
    pub playing: bool,
    pub all_playing: bool,
    pub stopped: bool,
    pub self_clear: bool,
    pub variable_history: BTreeMap<String, f64>,
    pub sample_tick: f32,
    stop_time: f32,
}
impl Playback {
    pub fn new(old_motion: bool) -> Self {
        Self {
            files: BTreeMap::new(),
            main: None,
            selected: None,
            motion_key: Vec::new(),
            character: String::new(),
            motion: String::new(),
            old_motion,
            clock: -1.,
            speed: 20.,
            playing: false,
            all_playing: false,
            stopped: false,
            self_clear: false,
            variable_history: BTreeMap::new(),
            sample_tick: 0.,
            stop_time: 0.,
        }
    }
    pub fn refresh(
        &mut self,
        files: impl IntoIterator<Item = (Vec<u16>, Arc<File>)>,
    ) -> Result<(), &'static str> {
        for (name, file) in files {
            if self
                .files
                .get(&name)
                .is_some_and(|s| Arc::ptr_eq(&s.file, &file))
            {
                continue;
            }
            self.files.insert(name, State::new(file)?);
        }
        self.update_stop_time();
        Ok(())
    }
    pub fn set_motion_key(&mut self, name: Vec<u16>) {
        self.main = self.files.contains_key(&name).then(|| name.clone());
        self.motion_key = name;
    }
    fn default_motion(storage: &[u16], file: &File) -> Option<Selection> {
        let character = &file.metadata.character;
        let motion = &file.metadata.motion;
        file.objects.get(character)?.motions.get(motion)?;
        Some(Selection {
            storage: storage.to_vec(),
            character: character.clone(),
            motion: motion.clone(),
        })
    }
    pub fn play(&mut self, name: String) {
        if !self.old_motion
            && let Some(storage) = &self.main
        {
            if let Some(selection) = Self::default_motion(storage, &self.files[storage].file) {
                self.selected = Some(selection);
            }
            self.self_clear = true;
        } else if self.old_motion && !self.files.is_empty() {
            self.selected = None;
            for (storage, state) in &self.files {
                if state
                    .file
                    .objects
                    .get(&self.character)
                    .is_some_and(|o| o.motions.contains_key(&name))
                {
                    self.main = Some(storage.clone());
                    self.selected = Some(Selection {
                        storage: storage.clone(),
                        character: self.character.clone(),
                        motion: name.clone(),
                    });
                    break;
                }
            }
            self.self_clear = true;
        } else {
            self.selected = self
                .files
                .iter()
                .find_map(|(name, state)| Self::default_motion(name, &state.file));
            self.main = self.selected.as_ref().map(|s| s.storage.clone());
            self.self_clear = false;
        }
        self.motion = name;
        self.clock = 0.;
        self.playing = true;
        self.all_playing = true;
        self.update_stop_time();
    }
    fn update_stop_time(&mut self) {
        self.stop_time = self
            .selected
            .as_ref()
            .and_then(|s| {
                let file = &self.files.get(&s.storage)?.file;
                let motion = file.objects.get(&s.character)?.motions.get(&s.motion)?;
                let sync = sync_time(file, &s.character, &s.motion);
                Some(if sync > 0. {
                    sync
                } else if motion.self_sync_time > 0. {
                    motion.self_sync_time
                } else {
                    motion.last_time as f32
                })
            })
            .unwrap_or(0.);
    }
    pub fn skip_to_sync(&mut self) {
        if self.motion_definition().is_some()
            && let Some(state) = self.main.as_ref().and_then(|n| self.files.get(n))
        {
            self.clock = state.file.sync_time as f64;
        }
    }
    pub fn motion_definition(&self) -> Option<&Motion> {
        let s = self.selected.as_ref()?;
        self.files
            .get(&s.storage)?
            .file
            .objects
            .get(&s.character)?
            .motions
            .get(&s.motion)
    }
    pub fn loop_time(&self) -> i32 {
        self.main
            .as_ref()
            .and_then(|name| self.files.get(name))
            .and_then(|s| s.file.objects.values().next())
            .and_then(|o| o.motions.values().next())
            .map_or(0, |m| m.loop_time as i32)
    }
    pub fn advance(&mut self, milliseconds: f64, size: [f64; 2], origin: [f64; 2]) -> bool {
        if self.stopped || self.clock <= -1. || size[0] == origin[0] || size[1] == origin[1] {
            return false;
        }
        let Some(selection) = self.selected.clone() else {
            return false;
        };
        if self.motion_definition().is_none() {
            return false;
        }
        if self.playing {
            self.clock += milliseconds / self.speed;
        }
        if !self.files[&selection.storage].variables.is_empty() {
            let state = self.files.get_mut(&selection.storage).unwrap();
            super::eye::update(
                &state.file.metadata.eyes,
                &mut state.eyes,
                &mut state.variables,
                self.clock as f32,
            );
            state.update_timelines(self.clock as f32);
            self.sample_tick = 0.;
        } else {
            let definition = self.motion_definition().unwrap();
            let last = definition.last_time;
            let loop_time = definition.loop_time;
            if !self.old_motion && self.clock > last {
                self.playing = false;
            }
            if self.old_motion && loop_time < 0. {
                let end = self.stop_time;
                if self.clock > end as f64 {
                    self.clock = end as f64;
                    self.playing = false;
                }
            }
            self.sample_tick = self.clock as f32;
        }
        true
    }
    pub fn set_variable(&mut self, name: &str, value: f64) -> Result<(), &'static str> {
        let qualified = name.split_once('/');
        for (storage, state) in &mut self.files {
            state.set_variable(name, value)?;
            // Qualified object/variable names address the main file only.
            if qualified.is_some() && self.main.as_ref() != Some(storage) {
                continue;
            }
            for (character, object) in &state.file.objects {
                let variable = if let Some((object, variable)) = qualified {
                    if character != object {
                        continue;
                    }
                    variable
                } else {
                    name
                };
                for (motion, definition) in &object.motions {
                    if definition.parameters.iter().any(|p| p.id == variable) {
                        state
                            .motion_variables
                            .entry((character.clone(), motion.clone()))
                            .or_default()
                            .insert(variable.into(), value as f32);
                    }
                }
            }
        }
        self.variable_history.insert(name.into(), value);
        Ok(())
    }
    pub fn variable(&self, name: &str) -> f64 {
        let Some(state) = self.main.as_ref().and_then(|name| self.files.get(name)) else {
            return 0.;
        };
        if !state.file.motion {
            return state.variables.get(name).copied().unwrap_or(0.) as f64;
        }
        let qualified = name.split_once('/');
        for (character, object) in &state.file.objects {
            let variable = if let Some((object, variable)) = qualified {
                if character != object {
                    continue;
                }
                variable
            } else {
                name
            };
            for (motion, definition) in &object.motions {
                if definition.parameters.iter().any(|p| p.id == variable) {
                    return state
                        .motion_variables
                        .get(&(character.clone(), motion.clone()))
                        .and_then(|v| v.get(variable))
                        .copied()
                        .unwrap_or(0.) as f64;
                }
            }
        }
        0.
    }
}
/// Same-file nested motions contribute to the final stop timestamp. Repeated
/// references are visited once because the operation is a maximum, not a sum.
fn sync_time(file: &File, character: &str, name: &str) -> f32 {
    let Some(root) = file
        .objects
        .get(character)
        .and_then(|o| o.motions.get(name))
    else {
        return 0.;
    };
    if root.parameter.is_some() {
        return 0.;
    }
    let mut result = 0f32;
    let mut pending = vec![(character, name)];
    let mut visited = BTreeSet::new();
    while let Some((character, name)) = pending.pop() {
        if !visited.insert((character, name)) {
            continue;
        }
        let Some(motion) = file
            .objects
            .get(character)
            .and_then(|o| o.motions.get(name))
        else {
            continue;
        };
        for &index in &motion.order {
            let layer = &motion.layers[index];
            for frame in &layer.frames {
                if let Some(content) = &frame.content {
                    if layer.parameter.is_none() && frame.time > result as f64 {
                        result = frame.time as f32;
                    }
                    let mut path = content.source.split('/');
                    if path.next() == Some("motion")
                        && let (Some(character), Some(name)) = (path.next(), path.next())
                    {
                        pending.push((character, name));
                    }
                }
            }
        }
    }
    result
}
