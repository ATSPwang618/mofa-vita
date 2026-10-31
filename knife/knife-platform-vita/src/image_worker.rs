//! One bounded CPU worker. Requests contain locators, never decoded pixels or GPU handles.
//! The host tests exercise this exact queue, including cancellation and teardown.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::thread::JoinHandle;
use std::time::{Instant, Duration};
#[path="resource_state.rs"]
pub mod resource_state;
use resource_state::Phase;

pub enum WorkerPoll<R> {Ready(R),Pending,Missing,Failed(String)}
const WAIT_LIMIT:Duration=Duration::from_secs(10);

const WINDOW_LIMIT: usize = 32;

#[derive(Clone, Copy, Debug, Default)]
pub struct WorkerStats {
    pub threads_started: u64,
    pub completed: u64,
    pub stale: u64,
    pub queue_full: u64,
    pub failed: u64,
    pub pending: usize,
    pub ready: usize,
    pub active: bool,
    pub peak_pending: usize,
    pub peak_ready: usize,
    pub decode_us: u64,
    pub max_decode_us: u64,
}

struct Request<J> {
    key: String,
    context: u64,
    id: u64,
    job: J,
}

struct Ready<R> {
    key: String,
    context: u64,
    id: u64,
    result: R,
}

struct State<J, R> {
    pending: VecDeque<Request<J>>,
    ready: VecDeque<Ready<R>>,
    active: Option<(String, u64, u64)>,
    context: u64,
    window: Vec<String>,
    needed:Option<String>,
    failures:HashMap<String,String>,
    claimed: HashMap<String, u64>,
    next_id: u64,
    stopped: bool,
    stats: WorkerStats,
}

struct Shared<J, R> {
    state: Mutex<State<J, R>>,
    wake: Condvar,
}

impl<J, R> Shared<J, R> {
    fn lock(&self) -> MutexGuard<'_, State<J, R>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn try_lock(&self)->Option<MutexGuard<'_,State<J,R>>> {match self.state.try_lock(){
        Ok(s)=>Some(s),Err(TryLockError::Poisoned(e))=>Some(e.into_inner()),Err(_)=>None}}
}

/// One CPU-only decoder with a bounded locator queue and bounded completed results.
/// The caller must also enforce an encoded/decoded byte cap for each admitted job.
/// Completed keys stay claimed until they leave the lookahead window, so eviction
/// under memory pressure cannot cause the same speculative decode every frame.
pub struct BoundedWorker<J, R> {
    shared: Arc<Shared<J, R>>,
    pending_limit: usize,
    thread: Option<JoinHandle<()>>,
}

