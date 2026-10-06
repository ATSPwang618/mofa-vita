//! Temporary main-thread stall tracing for the transition/video reproducer.
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Stage {
    Idle,
    Pump,
    Input,
    Render,
    Swap,
    Maintain,
    Wait,
    JoinVm,
    CaptureScene,
    Graphics,
    UploadYuv,
    CopyYuv,
    Copy,
    Transform,
    SceneRender,
    ReleaseSnapshot,
    VmBoot,
    VmPoll,
    VmCollect,
    VmWait,
    VideoOpen,
    DecodeNext,
    GetVideoData,
    VideoCopy,
    GetAudioData,
    Playback,
    VideoStop,
    VideoClose,
    HitPlane,
}
const NAMES: [&str; 29] = [
    "idle",
    "pump",
    "input",
    "render",
    "eglSwapBuffers",
    "maintain",
    "wait",
    "join-vm",
    "capture-scene",
    "graphics",
    "UploadYuv",
    "CopyYuv",
    "Copy",
    "Transform",
    "scene-render",
    "release-snapshot",
    "vm-boot",
    "vm-poll",
    "vm-collect",
    "vm-wait",
    "video-open",
    "decode-next",
    "sceAvPlayerGetVideoData",
    "video-copy",
    "sceAvPlayerGetAudioData",
    "playback-state",
    "sceAvPlayerStop",
    "sceAvPlayerClose",
    "ReadHitPlane",
];
#[derive(Default)]
struct Timing {
    micros: AtomicU32,
    calls: AtomicU32,
    max_micros: AtomicU32,
}
struct State {
    // The low byte is the stage, the upper bytes count every entry and exit.
    // One atomic read gives the observer a consistent progress/stage pair.
    stamp: AtomicU32,
    stop: AtomicBool,
    timings: [Timing; NAMES.len()],
}
impl State {
    fn enter(&self, stage: u8) -> u8 {
        let previous = self.stamp.load(Ordering::Relaxed);
        self.stamp.store(
            (previous.wrapping_add(256) & !255) | u32::from(stage),
            Ordering::Release,
        );
        previous as u8
    }
}
thread_local! {
    static ACTIVE: RefCell<Option<Arc<State>>> = const { RefCell::new(None) };
}
pub struct Scope(Option<(Arc<State>, u8, Stage, Option<Instant>)>);
pub fn scope(stage: Stage) -> Scope {
    Scope(ACTIVE.with(|slot| {
        slot.borrow().as_ref().map(|state| {
            let previous = state.enter(stage as u8);
            let timed = matches!(
                stage,
                Stage::Pump
                    | Stage::Input
                    | Stage::Render
                    | Stage::Swap
                    | Stage::Maintain
                    | Stage::CaptureScene
                    | Stage::Graphics
                    | Stage::UploadYuv
                    | Stage::CopyYuv
                    | Stage::Copy
                    | Stage::Transform
                    | Stage::SceneRender
                    | Stage::ReleaseSnapshot
                    | Stage::GetVideoData
                    | Stage::VideoCopy
                    | Stage::GetAudioData
                    | Stage::HitPlane
            );
            (state.clone(), previous, stage, timed.then(Instant::now))
        })
    }))
}
impl Drop for Scope {
    fn drop(&mut self) {
        if let Some((state, previous, stage, started)) = &self.0 {
            if let Some(started) = started {
                let micros = started.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
                let timing = &state.timings[*stage as usize];
                timing.micros.fetch_add(micros, Ordering::Relaxed);
                timing.calls.fetch_add(1, Ordering::Relaxed);
                timing.max_micros.fetch_max(micros, Ordering::Relaxed);
            }
            state.enter(*previous);
        }
    }
}
pub struct Watchdog {
    state: Arc<State>,
    worker: Option<JoinHandle<()>>,
}
impl Watchdog {
    pub fn start(enabled: bool) -> Result<Option<Self>, String> {
        Self::start_named(enabled, "main")
    }
    pub fn start_named(enabled: bool, thread: &'static str) -> Result<Option<Self>, String> {
        if !enabled {
            return Ok(None);
        }
        let state = Arc::new(State {
            stamp: AtomicU32::new(0),
            stop: AtomicBool::new(false),
            timings: std::array::from_fn(|_| Timing::default()),
        });
        let observed = state.clone();
        let worker = std::thread::Builder::new()
            .name("krkr-stall-watch".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                let mut stamp = observed.stamp.load(Ordering::Acquire);
                let mut since = Instant::now();
                let mut last_report = None::<Instant>;
                let mut heartbeat = Instant::now();
                while !observed.stop.load(Ordering::Acquire) {
                    // A relative sleep; no pthread absolute timeout involved.
                    std::thread::sleep(Duration::from_millis(100));
                    let now = Instant::now();
                    let next = observed.stamp.load(Ordering::Acquire);
                    if next != stamp {
                        stamp = next;
                        since = now;
                        last_report = None;
                    }
                    let elapsed = now.duration_since(since);
                    if stamp as u8 != Stage::Idle as u8
                        && elapsed >= Duration::from_millis(750)
                        && last_report
                            .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(1))
                    {
                        krkr_protocol::diagnostic!(
                            "[VITA][STALL] thread={} stage={} unchanged_ms={} progress={}",
                            thread,
                            NAMES[usize::from(stamp as u8)],
                            elapsed.as_millis(),
                            stamp >> 8
                        );
                        last_report = Some(now);
                    }
                    if now.duration_since(heartbeat) >= Duration::from_secs(5) {
                        krkr_protocol::diagnostic!(
                            "[VITA][WATCH] thread={} stage={} unchanged_ms={} progress={}",
                            thread,
                            NAMES[usize::from(stamp as u8)],
                            elapsed.as_millis(),
                            stamp >> 8
                        );
                        let totals = observed.timings.iter().enumerate().filter_map(|(index, timing)| {
                            let calls = timing.calls.swap(0, Ordering::Relaxed);
                            let micros = timing.micros.swap(0, Ordering::Relaxed);
                            let max = timing.max_micros.swap(0, Ordering::Relaxed);
                            (calls != 0).then(|| format!("{}={:.1}/{}/{:.1}", NAMES[index],
                                f64::from(micros) / 1000., calls, f64::from(max) / 1000.))
                        }).collect::<Vec<_>>().join(" ");
                        if !totals.is_empty() {
                            krkr_protocol::diagnostic!(
                                "[VITA][VIDEO-PERF] thread={} window_ms={} inclusive_ms/count/max_ms {}",
                                thread, now.duration_since(heartbeat).as_millis(), totals
                            );
                        }
                        heartbeat = now;
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        ACTIVE.with(|slot| *slot.borrow_mut() = Some(state.clone()));
        Ok(Some(Self {
            state,
            worker: Some(worker),
        }))
    }
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        ACTIVE.with(|slot| *slot.borrow_mut() = None);
        self.state.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_scopes_restore_stage_and_record_progress() {
        let state = Arc::new(State {
            stamp: AtomicU32::new(0),
            stop: AtomicBool::new(false),
            timings: std::array::from_fn(|_| Timing::default()),
        });
        ACTIVE.with(|slot| *slot.borrow_mut() = Some(state.clone()));
        let outer = scope(Stage::Pump);
        let inner = scope(Stage::UploadYuv);
        assert_eq!(
            state.stamp.load(Ordering::Acquire),
            512 | Stage::UploadYuv as u32
        );
        drop(inner);
        assert_eq!(
            state.stamp.load(Ordering::Acquire),
            768 | Stage::Pump as u32
        );
        drop(outer);
        assert_eq!(state.stamp.load(Ordering::Acquire), 1024);
        assert_eq!(
            state.timings[Stage::Pump as usize]
                .calls
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            state.timings[Stage::UploadYuv as usize]
                .calls
                .load(Ordering::Relaxed),
            1
        );
        ACTIVE.with(|slot| *slot.borrow_mut() = None);
    }
}
