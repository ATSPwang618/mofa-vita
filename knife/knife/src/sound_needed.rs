//! One CPU sound decoder, independent of image demand. Speculation only fills
//! the existing PCM cache; a real script barrier pins at most 16 MiB until play.
use super::*;
use std::{io::Read, time::{Duration, Instant}};
use knife_platform_vita::image_worker::{BoundedWorker, WorkerPoll};

const ENCODED_LIMIT: usize = 8 * knife_perf::memory_budget::MIB;
const PCM_LIMIT: usize = knife_perf::memory_budget::CPU_SOUND_PREPARED_BYTES;
const FRAME_LIMIT: usize = PCM_LIMIT / 8;
const PREPARED_ENTRIES: usize = 24;
const WINDOW: usize = 8;

pub enum SoundNeeded { Ready, Pending }
pub(super) struct Job { key: String, name: String }
pub(super) struct DecodedSound { key: String, result: Result<Arc<Pcm>> }
pub(super) struct Demand { key: String, started: Instant }

fn bounded_stream(path: &Path) -> Result<Pcm> {
    let file = std::fs::File::open(path)?;
    anyhow::ensure!(file.metadata()?.len() <= ENCODED_LIMIT as u64, "sound encoded source exceeds 8 MiB");
    let mut bytes = Vec::new();
    file.take(ENCODED_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= ENCODED_LIMIT, "sound encoded source grew past budget");
    if bytes.starts_with(b"OggS") {
        // Bound the exact packet table the unchanged codec will build. No
        // payload, gain, pre-skip, EOS or sample conversion semantics change.
        let (mut at, mut packets) = (0usize, 0usize);
        while at < bytes.len() {
            let header = bytes.get(at..at + 27).context("truncated sound Ogg page")?;
            anyhow::ensure!(&header[..4] == b"OggS", "invalid sound Ogg page");
            let count = header[26] as usize;
            let lacing = bytes.get(at + 27..at + 27 + count).context("truncated sound Ogg lacing")?;
            packets += lacing.iter().filter(|&&n| n < 255).count();
            anyhow::ensure!(packets <= 65536, "sound packet table exceeds budget");
            at = at.checked_add(27 + count + lacing.iter().map(|&n| n as usize).sum::<usize>())
                .context("sound Ogg page overflow")?;
            anyhow::ensure!(at <= bytes.len(), "truncated sound Ogg payload");
        }
    }
    // An extra frame detects oversize input. It is rejected, never silently
    // truncated into a shorter supposedly successful sound.
    let (_, pcm) = codec::decode_stream_part(&bytes, 0, FRAME_LIMIT + 1)?;
    anyhow::ensure!(pcm.frames.len() <= FRAME_LIMIT
        && pcm.frames.capacity().saturating_mul(8) <= PCM_LIMIT, "sound PCM exceeds 16 MiB");
    Ok(pcm)
}