impl<J: Send + 'static, R: Send + 'static> BoundedWorker<J, R> {
    pub fn new(
        pending_limit: usize,
        ready_limit: usize,
        mut decode: impl FnMut(J) -> R + Send + 'static,
    ) -> std::io::Result<Self> {
        if pending_limit == 0 || ready_limit == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "zero image worker capacity"));
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pending: VecDeque::new(), ready: VecDeque::new(), active: None,
                context: 0, window: Vec::new(), needed:None, failures:HashMap::new(), claimed: HashMap::new(), next_id: 0,
                stopped: false, stats: WorkerStats::default(),
            }),
            wake: Condvar::new(),
        });
        let run = shared.clone();
        let thread = std::thread::Builder::new()
            .name("knife-image".into()).stack_size(512 * 1024).spawn(move || {
                run.lock().stats.threads_started = 1;
                loop {
                    let mut state = run.lock();
                    while !state.stopped && (state.pending.is_empty() || state.ready.len() >= ready_limit) {
                        state = run.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                    }
                    if state.stopped { break; }
                    let request = state.pending.pop_front().unwrap();
                    state.active = Some((request.key.clone(), request.context, request.id));
                    drop(state);
                    resource_state::image(Phase::Decode,&request.key,0,usize::MAX,request.context);
                    let start = Instant::now();
                    // Wake demand waiters even if a decoder panics in an unwind build.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode(request.job)));
                    let elapsed = start.elapsed().as_micros().min(u64::MAX as u128) as u64;
                    let mut state = run.lock();
                    state.active = None;
                    state.stats.decode_us = state.stats.decode_us.saturating_add(elapsed);
                    state.stats.max_decode_us = state.stats.max_decode_us.max(elapsed);
                    match result {
                        Ok(result) => {
                            state.stats.completed += 1;
                            if !state.stopped && state.context == request.context && state.claimed.get(&request.key) == Some(&request.id) {
                                state.ready.push_back(Ready { key: request.key, context: request.context, id: request.id, result });
                                state.stats.peak_ready = state.stats.peak_ready.max(state.ready.len());
                            } else {
                                state.stats.stale += 1;
                            }
                        }
                        Err(_) => {
                            state.stats.failed += 1;
                            if state.context==request.context {state.failures.insert(request.key,"image decoder panicked".into());}
                        }
                    }
                    resource_state::image(Phase::Idle,"",0,usize::MAX,request.context);
                    run.wake.notify_all();
                }
                resource_state::image(Phase::Stopped,"",0,usize::MAX,0);
            })?;
        Ok(Self { shared, pending_limit, thread: Some(thread) })
    }

    /// Change language/context or the small ordered working window. Dropped
    /// in-flight work is allowed to finish, but its stale pixels are discarded.
    pub fn retain_window(&self, context: u64, keys: &[String])->bool {
        let Some(mut state)=self.shared.try_lock()else{return false};
        let mut window = Vec::with_capacity(keys.len().min(WINDOW_LIMIT));
        for key in keys {
            if window.len() == WINDOW_LIMIT { break; }
            if !window.contains(key) { window.push(key.clone()); }
        }
        if context == state.context && window == state.window { return true; }
        let changed = context != state.context;
        if changed {state.needed=None;state.failures.clear();}
        if let Some(key)=state.needed.as_ref() {if !window.contains(key){window.push(key.clone());}}
        let before = state.pending.len() + state.ready.len();
        state.pending.retain(|r| !changed && window.contains(&r.key));
        state.ready.retain(|r| !changed && window.contains(&r.key));
        state.stats.stale += (before - state.pending.len() - state.ready.len()) as u64;
        state.claimed.retain(|key, _| !changed && window.contains(key));
        state.failures.retain(|key,_|!changed && window.contains(key));
        // Prioritize imminent animation frames over more distant script images.
        let needed=state.needed.clone();
        state.pending.make_contiguous().sort_by_key(|r| (u8::from(needed.as_ref()!=Some(&r.key)),
            window.iter().position(|k| k == &r.key).unwrap_or(WINDOW_LIMIT)));
        state.context = context;
        state.window = window;
        self.shared.wake.notify_all();true
    }

    /// Non-blocking admission. A full queue leaves the key eligible for retry.
    pub fn request(&self, key: String, job: J) -> bool {
        let Some(mut state)=self.shared.try_lock()else{return false};
        if state.stopped || self.thread.as_ref().is_none_or(JoinHandle::is_finished) || !state.window.contains(&key) { return false; }
        if state.claimed.contains_key(&key) { return true; }
        if state.pending.len() >= self.pending_limit {
            state.stats.queue_full += 1;
            return false;
        }
        state.next_id = state.next_id.wrapping_add(1);
        let id = state.next_id;
        state.claimed.insert(key.clone(), id);
        let context = state.context;
        state.pending.push_back(Request { key, context, id, job });
        state.stats.peak_pending = state.stats.peak_pending.max(state.pending.len());
        self.shared.wake.notify_all();
        true
    }

    pub fn try_take(&self) -> Option<(String, R)> {
        let mut state = self.shared.try_lock()?;
        let index=state.ready.iter().position(|r|state.needed.as_ref()!=Some(&r.key))?;
        let ready = state.ready.remove(index)?;
        self.shared.wake.notify_all();
        Some((ready.key, ready.result))
    }

    /// Demand is pinned to this generation independently of lookahead changes.
    /// It promotes queued work and reuses an exact active decode. A full ready
    /// queue may discard only speculative results so demand cannot starve there.
    pub fn request_needed(&self,context:u64,key:String,job:J)->bool {
        let Some(mut state)=self.shared.try_lock()else{return false};
        if state.stopped || self.thread.as_ref().is_none_or(JoinHandle::is_finished) {return false;}
        if state.context!=context {
            state.context=context;state.pending.clear();state.ready.clear();state.claimed.clear();
            state.window.clear();state.failures.clear();state.needed=None;
        }
        state.needed=Some(key.clone());
        if state.failures.contains_key(&key) {return true;}
        if state.ready.iter().any(|r|r.key==key && r.context==context) {return true;}
        if state.active.as_ref().is_some_and(|(k,c,id)|*k==key && *c==context && state.claimed.get(k)==Some(id)) {return true;}
        if let Some(i)=state.pending.iter().position(|r|r.key==key && r.context==context) {
            let request=state.pending.remove(i).unwrap();state.pending.push_front(request);
        }else{
            if state.pending.len()>=self.pending_limit {
                if let Some(old)=state.pending.pop_back(){state.claimed.remove(&old.key);}
            }
            state.next_id=state.next_id.wrapping_add(1);let id=state.next_id;
            state.claimed.insert(key.clone(),id);
            state.pending.push_front(Request {key:key.clone(),context,id,job});
        }
        // At most one real demand is outstanding. Everything else here is an
        // optional prefetch result, whose payload may be safely released.
        state.ready.retain(|r|r.key==key);self.shared.wake.notify_all();true
    }
    pub fn poll_needed(&self,context:u64,key:&str)->WorkerPoll<R> {
        let Some(mut state)=self.shared.try_lock()else{return WorkerPoll::Pending};
        if state.context!=context {return WorkerPoll::Missing;}
        if let Some(i)=state.ready.iter().position(|r|r.key==key && r.context==context && state.claimed.get(key)==Some(&r.id)) {
            let ready=state.ready.remove(i).unwrap();self.shared.wake.notify_all();return WorkerPoll::Ready(ready.result);
        }
        if let Some(error)=state.failures.get(key) {return WorkerPoll::Failed(error.clone());}
        if state.stopped || self.thread.as_ref().is_none_or(JoinHandle::is_finished) {return WorkerPoll::Failed("image worker unavailable".into());}
        if state.pending.iter().any(|r|r.key==key) || state.active.as_ref().is_some_and(|(k,c,_)|k==key && *c==context) {WorkerPoll::Pending}
        else{WorkerPoll::Missing}
    }
    pub fn release_needed(&self,key:&str) {
        if let Some(mut state)=self.shared.try_lock() {
            if state.needed.as_deref()==Some(key){state.needed=None;}
            state.pending.retain(|r|r.key!=key);state.claimed.remove(key);
            state.failures.remove(key);self.shared.wake.notify_all();
        }
    }
    /// Legacy synchronous callers retain their API purpose but cannot wait
    /// forever or replay an active request after a deadline failure.
    pub fn take_for(&self,key:&str)->Result<Option<R>,String> {
        self.take_for_timeout(key,WAIT_LIMIT)
    }
    fn take_for_timeout(&self,key:&str,limit:Duration)->Result<Option<R>,String> {
        let start=Instant::now();
        loop {
            if let Some(mut state)=self.shared.try_lock() {
                if let Some(i)=state.ready.iter().position(|r|r.context==state.context && r.key==key && state.claimed.get(key)==Some(&r.id)) {
                    let ready=state.ready.remove(i).unwrap();self.shared.wake.notify_all();return Ok(Some(ready.result));
                }
                if let Some(error)=state.failures.get(key){return Err(error.clone());}
                if state.stopped || self.thread.as_ref().is_none_or(JoinHandle::is_finished){return Err("image worker unavailable".into());}
                let active=state.active.as_ref().is_some_and(|(k,c,id)|k==key && *c==state.context && state.claimed.get(key)==Some(id));
                if !active {
                    state.pending.retain(|r|r.key!=key);
                    if state.window.iter().any(|k|k==key) && !state.claimed.contains_key(key) {
                        state.next_id=state.next_id.wrapping_add(1);let id=state.next_id;state.claimed.insert(key.to_owned(),id);
                    }
                    return Ok(None);
                }
            }
            if start.elapsed()>=limit {return Err(format!("image wait deadline for {key}; active decode retained"));}
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    pub fn stats(&self) -> WorkerStats {
        let state = self.shared.lock();
        WorkerStats { pending: state.pending.len(), ready: state.ready.len(), active: state.active.is_some(), ..state.stats }
    }

    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|thread|!thread.is_finished())
            && self.shared.try_lock().is_none_or(|state|!state.stopped)
    }
}

