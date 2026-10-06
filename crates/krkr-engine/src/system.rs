//! Portable System services and host configuration. Window, shell, registry and
//! graphics services are supplied by the later platform/media host.
mod bindings;
pub(crate) mod extension;
pub mod files;
use crate::operations;
use krkr_assets::{local, name};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    time::Duration,
};
use tjs_core::{Heap, NativeError, NativeResult, ObjId, Trace, Value};
use tjs_runtime::clock::Clock;

/// Platform operations carry owned/plain data and no VM or native-state borrow
/// across calls. The desktop and Vita hosts implement this independently.
pub trait SystemHost {
    fn create_app_lock(&mut self, name: &[u16]) -> Result<bool, String>;
    fn file_attributes(&mut self, _path: &[u16]) -> Result<u32, String> {
        Err("file attributes require a filesystem host".into())
    }
    fn change_file_attributes(
        &mut self,
        _path: &[u16],
        _mask: u32,
        _set: bool,
    ) -> Result<bool, String> {
        Err("file attributes require a filesystem host".into())
    }
    fn file_display_name(&mut self, _path: &[u16]) -> Result<Vec<u16>, String> {
        Err("file display names require a filesystem host".into())
    }
}

pub struct SystemConfig {
    pub title: Vec<u16>,
    pub exe_name: Vec<u16>,
    /// Application resource directory. A game launcher supplies its game root.
    pub exe_path: Vec<u16>,
    pub data_path: Vec<u16>,
    pub personal_path: Vec<u16>,
    pub app_data_path: Vec<u16>,
    /// Raw platform path, matching the original savedGamesPath property.
    pub saved_games_path: Vec<u16>,
    pub host: Option<Box<dyn SystemHost>>,
    pub arguments: BTreeMap<Vec<u16>, Vec<u16>>,
    /// Host pacing policy for continuous callbacks, not an animation clock.
    pub continuous_interval: Duration,
}
impl SystemConfig {
    pub fn for_process() -> NativeResult<Self> {
        let io = |e: std::io::Error| NativeError::Detail(e.to_string());
        let assets = |e: krkr_assets::Error| NativeError::Detail(e.to_string());
        let exe = std::env::current_exe().map_err(io)?;
        let directory =
            local::directory(exe.parent().expect("absolute executable path")).map_err(assets)?;
        Ok(Self {
            title: name::units("krkr-rs"),
            exe_path: directory.clone(),
            exe_name: name::normalize(&local::units(&exe).map_err(assets)?, &directory)
                .map_err(assets)?,
            data_path: local::directory(&std::env::current_dir().map_err(io)?.join("savedata"))
                .map_err(assets)?,
            arguments: BTreeMap::new(),
            personal_path: directory.clone(),
            app_data_path: directory,
            saved_games_path: Vec::new(),
            host: None,
            continuous_interval: Duration::from_millis(16),
        })
    }
}

