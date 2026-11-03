//! A single, demand-driven atlas worker. Cache and borrowed pins share one byte charge.
//! Unlike per-image range inflation, one active key reads one complete NX output.
use std::{collections::VecDeque, sync::{Arc, Mutex, Condvar, TryLockError}, thread::JoinHandle, time::{Duration, Instant}};
use knife_platform_vita::image_worker::resource_state::{self, Phase};
use anyhow::{Context, Result, ensure};
use knife_core::format::archive::Archive;
use knife_render::ByteView;
use knife_perf::{Gauge, Metric, Scope};

pub const CAP: usize = knife_perf::memory_budget::CPU_SOURCE_BYTES;
const PENDING: usize = 4;
const MAX_ENTRIES: usize = 512;
// A soft preference inside CAP, never another allocation or a borrowed pin.
// MRU language pages win when both languages would exceed this allowance.
const UI_PREFERRED_BYTES: usize = 24 * 1024 * 1024;
type Key = (u64, usize);
struct Entry { key: Key, bytes: ByteView, ui_hot: bool }
struct Job { key: Key, archive: Arc<Archive>, index: usize, generation:u64, demand:bool, no_evict:bool }
pub enum Poll { Ready(ByteView), Pending, Failed(String) }
const WAIT_LIMIT:Duration=Duration::from_secs(10);
#[derive(Default)]
struct State {
    entries: VecDeque<Entry>, jobs: VecDeque<Job>, active: Option<Key>,
    failed: VecDeque<(Key,String)>, next_generation:u64, bytes: usize, reserved: usize, stopped: bool,
}
#[derive(Default)]
struct Shared { state: Mutex<State>, wake: Condvar }
pub struct AtlasCache { shared: Arc<Shared>, thread: Option<JoinHandle<()>> }
impl Default for AtlasCache {
    fn default() -> Self {
        let shared = Arc::new(Shared::default());
        let run = shared.clone();
        let thread = std::thread::Builder::new().name("knife-atlas".into()).stack_size(256 * 1024).spawn(move || {
            loop {
                let mut state = run.state.lock().unwrap_or_else(|e| e.into_inner());
                while !state.stopped && state.jobs.is_empty() {
                    state = run.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                }
                if state.stopped { break; }
                let job = state.jobs.pop_front().unwrap();
                state.active = Some(job.key.clone());
                drop(state);
                resource_state::source(Phase::Plan,job.key.0,job.index,job.generation);
                knife_perf::add(Metric::StageStart, 0, 0);
                knife_perf::gauge_set(Gauge::StageInflight, 1);
                let load = Scope::new(Metric::AtlasLoad);
                let label = format!("{}:{}:{}", job.archive.name, job.index,
                    job.archive.entries().get(job.index).map_or("?", |entry| entry.name.as_str()));
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Option<ByteView>> {
                    let plan = job.archive.read_plan(job.index)?;
                    ensure!(plan.decoded_bytes <= CAP, "atlas output exceeds shared CPU budget");
                    {
                        let mut state = run.state.lock().unwrap_or_else(|e| e.into_inner());
                        // Reserve this entry, not a fictitious full-cache allocation.
                        if job.no_evict {
                            // Opportunistic UI warming never displaces an owner.
                            // A newer demand/seed may have used the room since queueing.
                            if state.bytes.saturating_add(plan.decoded_bytes) > CAP || state.entries.len() >= MAX_ENTRIES {
                                return Ok(None);
                            }
                        } else { Self::reclaim(&mut state, plan.decoded_bytes); }
                        ensure!(state.bytes.saturating_add(plan.decoded_bytes) <= CAP && state.entries.len() < MAX_ENTRIES,
                            "shared atlas pins leave insufficient output budget");
                        state.reserved = plan.decoded_bytes;
                        Self::gauges(&state);
                        knife_perf::gauge_set(Gauge::StageRequestedBytes, plan.decoded_bytes as u64);
                    }
                    // Re-check the immutable header before the actual body allocation.
                    // Input scratch has its own stored bound; retained output has the
                    // exact decoded reservation, even if compressed input is larger.
                    resource_state::source(Phase::ReadInflate,job.key.0,job.index,job.generation);
                    let bytes = ByteView::owned(job.archive.read_bounded_lengths(job.index,
                        plan.stored_bytes, plan.decoded_bytes)?);
                    ensure!(bytes.len() == plan.decoded_bytes, "atlas output disagrees with planned length");
                    Ok(Some(bytes))
                }));
                resource_state::source(Phase::Commit,job.key.0,job.index,job.generation);
                let mut state = run.state.lock().unwrap_or_else(|e| e.into_inner());
                state.active = None;
                state.reserved = 0;
                match result {
                    Ok(Ok(Some(bytes))) => {
                        if !job.no_evict { Self::reclaim(&mut state, bytes.allocation_len()); }
                        if state.bytes.saturating_add(bytes.allocation_len()) <= CAP && state.entries.len() < MAX_ENTRIES {
                            state.bytes += bytes.allocation_len();
                            knife_perf::add(Metric::StageComplete, bytes.len() as u64, 0);
                            load.finish_hot(&label, bytes.allocation_len() as u64, bytes.len() as u64);
                            state.entries.push_back(Entry { key: job.key, bytes, ui_hot: Self::ui_hot(&job.archive, job.index) });
                        } else if !job.no_evict { Self::fail(&mut state,job.key,"shared source pins leave insufficient output budget".into()); }
                    },
                    Ok(Ok(None)) => {},
                    Ok(Err(error)) => Self::fail(&mut state,job.key,format!("{error:#}")),
                    Err(_) => Self::fail(&mut state,job.key,"source worker panicked".into()),
                }
                knife_perf::gauge_set(Gauge::StageRequestedBytes, 0);
                knife_perf::gauge_set(Gauge::StageInflight, 0);
                Self::gauges(&state);
                resource_state::source(Phase::Idle,job.key.0,job.index,job.generation);
                run.wake.notify_all();
            }
            resource_state::source(Phase::Stopped,0,usize::MAX,0);
        }).ok();
        Self { shared, thread }
    }
}
impl AtlasCache {
    fn ui_hot(archive: &Archive, index: usize) -> bool {
        archive.name.ends_with("allui") && archive.entries().get(index).is_some_and(|entry| {
            let stem = entry.name.split('.').next().unwrap_or(&entry.name);
            ["DISP_PARTS", "TITLE_PARTS", "MODEPNL_PARTS", "BORDER_PARTS", "NOISE_PARTS"]
                .iter().any(|base| stem == *base || stem.strip_prefix(base).is_some_and(|tail| tail.starts_with('_')))
        })
    }
    fn preferred(state: &State) -> ([bool; MAX_ENTRIES], usize, usize) {
        let mut preferred = [false; MAX_ENTRIES];
        let (mut bytes, mut count) = (0usize, 0usize);
        for (i, entry) in state.entries.iter().enumerate().rev() {
            let charge = entry.bytes.allocation_len();
            if entry.ui_hot && charge <= UI_PREFERRED_BYTES.saturating_sub(bytes) {
                preferred[i] = true; bytes += charge; count += 1;
            }
        }
        (preferred, bytes, count)
    }
    fn victim(state: &State, include_preferred: bool) -> Option<usize> {
        let (preferred, _, _) = Self::preferred(state);
        state.entries.iter().enumerate().find_map(|(i, entry)|
            (entry.bytes.is_unpinned() && !preferred[i]).then_some(i))
            .or_else(|| include_preferred.then(|| state.entries.iter().position(|entry| entry.bytes.is_unpinned())).flatten())
    }
    fn gauges(state: &State) {
        knife_perf::gauge_set(Gauge::StageReadyBytes, state.bytes as u64);
        // One owner pool, exposed under its new name as well. Never add these
        // two gauges when calculating the total CPU cache allowance.
        knife_perf::gauge_set(Gauge::SourceCacheBytes, (state.bytes + state.reserved) as u64);
    }
    fn reclaim(state: &mut State, need: usize) {
        while state.bytes.saturating_add(need) > CAP || state.entries.len() >= MAX_ENTRIES {
            let Some(index) = Self::victim(state, true) else { break; };
            let old = state.entries.remove(index).unwrap();
            state.bytes -= old.bytes.allocation_len();
            knife_perf::add(Metric::StageEvict, old.bytes.len() as u64, 0);
            knife_perf::add(Metric::SourceCacheDropped, old.bytes.allocation_len() as u64, 0);
        }
    }
    /// Move the first worker allocation into the same bounded owner pool used
    /// by KATL. A later GPU eviction can borrow it without read/inflate/copy.
    /// In-flight reservations and all live borrowed views remain charged.
    pub fn seed(&self, archive: &Archive, index: usize, bytes: ByteView) -> bool {
        let key = (archive.cache_identity(), index);
        let mut state=match self.shared.state.try_lock() {Ok(s)=>s,Err(TryLockError::Poisoned(e))=>e.into_inner(),Err(_)=>return false};
        if let Some(i) = state.entries.iter().position(|e| e.key == key) {
            let entry=state.entries.remove(i).unwrap();state.entries.push_back(entry);
            state.failed.retain(|(failed,_)|*failed!=key);state.jobs.retain(|job|job.key!=key);return true;
        }
        // A worker already owns this exact reservation. Do not admit a second
        // backing block under the same key while it is reading.
        if state.active == Some(key) { return false; }
        let need = bytes.allocation_len();
        if need > CAP { return false; }
        let reserved = state.reserved;
        Self::reclaim(&mut state, reserved.saturating_add(need));
        if state.bytes.saturating_add(reserved).saturating_add(need) > CAP || state.entries.len() >= MAX_ENTRIES {
            Self::gauges(&state);return false;
        }
        state.bytes += need;
        state.entries.push_back(Entry { key, bytes, ui_hot: Self::ui_hot(archive, index) });
        state.failed.retain(|(failed,_)|*failed!=key);state.jobs.retain(|job|job.key!=key);
        knife_perf::add(Metric::SourceCacheSeed, need as u64, 0);
        Self::gauges(&state);
        self.shared.wake.notify_all();
        true
    }
    fn fail(state:&mut State,key:Key,error:String) {
        state.failed.retain(|(old,_)|*old!=key);
        while state.failed.len()>=MAX_ENTRIES {state.failed.pop_front();}
        state.failed.push_back((key,error));knife_perf::add(Metric::StageFailed,0,0);
    }
    /// An explicit new VM generation can retry a completed failure. An active
    /// read/reservation is never canceled, duplicated or freed by this operation.
    pub fn retry_failed(&self,archive:&Archive,index:usize)->bool {
        let mut state=match self.shared.state.try_lock() {Ok(s)=>s,Err(TryLockError::Poisoned(e))=>e.into_inner(),Err(_)=>return false};
        let key=(archive.cache_identity(),index);
        if state.active!=Some(key) {state.failed.retain(|(old,_)|*old!=key);}true
    }
    /// Polling only touches bounded metadata. Caller never waits for a mutex,
    /// file read, inflation or another request. A demand promotes its locator
    /// ahead of speculative work, but cannot interrupt an already active read.
    pub fn poll(&self,archive:Arc<Archive>,index:usize,demand:bool)->Poll {
        if archive.entries().get(index).is_none() {return Poll::Failed("missing archive entry".into());}
        let key=(archive.cache_identity(),index);
        let mut state=match self.shared.state.try_lock() {Ok(s)=>s,Err(TryLockError::Poisoned(e))=>e.into_inner(),Err(_)=>return Poll::Pending};
        if let Some(i)=state.entries.iter().position(|e|e.key==key) {
            let entry=state.entries.remove(i).unwrap();let bytes=entry.bytes.clone();state.entries.push_back(entry);
            knife_perf::add(Metric::SourceCacheHit,bytes.len()as u64,0);return Poll::Ready(bytes);
        }
        if let Some((_,error))=state.failed.iter().find(|(failed,_)|*failed==key) {return Poll::Failed(error.clone());}
        if state.stopped || self.thread.as_ref().is_none_or(JoinHandle::is_finished) {return Poll::Failed("source worker unavailable".into());}
        if state.active==Some(key) {return Poll::Pending;}
        if let Some(i)=state.jobs.iter().position(|j|j.key==key) {
            if demand {let mut job=state.jobs.remove(i).unwrap();job.demand=true;job.no_evict=false;state.jobs.push_front(job);}
            return Poll::Pending;
        }
        if state.jobs.len()>=PENDING {
            if demand {if let Some(i)=state.jobs.iter().rposition(|j|!j.demand) {state.jobs.remove(i);}else{return Poll::Pending;}}
            else{return Poll::Pending;}
        }
        state.next_generation=state.next_generation.wrapping_add(1).max(1);
        let job=Job {key,archive,index,generation:state.next_generation,demand,no_evict:false};
        if demand {state.jobs.push_front(job);}else{state.jobs.push_back(job);}
        knife_perf::add(Metric::StageRequest,0,0);knife_perf::add(Metric::StageMiss,0,0);
        knife_perf::add(Metric::SourceCacheMiss,0,0);
        if !demand {knife_perf::add(Metric::SourceCachePrefetch,0,0);}
        self.shared.wake.notify_all();Poll::Pending
    }
    pub fn request(&self,archive:Arc<Archive>,index:usize)->bool {
        !matches!(self.poll(archive,index,false),Poll::Failed(_))
    }
    /// Queue at most one idle, metadata-sized warmup. No read/wait on the caller,
    /// no eviction at admission/commit, and demand can promote the queued job.
    /// Returns only the output bytes newly queued (zero for resident/busy).
    pub fn prewarm_if_room(&self, archive: Arc<Archive>, index: usize, output: usize) -> usize {
        if output == 0 || output > CAP || archive.entries().get(index).is_none() { return 0; }
        let key = (archive.cache_identity(), index);
        let mut state=match self.shared.state.try_lock() {Ok(s)=>s,Err(TryLockError::Poisoned(e))=>e.into_inner(),Err(_)=>return 0};
        if state.stopped || self.thread.as_ref().is_none_or(JoinHandle::is_finished)
            || state.entries.iter().any(|entry|entry.key == key) || state.failed.iter().any(|(failed,_)|*failed == key)
            || state.active.is_some() || !state.jobs.is_empty() || state.entries.len() >= MAX_ENTRIES
            || state.bytes.saturating_add(state.reserved).saturating_add(output) > CAP { return 0; }
        state.next_generation=state.next_generation.wrapping_add(1).max(1);
        let generation=state.next_generation;
        state.jobs.push_back(Job {key,archive,index,generation,demand:false,no_evict:true});
        knife_perf::add(Metric::StageRequest,0,0);knife_perf::add(Metric::StageMiss,0,0);
        knife_perf::add(Metric::SourceCacheMiss,0,0);knife_perf::add(Metric::SourceCachePrefetch,output as u64,1);
        self.shared.wake.notify_all();output
    }
    /// Free only the requested whole-owner charge, oldest unpinned first. A
    /// pressure sweep first calls this with false, then other CPU pools, and
    /// only uses true if those cannot meet the real headroom shortfall.
    pub fn trim_lru(&self, need: usize, include_preferred: bool) -> usize {
        if need == 0 { return 0; }
        let mut state=match self.shared.state.try_lock() {Ok(s)=>s,Err(TryLockError::Poisoned(e))=>e.into_inner(),Err(_)=>return 0};
        let before = state.bytes;
        while before - state.bytes < need {
            let Some(i) = Self::victim(&state, include_preferred) else { break; };
            let old = state.entries.remove(i).unwrap(); state.bytes -= old.bytes.allocation_len();
            knife_perf::add(Metric::SourceCacheDropped, old.bytes.allocation_len() as u64, 1);
        }
        Self::gauges(&state);
        before - state.bytes
    }
    pub fn record_hot_retained(&self) {
        if let Ok(state) = self.shared.state.try_lock() {
            let (_, bytes, count) = Self::preferred(&state);
            knife_perf::add(Metric::CpuSourceHotRetained, bytes as u64, count as u64);
        }
    }
    /// Page teardown drops only cold owners and queued speculation. Never free
    /// an active read's reservation or invalidate another live ByteView.
    pub fn discard_idle(&self, archive: &Archive, index: usize) -> usize {
        let key = (archive.cache_identity(), index);
        let mut state = match self.shared.state.try_lock() {
            Ok(state) => state, Err(TryLockError::Poisoned(e)) => e.into_inner(), Err(_) => return 0,
        };
        state.jobs.retain(|job| job.key != key || job.demand);
        if let Some(i) = state.entries.iter().position(|entry| entry.key == key && entry.bytes.is_unpinned()) {
            let entry = state.entries.remove(i).unwrap();
            let released = entry.bytes.allocation_len();
            state.bytes -= released;
            knife_perf::add(Metric::SourceCacheDropped, released as u64, 2);
            Self::gauges(&state);
            return released;
        }
        0
    }
    pub fn range(&self, archive: Arc<Archive>, index: usize, offset: usize, len: usize) -> Result<ByteView> {
        let view = self.whole(archive, index)?.range(offset, len).context("shared atlas range outside entry")?;
        knife_perf::add(Metric::StageHit, len as u64, view.allocation_len() as u64);
        Ok(view)
    }
    /// Compatibility reload path. STCH uses poll() instead. A hung card read
    /// keeps its worker and owner alive; this waiter reports a bounded error
    /// without replaying the same active read or treating missing pixels as ready.
    pub fn whole(&self, archive: Arc<Archive>, index: usize) -> Result<ByteView> {
        let started=Instant::now();let mut wait=None;
        loop {
            match self.poll(archive.clone(),index,true) {
                Poll::Ready(bytes)=>{
                    if let Some(wait)=wait {let wait:Scope=wait;wait.finish_hot(&format!("{}:{index}:{}",archive.name,
                        archive.entries().get(index).map_or("?",|e|e.name.as_str())),bytes.len()as u64,0);}
                    return Ok(bytes);
                },
                Poll::Failed(error)=>anyhow::bail!("bounded source {}:{index}: {error}",archive.name),
                Poll::Pending=>{},
            }
            if wait.is_none() {wait=Some(Scope::new(Metric::AtlasDemandWait));}
            ensure!(started.elapsed()<WAIT_LIMIT,"source wait deadline (10s) {}:{index}; active worker retained",archive.name);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

}
impl Drop for AtlasCache {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        state.jobs.clear();
        self.shared.wake.notify_all();
        drop(state);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}