impl<J, R> Drop for BoundedWorker<J, R> {
    fn drop(&mut self) {
        {
            let mut state = self.shared.lock();
            state.stopped = true;
            state.pending.clear();
            state.ready.clear();
            self.shared.wake.notify_all();
        }
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn until(mut f: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !f() {
            assert!(std::time::Instant::now() < deadline, "worker did not make progress");
            std::thread::yield_now();
        }
    }

    #[test]
    fn one_worker_bounded_requests_and_results_no_duplicate_work() {
        let (entered, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let worker = BoundedWorker::new(2, 1, move |job: u32| {
            entered.send((job, std::thread::current().id())).unwrap();
            gate.recv().unwrap();
            job * 2
        }).unwrap();
        until(||worker.retain_window(1, &["a".into(), "b".into(), "c".into(), "d".into()]));
        until(||worker.request("a".into(), 1));
        let (_, thread) = seen.recv_timeout(Duration::from_secs(3)).unwrap();
        until(||worker.request("a".into(), 99)); // active is deduplicated
        until(||worker.request("b".into(), 2));
        until(||worker.request("c".into(), 3));
        assert!(!worker.request("d".into(), 4)); // no blocking or unbounded growth
        release.send(()).unwrap();
        until(|| worker.stats().ready == 1);
        assert!(seen.try_recv().is_err()); // ready limit stops further decode
        assert_eq!(worker.try_take().unwrap(), ("a".into(), 2));
        let (job, next_thread) = seen.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!((job, next_thread), (2, thread));
        release.send(()).unwrap();
        until(|| worker.stats().ready == 1);
        assert_eq!(worker.try_take().unwrap(), ("b".into(), 4));
        assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap().0, 3);
        release.send(()).unwrap();
        until(|| worker.stats().ready == 1);
        assert_eq!(worker.try_take().unwrap(), ("c".into(), 6));
        let stats = worker.stats();
        assert_eq!(stats.threads_started, 1);
        assert_eq!(stats.completed, 3);
        assert!(stats.peak_pending <= 2 && stats.peak_ready <= 1);
    }

    #[test]
    fn changed_window_drops_queued_ready_and_inflight_stale_results() {
        let (entered, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let worker = BoundedWorker::new(2, 1, move |job: u32| {
            entered.send(job).unwrap();
            gate.recv().unwrap();
            job
        }).unwrap();
        until(||worker.retain_window(1, &["a".into(), "b".into()]));
        until(||worker.request("a".into(), 1));
        assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(), 1);
        until(||worker.request("b".into(), 2));
        until(||worker.retain_window(1, &["c".into()]));
        until(||worker.request("c".into(), 3));
        release.send(()).unwrap();
        assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(), 3);
        release.send(()).unwrap();
        until(|| worker.stats().ready == 1);
        until(||worker.retain_window(1, &[]));
        assert!(worker.try_take().is_none());
        assert_eq!(worker.stats().stale, 3);
    }

