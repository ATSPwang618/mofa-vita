//! Console-only observations from a thread that never touches Game, GXM or cache locks.
//! Atomic fields are best-effort observations, not a transactional game snapshot.
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering::Relaxed};

pub const STARTUP: u32 = 1;
pub const INPUT: u32 = 2;
pub const STORY_INPUT: u32 = 3;
pub const GAME_UPDATE: u32 = 4;
pub const UI_UPDATE: u32 = 5;
pub const AUDIO_FLUSH: u32 = 6;
pub const RENDER: u32 = 7;
pub const CACHE_MAINTENANCE: u32 = 8;
pub const PERF_LOG: u32 = 9;
pub const PACER: u32 = 10;
pub const SCENE_LOAD: u32 = 11;

static ENABLED: AtomicBool = AtomicBool::new(false);
static FRAME: AtomicU32 = AtomicU32::new(0);
static PROGRESS: AtomicU32 = AtomicU32::new(0);
static PHASE: AtomicU32 = AtomicU32::new(STARTUP);
static SCENE: AtomicU32 = AtomicU32::new(0);
static PC: AtomicU32 = AtomicU32::new(0);
static REQUESTED_SCENE: AtomicU32 = AtomicU32::new(u32::MAX);
static COMMAND: AtomicU32 = AtomicU32::new(0);
static COMMAND_SCENE: AtomicU32 = AtomicU32::new(0);
static COMMAND_PC: AtomicU32 = AtomicU32::new(0);
static BUTTONS: AtomicU32 = AtomicU32::new(0);
static PRESSED: AtomicU32 = AtomicU32::new(0);
static PRESS_EDGES: AtomicU32 = AtomicU32::new(0);
static LAST_PRESS_FRAME: AtomicU32 = AtomicU32::new(0);
static INPUT_STATUS: AtomicI32 = AtomicI32::new(0);
static WAIT: AtomicU32 = AtomicU32::new(0);
static FLAGS: AtomicU32 = AtomicU32::new(0);
static PAGE_CHARS: AtomicU32 = AtomicU32::new(0);
static PAGE_REVEALED: AtomicU32 = AtomicU32::new(0);

pub fn phase(value: u32) {
    if ENABLED.load(Relaxed) {
        PHASE.store(value, Relaxed);
        PROGRESS.fetch_add(1, Relaxed);
    }
}
pub struct PhaseGuard(u32);
impl Drop for PhaseGuard {
    fn drop(&mut self) { phase(self.0); }
}
pub fn scene_load(scene: usize) -> PhaseGuard {
    let previous = PHASE.load(Relaxed);
    if ENABLED.load(Relaxed) { REQUESTED_SCENE.store(scene as u32, Relaxed); }
    phase(SCENE_LOAD);
    PhaseGuard(previous)
}
pub fn frame(frame: u64) {
    if ENABLED.load(Relaxed) { FRAME.store(frame as u32, Relaxed); }
}
/// Called immediately after the native poll, before UI handlers can block.
pub fn input(buttons: u32, status: i32) {
    if !ENABLED.load(Relaxed) { return; }
    INPUT_STATUS.store(status, Relaxed);
    if status != 1 { return; }
    let pressed = buttons & !BUTTONS.swap(buttons, Relaxed);
    if pressed != 0 {
        PRESSED.store(pressed, Relaxed);
        PRESS_EDGES.fetch_add(1, Relaxed);
        LAST_PRESS_FRAME.store(FRAME.load(Relaxed), Relaxed);
    }
}
/// Does not advance the heartbeat: an endlessly looping script is still a stall.
pub fn command(scene: usize, pc: usize, name: &str) {
    if !ENABLED.load(Relaxed) { return; }
    COMMAND_SCENE.store(scene as u32, Relaxed);
    COMMAND_PC.store(pc as u32, Relaxed);
    let mut bytes = [0; 4];
    for (out, &byte) in bytes.iter_mut().zip(name.as_bytes()) {
        *out = if byte.is_ascii_alphanumeric() || byte == b'_' { byte } else { b'?' };
    }
    COMMAND.store(u32::from_le_bytes(bytes), Relaxed);
}

