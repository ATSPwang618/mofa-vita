//! Bounded console transport. The render thread only transfers an owned batch;
//! neither printf nor network/plugin latency is allowed under the queue lock.
use std::{collections::VecDeque, io, sync::{Arc, Condvar, Mutex, atomic::{AtomicU64, Ordering}}, thread::JoinHandle, time::Instant};

const MAX_BATCH_BYTES: usize = 96 * 1024;
const MAX_QUEUED_BYTES: usize = 192 * 1024;
const MAX_BATCHES: usize = 3;
#[derive(Default)]
struct Pending { batches: VecDeque<Vec<String>>, bytes: usize, stopped: bool }
#[derive(Default)]
struct Shared {
    pending: Mutex<Pending>, wake: Condvar,
    last_us: AtomicU64, total_us: AtomicU64, completed: AtomicU64,
    dropped: AtomicU64, failures: AtomicU64,
}
#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub last_us: u64, pub total_us: u64, pub completed: u64,
    pub dropped: u64, pub failures: u64, pub queued_bytes: usize,
}
pub struct ConsoleQueue { shared: Arc<Shared>, worker: Option<JoinHandle<()>> }
impl ConsoleQueue {
    pub fn new(write: fn(&[String]) -> io::Result<()>) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let run = shared.clone();
        let worker = std::thread::Builder::new().name("knife-console".into()).stack_size(256 * 1024).spawn(move || {
            loop {
                let mut state = run.pending.lock().unwrap_or_else(|e| e.into_inner());
                while !state.stopped && state.batches.is_empty() {
                    state = run.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                }
                if state.stopped && state.batches.is_empty() { break; }
                let batch = state.batches.pop_front().expect("nonempty console queue");
                state.bytes -= batch.iter().map(String::len).sum::<usize>();
                drop(state);
                let started = Instant::now();
                if write(&batch).is_err() { run.failures.fetch_add(1, Ordering::Relaxed); }
                let elapsed = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
                run.last_us.store(elapsed, Ordering::Relaxed);
                run.total_us.fetch_add(elapsed, Ordering::Relaxed);
                run.completed.fetch_add(1, Ordering::Relaxed);
            }
        })?;
        Ok(Self { shared, worker: Some(worker) })
    }
    pub fn submit(&self, batch: Vec<String>) -> bool {
        let bytes = batch.iter().map(String::len).sum::<usize>();
        if batch.is_empty() { return true; }
        let mut state = self.shared.pending.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopped || bytes > MAX_BATCH_BYTES || state.batches.len() >= MAX_BATCHES
            || bytes > MAX_QUEUED_BYTES.saturating_sub(state.bytes) {
            // Drop the whole observation, including its end marker. Readers
            // detect a gap rather than combining half of two audit snapshots.
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        state.bytes += bytes;
        state.batches.push_back(batch);
        self.shared.wake.notify_one();
        true
    }
    pub fn stats(&self) -> Stats {
        let state = self.shared.pending.lock().unwrap_or_else(|e| e.into_inner());
        Stats { last_us: self.shared.last_us.load(Ordering::Relaxed),
            total_us: self.shared.total_us.load(Ordering::Relaxed),
            completed: self.shared.completed.load(Ordering::Relaxed),
            dropped: self.shared.dropped.load(Ordering::Relaxed),
            failures: self.shared.failures.load(Ordering::Relaxed), queued_bytes: state.bytes }
    }
}
impl Drop for ConsoleQueue {
    fn drop(&mut self) {
        self.shared.pending.lock().unwrap_or_else(|e| e.into_inner()).stopped = true;
        self.shared.wake.notify_one();
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static RECEIVED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn writer(batch: &[String]) -> io::Result<()> {
        assert_ne!(std::thread::current().name(), Some("console_batch_is_ordered_and_bounded"));
        RECEIVED.lock().unwrap().extend_from_slice(batch); Ok(())
    }
    #[test]
    fn console_batch_is_ordered_and_bounded() {
        let queue = ConsoleQueue::new(writer).unwrap();
        assert!(queue.submit(vec!["first".into(), "end".into()]));
        assert!(!queue.submit(vec!["x".repeat(MAX_BATCH_BYTES + 1)]));
        assert_eq!(queue.stats().dropped, 1);
        drop(queue); // drains accepted batches before shutdown
        assert_eq!(&*RECEIVED.lock().unwrap(), &["first", "end"]);
    }
}