pub(crate) type Shared = Rc<RefCell<System>>;
pub(crate) struct System {
    pub input: Option<krkr_protocol::window::Client>,
    pub clock: Rc<dyn Clock>,
    pub operations: operations::Shared,
    pub config: SystemConfig,
    pub event_disabled: bool,
    pub exit: Option<i32>,
    pub exit_on_window_close: bool,
    pub exit_on_no_window_startup: bool,
    pub class: Option<ObjId>,
    pub active: bool,
    pub activations: VecDeque<bool>,
    pub limit: usize,
    handlers: Vec<Option<Value>>,
    round: Option<(usize, i64)>,
    next_tick: Option<Duration>,
    pub continuous_busy: bool,
    pub breathing: bool,
}
impl Trace for System {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for value in self.handlers.iter().flatten() {
            visit(*value);
        }
    }
}
impl System {
    pub fn main_window_closed(&mut self) {
        if self.exit_on_window_close {
            self.exit = Some(0);
        }
    }
    pub fn set_active(&mut self, active: bool) -> bool {
        if self.active == active {
            return true;
        }
        if self.activations.len() >= self.limit {
            return false;
        }
        self.active = active;
        self.activations.push_back(active);
        true
    }
    pub fn add(&mut self, function: Value) -> NativeResult<()> {
        let Value::Obj(reference) = function else {
            return Err(NativeError::Type("a function object"));
        };
        if reference.object.is_none() {
            return Ok(());
        }
        if self
            .handlers
            .iter()
            .any(|entry| matches!(entry, Some(Value::Obj(r)) if *r == reference))
        {
            return Ok(());
        }
        if self.handlers.len() >= self.limit {
            return Err(NativeError::Message("continuous handler capacity reached"));
        }
        // Append during delivery: newly registered handlers join this round,
        // while a removed handler's old position stays empty.
        self.handlers.push(Some(function));
        if self.next_tick.is_none() {
            self.next_tick = Some(self.clock.now());
        }
        Ok(())
    }
    pub fn remove(&mut self, function: Value) {
        let Value::Obj(reference) = function else {
            return;
        };
        for entry in &mut self.handlers {
            if matches!(entry, Some(Value::Obj(r)) if *r == reference) {
                *entry = None;
            }
        }
        if self.round.is_none() {
            self.handlers.retain(Option::is_some);
        }
        if self.handlers.iter().all(Option::is_none) {
            self.next_tick = None;
        }
    }
    pub fn next_continuous(&mut self) -> Option<(Value, i64)> {
        if self.continuous_busy {
            return None;
        }
        let now = self.clock.now();
        if self.round.is_none() && self.next_tick.is_some_and(|at| at <= now) {
            self.round = Some((0, now.as_millis() as i64));
            self.next_tick = Some(now.saturating_add(self.config.continuous_interval));
        }
        let (index, tick) = self.round.as_mut()?;
        while let Some(function) = self.handlers.get(*index) {
            *index += 1;
            if let Some(function) = function {
                return Some((*function, *tick));
            }
        }
        self.round = None;
        self.handlers.retain(Option::is_some);
        if self.handlers.is_empty() {
            self.next_tick = None;
        }
        None
    }
    pub fn finished(&mut self, function: Value, failed: bool) {
        self.continuous_busy = false;
        if failed {
            self.abort_continuous(function);
        }
    }
    pub fn abort_continuous(&mut self, function: Value) {
        self.round = None;
        self.remove(function);
    }
    pub fn sleep_duration(&self) -> Option<Duration> {
        if self.event_disabled {
            return None;
        }
        if !self.activations.is_empty() || (self.round.is_some() && !self.continuous_busy) {
            return Some(Duration::ZERO);
        }
        if self.continuous_busy {
            return None;
        }
        self.next_tick.map(|at| at.saturating_sub(self.clock.now()))
    }
    pub fn reset(&mut self) {
        self.exit = None;
        self.handlers.clear();
        self.round = None;
        self.next_tick = None;
        self.continuous_busy = false;
        self.activations.clear();
        self.event_disabled = false;
    }
}

pub(crate) fn install(
    heap: &mut Heap,
    clock: Rc<dyn Clock>,
    operations: operations::Shared,
    config: SystemConfig,
    limit: usize,
) -> NativeResult<Shared> {
    if config.continuous_interval.is_zero() {
        return Err(NativeError::Message("continuous interval must be positive"));
    }
    let shared = Rc::new(RefCell::new(System {
        input: None,
        clock,
        operations,
        config,
        event_disabled: false,
        exit: None,
        exit_on_window_close: true,
        exit_on_no_window_startup: true,
        class: None,
        active: true,
        activations: VecDeque::new(),
        limit,
        handlers: Vec::new(),
        round: None,
        next_tick: None,
        continuous_busy: false,
        breathing: false,
    }));
    let class = bindings::install(heap, shared.clone())?;
    shared.borrow_mut().class = Some(class);
    for key in ["onActivate", "onDeactivate", "exceptionHandler"] {
        let key = heap.intern(&name::units(key));
        heap.set_member(class, key, Value::Obj(Default::default()))?;
    }
    Ok(shared)
}