#[cfg(target_os = "vita")]
pub fn game(game: &crate::game::Game) {
    if !ENABLED.load(Relaxed) { return; }
    use crate::game::Wait;
    SCENE.store(game.scene_index as u32, Relaxed);
    PC.store(game.current_pc() as u32, Relaxed);
    let wait = match game.wait {
        Wait::None => 0, Wait::Time { .. } => 1, Wait::Click => 2, Wait::Transition => 3,
        Wait::Layer(..) => 4, Wait::Choice => 5, Wait::Voice => 6, Wait::Message { .. } => 7,
        Wait::Movie => 8, Wait::Halt => 9, Wait::TeaTime => 10,
    };
    WAIT.store(wait, Relaxed);
    let (_, chars, waiting, visible, revealed, _) = game.vita_diagnostic_page();
    FLAGS.store(u32::from(game.paused) | u32::from(game.menu_lock) << 1
        | u32::from(game.ended) << 2 | u32::from(waiting) << 3 | u32::from(visible) << 4
        | u32::from(game.vita_diagnostic_resource().0) << 5
        | u32::from(game.vita_diagnostic_resource().1) << 6, Relaxed);
    PAGE_CHARS.store(chars as u32, Relaxed);
    PAGE_REVEALED.store(revealed as u32, Relaxed);
}

fn phase_name(value: u32) -> &'static str {
    match value {
        STARTUP => "startup", INPUT => "input", STORY_INPUT => "story_input",
        GAME_UPDATE => "game_update", UI_UPDATE => "ui_update", AUDIO_FLUSH => "audio_flush",
        RENDER => "render", CACHE_MAINTENANCE => "cache_maintenance", PERF_LOG => "perf_log",
        PACER => "pacer", SCENE_LOAD => "scene_load", _ => "unknown",
    }
}
fn native_name(value: u32) -> &'static str {
    match value {
        0 => "idle", 1 => "gxm_begin_scene", 2 => "gxm_end_scene", 3 => "gxm_finish",
        4 => "gxm_transfer_finish", 5 => "gxm_display_finish", 6 => "gxm_display_enqueue",
        7 => "gxm_notification_wait", _ => "unknown",
    }
}

struct Detector { progress: u32, changed_ms: u64, emitted_ms: u64 }
impl Detector {
    fn new() -> Self { Self { progress: 0, changed_ms: 0, emitted_ms: 0 } }
    fn observe(&mut self, now: u64, progress: u32) -> Option<u64> {
        if progress != self.progress { self.progress = progress; self.changed_ms = now; }
        let idle = now.saturating_sub(self.changed_ms);
        let interval = if idle >= 2000 { 2000 } else { 5000 };
        if now.saturating_sub(self.emitted_ms) < interval { return None; }
        self.emitted_ms = now;
        Some(idle)
    }
}