impl Assets {
    pub(super) fn start_sound_worker(&mut self) {
        let sar = self.sar.clone();
        let streams = self.stream_resolver.fork_for_worker();
        self.sound_worker = match BoundedWorker::new_cpu("knife-sound", 4, 1, move |job: Job| {
            let scope = Scope::new(Metric::SoundEffectDecode);
            let result = sar.load_wave_sound(&format!("wsd_{}", job.key))
                .or_else(|error| if Self::audio_allocation_failed(&error) { Err(error) }
                    else { sar.load_wave_sound(&job.key) })
                .or_else(|error| if Self::audio_allocation_failed(&error) { Err(error) }
                    else {
                        // BFSAR is the primary source. The main-thread job
                        // contains only names; even a custom fallback's stat
                        // and .ogg/.opus choice run here, only after failure.
                        let relative = format!("stream/{}.opus", job.name);
                        let path = streams.resolve(&relative, true)
                            .with_context(|| format!("sound {} absent from archive and stream profile", job.name))?;
                        bounded_stream(&path)
                    })
                .and_then(|pcm| {
                    anyhow::ensure!(pcm.frames.capacity().saturating_mul(8) <= PCM_LIMIT,
                        "sound PCM exceeds existing 16 MiB owner budget");
                    Ok(Arc::new(pcm))
                });
            let bytes = result.as_ref().map_or(0, |pcm| pcm.frames.len() as u64 * 8);
            scope.finish_hot(&job.name, bytes, u64::from(result.is_ok()));
            DecodedSound { key: job.key, result }
        }) {
            Ok(worker) => Some(worker),
            Err(error) => {
                tracing::warn!("sound preparation worker unavailable: {error}; script requests fail without synchronous decoding");
                None
            }
        };
    }
    fn sound_job(&self, name: &str) -> Job {
        Job { key: name.to_ascii_lowercase(), name: name.into() }
    }
    pub fn cached_sound_effect(&mut self, name: &str) -> Option<Option<Arc<Pcm>>> {
        let key = name.to_ascii_lowercase();
        if let Some(pcm) = self.sound_prepared.get(&key) { return Some(pcm.clone()); }
        self.sounds.borrow_mut().get(&key)
    }
    fn prepared_sound_bytes(&self) -> usize {
        self.sound_prepared.values().filter_map(Option::as_ref)
            .map(|pcm| pcm.frames.capacity().saturating_mul(8)).sum()
    }
    fn sound_prepared_gauges(&self) {
        // These are pinned logical owners, often shared with PCM cache/mixer.
        // Do not add this gauge to those pools as exclusive allocated bytes.
        knife_perf::gauge_set(Gauge::SoundPreparedBytes, self.prepared_sound_bytes() as u64);
        knife_perf::gauge_set(Gauge::SoundPreparedEntries, self.sound_prepared.len() as u64);
    }
    fn pin_sound(&mut self, key: String, pcm: Option<Arc<Pcm>>) -> bool {
        if self.sound_prepared.contains_key(&key) { return true; }
        let bytes = pcm.as_ref().map_or(0, |pcm| pcm.frames.capacity().saturating_mul(8));
        if self.sound_prepared.len() >= PREPARED_ENTRIES || self.prepared_sound_bytes().saturating_add(bytes) > PCM_LIMIT {
            tracing::warn!(name=%key, "sound preparation cohort exceeds 16 MiB/24 owners; current effect skipped");
            knife_perf::add(Metric::SoundDemandFailed, bytes as u64, 1);
            // Negative pins carry no PCM payload and preserve the old None
            // failure behavior for the exact command rather than retry forever.
            if self.sound_prepared.len() < PREPARED_ENTRIES { self.sound_prepared.insert(key, None); }
            self.sound_prepared_gauges();
            return false;
        }
        self.sound_prepared.insert(key, pcm);
        self.sound_prepared_gauges();true
    }
    pub fn fail_sound_cohort(&mut self, names: &[String], detail: &str) {
        for name in names {
            let key = name.to_ascii_lowercase();
            if !self.sound_prepared.contains_key(&key) { self.sound_failed(key, detail); }
        }
    }
    pub fn release_prepared_sounds(&mut self) {
        if self.sound_prepared.is_empty() { return; }
        knife_perf::add(Metric::SoundPreparedRelease, self.prepared_sound_bytes() as u64, self.sound_prepared.len() as u64);
        self.sound_prepared.clear();self.sound_prepared_gauges();
    }
    pub fn reset_sound_needed_context(&mut self) {
        if self.sound_demand.take().is_some() { knife_perf::add(Metric::SoundDemandCancel, 0, 1); }
        self.release_prepared_sounds();
        self.sound_context = self.sound_context.wrapping_add(1).max(1);
        if let Some(worker) = &self.sound_worker { worker.retain_window(self.sound_context, &[]); }
    }
    fn sound_failed(&mut self, key: String, detail: impl std::fmt::Display) -> SoundNeeded {
        tracing::warn!(name=%key, "sound preparation failed: {detail}; effect omitted, VM continues");
        knife_perf::add(Metric::SoundDemandFailed, 0, 1);
        if let Some(worker) = &self.sound_worker { worker.release_needed(&key); }
        self.sound_demand = None;
        // Do not permanently negative-cache transient OOM/worker/deadline errors.
        self.pin_sound(key, None);SoundNeeded::Ready
    }
    /// Drain one speculative owner and queue bounded names, with no playback.
    /// No cache eviction is permitted merely to admit optional lookahead work.
    pub fn prefetch_sounds(&mut self, names: &[String]) {
        if self.sound_demand.is_some() { return; }
        if let Some((_, decoded)) = self.sound_worker.as_ref().and_then(BoundedWorker::try_take) {
            match decoded.result {
                Ok(pcm) => { self.sounds.borrow_mut().insert(decoded.key, Some(pcm)); },
                Err(error) => {
                    tracing::debug!("speculative sound decode: {error:#}");
                    // Optional failed speculation remains eligible for one real
                    // demand; it must not turn into an assumed valid None.
                }
            }
        }
        let wanted: Vec<String> = names.iter().map(|name| name.to_ascii_lowercase()).take(WINDOW).collect();
        let Some(worker) = &self.sound_worker else { return; };
        if !worker.retain_window(self.sound_context, &wanted) { return; }
        let floor = knife_perf::memory_budget::CPU_TARGET_HEADROOM + knife_perf::memory_budget::CPU_SOUND_PREPARE_BYTES;
        if !knife_platform_vita::memory_available().ok().is_some_and(|memory| memory.heap_headroom as usize >= floor) { return; }
        for name in names.iter().take(WINDOW) {
            if self.cached_sound_effect(name).is_some() { continue; }
            let job = self.sound_job(name);
            let Some(worker) = &self.sound_worker else { return; };
            if !worker.request(job.key.clone(), job) { break; }
            knife_perf::add(Metric::SoundPrefetch, 0, 1);
        }
    }
    /// A real instruction/queued-play boundary promotes the same exact job.
    /// Ready only pins a PCM; the caller alone executes the original play once.
    pub fn poll_sound_needed(&mut self, name: &str) -> SoundNeeded {
        let key = name.to_ascii_lowercase();
        if self.sound_prepared.contains_key(&key) { return SoundNeeded::Ready; }
        let cached = { self.sounds.borrow_mut().get(&key) };
        if let Some(pcm) = cached { self.pin_sound(key, pcm); return SoundNeeded::Ready; }
        if self.sound_demand.as_ref().is_some_and(|demand| demand.key != key) {
            // The owner polls a cohort serially; a different outstanding demand
            // indicates cancellation, never permission to consume its result.
            self.reset_sound_needed_context();
        }
        self.sound_demand.get_or_insert_with(|| Demand { key: key.clone(), started: Instant::now() });
        if self.sound_demand.as_ref().is_some_and(|demand| demand.started.elapsed() >= Duration::from_secs(10)) {
            return self.sound_failed(key, "10 second decoder/admission deadline");
        }
        let Some(worker) = &self.sound_worker else { return self.sound_failed(key, "worker unavailable"); };
        let had_work = match worker.poll_needed(self.sound_context, &key) {
            WorkerPoll::Ready(decoded) => {
                worker.release_needed(&key);self.sound_demand = None;
                let transient = decoded.result.as_ref().is_err_and(Self::audio_allocation_failed);
                let pcm = match decoded.result {
                    Ok(pcm) => Some(pcm),
                    Err(error) => {
                        tracing::warn!(name=%key, "sound decode failed: {error:#}");
                        knife_perf::add(Metric::SoundDemandFailed, 0, 1);None
                    }
                };
                if !transient { self.sounds.borrow_mut().insert(key.clone(), pcm.clone()); }
                self.pin_sound(key, pcm);
                knife_perf::add(Metric::SoundDemandReady, 0, 1);
                return SoundNeeded::Ready;
            }
            WorkerPoll::Failed(error) => return self.sound_failed(key, error),
            WorkerPoll::Pending => true,
            WorkerPoll::Missing => false,
        };
        let reserve = knife_perf::memory_budget::CPU_SOUND_PREPARE_BYTES;
        let target = knife_perf::memory_budget::CPU_TARGET_HEADROOM + reserve;
        if !had_work && let Ok(memory) = knife_platform_vita::memory_available() {
            self.vita_trim_cpu_caches(target.saturating_sub(memory.heap_headroom as usize));
        }
        let minimum = knife_perf::memory_budget::CPU_PRESSURE_HEADROOM + reserve;
        if !had_work && !knife_platform_vita::memory_available().ok().is_some_and(|memory| memory.heap_headroom as usize >= minimum) {
            return self.sound_failed(key, "insufficient CPU headroom after optional cache reclamation");
        }
        let job = self.sound_job(name);
        if !self.sound_worker.as_ref().is_some_and(BoundedWorker::is_running) {
            return self.sound_failed(key, "worker stopped");
        }
        // Promote even an already queued speculative job. Otherwise a full
        // unrelated ready slot could prevent this real demand from ever running.
        self.sound_worker.as_ref().unwrap().request_needed(self.sound_context, key, job);
        knife_perf::add(Metric::SoundDemandPending, 0, 1);SoundNeeded::Pending
    }
}