    #[test]
    fn language_change_rejects_same_key_old_pixels_and_allows_retry() {
        let (entered, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let worker = BoundedWorker::new(2, 1, move |job: u32| {
            entered.send(job).unwrap();
            gate.recv().unwrap();
            job
        }).unwrap();
        until(||worker.retain_window(1, &["strip".into()]));
        until(||worker.request("strip".into(), 1));
        assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(), 1);
        until(||worker.retain_window(2, &["strip".into()]));
        until(||worker.request("strip".into(), 2));
        release.send(()).unwrap();
        assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(), 2);
        release.send(()).unwrap();
        until(|| worker.stats().ready == 1);
        assert_eq!(worker.try_take().unwrap(), ("strip".into(), 2));
    }

    #[test]
    fn demand_joins_active_decode_instead_of_starting_it_again() {
        let (entered, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let worker = BoundedWorker::new(2, 1, move |job: u32| {
            entered.send(job).unwrap();
            gate.recv().unwrap();
            job
        }).unwrap();
        until(||worker.retain_window(1, &["a".into(), "b".into()]));
        until(||worker.request("a".into(), 7));
        assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(), 7);
        until(||worker.request("b".into(), 8));
        // A queued request can be claimed by the synchronous loader without decoding twice.
        assert!(worker.take_for("b").unwrap().is_none());
        release.send(()).unwrap();
        assert_eq!(worker.take_for("a").unwrap(), Some(7));
        assert!(seen.try_recv().is_err());
    }

    #[test]
    fn demand_poll_and_deadline_leave_the_active_decode_owned_once() {
        let (entered,seen)=mpsc::channel();let (release,gate)=mpsc::channel();
        let worker=BoundedWorker::new(2,1,move |job:u32|{entered.send(job).unwrap();gate.recv().unwrap();job}).unwrap();
        until(||worker.retain_window(1,&["a".into()]));until(||worker.request("a".into(),7));
        assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(),7);
        let started=Instant::now();let pending=matches!(worker.poll_needed(1,"a"),WorkerPoll::Pending);
        let error=worker.take_for_timeout("a",Duration::from_millis(10)).unwrap_err();
        let active=worker.stats().active;release.send(()).unwrap();
        let mut value=None;until(||{if let WorkerPoll::Ready(v)=worker.poll_needed(1,"a"){value=Some(v);true}else{false}});
        assert!(pending);assert!(started.elapsed()<Duration::from_secs(1));assert!(error.contains("deadline"));
        assert!(active);assert_eq!(value,Some(7));assert!(seen.try_recv().is_err());
    }

    #[test]
    fn real_demand_bypasses_full_speculative_ready_queue_and_pending_work() {
        let (sent,seen)=mpsc::channel();let worker=BoundedWorker::new(3,1,move |job:u32|{sent.send(job).unwrap();job}).unwrap();
        until(||worker.retain_window(1,&["old".into(),"later".into()]));until(||worker.request("old".into(),1));
        assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(),1);until(||worker.stats().ready==1);
        until(||worker.request("later".into(),2));until(||worker.request_needed(1,"needed".into(),3));
        assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(),3);
        let mut value=None;until(||{if let WorkerPoll::Ready(v)=worker.poll_needed(1,"needed"){value=Some(v);true}else{false}});
        assert_eq!(value,Some(3));worker.release_needed("needed");
        assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(),2);
    }

    #[test]
    fn new_demand_generation_cannot_consume_an_old_active_result() {
        let (entered,seen)=mpsc::channel();let (release,gate)=mpsc::channel();
        let worker=BoundedWorker::new(2,1,move |job:u32|{entered.send(job).unwrap();gate.recv().unwrap();job}).unwrap();
        until(||worker.request_needed(1,"same".into(),1));assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(),1);
        until(||worker.request_needed(2,"same".into(),2));release.send(()).unwrap();
        assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(),2);
        let old_rejected=!matches!(worker.poll_needed(2,"same"),WorkerPoll::Ready(_));release.send(()).unwrap();
        let mut value=None;until(||{if let WorkerPoll::Ready(v)=worker.poll_needed(2,"same"){value=Some(v);true}else{false}});
        assert!(old_rejected);assert_eq!(value,Some(2));assert_eq!(worker.stats().stale,1);
    }

    #[test]
    fn lookahead_reordering_keeps_real_demand_first() {
        let (entered,seen)=mpsc::channel();let (release,gate)=mpsc::channel();
        let worker=BoundedWorker::new(3,1,move |job:u32|{entered.send(job).unwrap();gate.recv().unwrap();job}).unwrap();
        until(||worker.retain_window(1,&["active".into(),"later".into()]));
        until(||worker.request("active".into(),1));assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(),1);
        until(||worker.request("later".into(),2));until(||worker.request_needed(1,"needed".into(),3));
        until(||worker.retain_window(1,&["later".into(),"needed".into()]));release.send(()).unwrap();
        // The old active result is outside the new window and becomes stale.
        let next=seen.recv_timeout(Duration::from_secs(3)).unwrap();release.send(()).unwrap();
        let mut value=None;until(||{if let WorkerPoll::Ready(v)=worker.poll_needed(1,"needed"){value=Some(v);true}else{false}});
        assert_eq!(next,3);assert_eq!(value,Some(3));worker.release_needed("needed");
        assert_eq!(seen.recv_timeout(Duration::from_secs(3)).unwrap(),2);release.send(()).unwrap();
    }

    #[test]
    fn shutdown_joins_worker_and_drops_retained_results() {
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        struct Owner(Arc<AtomicUsize>);
        impl Drop for Owner { fn drop(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); } }
        let drops = Arc::new(AtomicUsize::new(0));
        let counted = drops.clone();
        let worker = BoundedWorker::new(2, 1, move |_: ()| Owner(counted.clone())).unwrap();
        until(||worker.retain_window(1, &["a".into()]));
        until(||worker.request("a".into(), ()));
        until(|| worker.stats().ready == 1);
        drop(worker);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