#[cfg(target_os = "vita")]
pub fn start() -> std::io::Result<()> {
    use std::time::{Duration, Instant};
    unsafe extern "C" { fn kv_perf_native_state(out: *mut u32); }
    ENABLED.store(true, Relaxed);
    let spawned = std::thread::Builder::new().name("runtime-watchdog".into()).stack_size(64 * 1024)
        .spawn(|| {
            let started = Instant::now();
            let mut detector = Detector::new();
            while ENABLED.load(Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                let now = started.elapsed().as_millis() as u64;
                let Some(idle) = detector.observe(now, PROGRESS.load(Relaxed)) else { continue; };
                let mut native = [0u32; 2];
                // Only relaxed atomic loads in C. Never queries or waits for GXM.
                unsafe { kv_perf_native_state(native.as_mut_ptr()); }
                let bytes = COMMAND.load(Relaxed).to_le_bytes();
                let command = std::str::from_utf8(&bytes).unwrap_or("????").trim_end_matches('\0');
                let flags = FLAGS.load(Relaxed);
                let wait = match WAIT.load(Relaxed) {
                    0 => "none", 1 => "time", 2 => "click", 3 => "transition", 4 => "layer",
                    5 => "choice", 6 => "voice", 7 => "message", 8 => "movie", 9 => "halt",
                    10 => "teatime", _ => "unknown",
                };
                let line = format!(concat!("{{\"kind\":\"runtime_state\",\"schema\":1,\"elapsed_ms\":{},",
                    "\"frame\":{},\"phase\":\"{}\",\"no_progress_ms\":{},\"stalled\":{},",
                    "\"scene\":{},\"pc\":{},\"requested_scene\":{},",
                    "\"command_scene\":{},\"command_pc\":{},\"command\":\"{}\",",
                    "\"native_phase\":\"{}\",\"native_detail\":{},",
                    "\"input_status\":{},\"buttons\":{},\"last_pressed\":{},\"press_edges\":{},\"last_press_frame\":{},",
                    "\"wait\":\"{}\",\"paused\":{},\"menu_lock\":{},\"ended\":{},",
                    "\"page_waiting\":{},\"page_visible\":{},\"page_chars\":{},\"page_revealed\":{},",
                    "\"observation\":\"atomic-best-effort\"}}\n"),
                    now, FRAME.load(Relaxed), phase_name(PHASE.load(Relaxed)), idle, idle >= 2000,
                    SCENE.load(Relaxed), PC.load(Relaxed), REQUESTED_SCENE.load(Relaxed),
                    COMMAND_SCENE.load(Relaxed), COMMAND_PC.load(Relaxed), command,
                    native_name(native[0]), native[1], INPUT_STATUS.load(Relaxed), BUTTONS.load(Relaxed),
                    PRESSED.load(Relaxed), PRESS_EDGES.load(Relaxed), LAST_PRESS_FRAME.load(Relaxed), wait,
                    flags & 1 != 0, flags & 2 != 0, flags & 4 != 0, flags & 8 != 0, flags & 16 != 0,
                    PAGE_CHARS.load(Relaxed), PAGE_REVEALED.load(Relaxed));
                let resources = knife_platform_vita::image_worker::resource_state::snapshot();
                let worker_json = |state: knife_platform_vita::image_worker::resource_state::WorkerSnapshot| {
                    format!(concat!("{{\"phase\":\"{}\",\"key\":{},\"archive\":{},\"index\":{},",
                        "\"generation\":{},\"age_ms\":{}}}"), state.phase.name(), state.key,
                        state.archive, state.index, state.generation, state.age_ms)
                };
                let line = line.trim_end().strip_suffix('}').expect("runtime state object").to_owned()
                    + &format!(concat!(",\"resource_wait\":{},\"resource_failed\":{},",
                        "\"resources\":{{\"source\":{},\"image\":{},\"demand\":{}}}}}\n"),
                        flags & 32 != 0, flags & 64 != 0, worker_json(resources.source),
                        worker_json(resources.image), worker_json(resources.demand));
                let _ = knife_platform_vita::audit::console(&line);
            }
        });
    if let Err(error) = spawned { ENABLED.store(false, Relaxed); return Err(error); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocked_main_reports_without_another_main_frame_and_recovers() {
        let mut detector = Detector::new();
        assert_eq!(detector.observe(500, 41), None);
        assert_eq!(detector.observe(2000, 41), None);
        assert_eq!(detector.observe(2500, 41), Some(2000));
        assert_eq!(detector.observe(4500, 41), Some(4000));
        assert_eq!(detector.observe(5000, 42), None);
        for now in (5500..9500).step_by(500) { assert_eq!(detector.observe(now, now as u32), None); }
        assert_eq!(detector.observe(9500, 50), Some(0));
    }
}
