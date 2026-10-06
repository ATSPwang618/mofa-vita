use super::*;
use krkr_protocol::pixels::Pixels;
use std::sync::{
    Condvar,
    atomic::{AtomicUsize, Ordering::SeqCst},
};

#[derive(Default)]
struct Probe {
    ticks: AtomicUsize,
    suspended: AtomicBool,
    poll_waits: AtomicUsize,
}
struct EventWake {
    signalled: Mutex<bool>,
    changed: Condvar,
    probe: Arc<Probe>,
}
impl WorkerWake for EventWake {
    fn signal(&self) {
        *self.signalled.lock().unwrap() = true;
        self.changed.notify_one();
    }
    fn wait(&self, timeout: Option<Duration>) -> Result<()> {
        let state = self.signalled.lock().unwrap();
        let mut state = if let Some(timeout) = timeout {
            assert_eq!(timeout, Duration::from_millis(2));
            self.probe.poll_waits.fetch_add(1, SeqCst);
            self.changed
                .wait_timeout_while(state, timeout, |signal| !*signal)
                .unwrap()
                .0
        } else {
            self.changed.wait_while(state, |signal| !*signal).unwrap()
        };
        *state = false;
        Ok(())
    }
}
struct Movie {
    info: Info,
    probe: Arc<Probe>,
    playing: bool,
    hardware: bool,
    pending: bool,
    frame: usize,
    limit: usize,
    target: f64,
    // Force play() to race just before the worker parks.
    gate: Option<(SyncSender<()>, Receiver<()>)>,
}
impl Decoder for Movie {
    fn info(&self) -> &Info {
        &self.info
    }
    fn playback(&mut self, playing: bool, _: f64) -> Result<()> {
        self.probe.ticks.fetch_add(1, SeqCst);
        self.playing = playing;
        Ok(())
    }
    fn next(&mut self, video: bool, _: bool) -> Result<Option<Decoded>> {
        assert!(video);
        if self.playing && self.pending {
            self.pending = false;
            return Ok(Some(Decoded::Pending));
        }
        if self.hardware && !self.playing && self.frame > 0 {
            self.probe.suspended.store(true, SeqCst);
            if let Some((entered, resume)) = self.gate.take() {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(2)).unwrap();
                // Codec IO may use the same thread's park token internally.
                std::thread::park_timeout(Duration::ZERO);
            }
            return Ok(Some(Decoded::Suspended));
        }
        if self.frame == self.limit {
            return Ok(None);
        }
        let time = self.target + self.frame as f64 / 30.;
        self.frame += 1;
        Ok(Some(Decoded::Video(Frame {
            time,
            pixels: Arc::new(Pixels {
                size: self.info.size,
                main: None,
                province: None,
            })
            .into(),
        })))
    }
    fn seek(&mut self, time: f64) -> Result<()> {
        self.frame = 0;
        self.target = time;
        self.probe.suspended.store(false, SeqCst);
        Ok(())
    }
    fn audio_stream(&mut self, _: Option<usize>) -> Result<()> {
        Ok(())
    }
    fn video_stream(&mut self, _: usize) -> Result<()> {
        Ok(())
    }
}
fn start(
    hardware: bool,
    limit: usize,
    gate: Option<(SyncSender<()>, Receiver<()>)>,
) -> (Handle, Arc<Probe>, Receiver<Result<()>>) {
    let info = Info {
        size: Size {
            width: 2,
            height: 2,
        },
        fps: 30.,
        frames: limit as u64,
        duration: limit as f64 / 30.,
        audio_streams: 0,
        video_streams: 1,
        video_stream: 0,
        audio_rate: 48000,
    };
    let probe = Arc::new(Probe::default());
    let decoder = Movie {
        info: info.clone(),
        probe: probe.clone(),
        hardware,
        pending: hardware,
        playing: false,
        frame: 0,
        limit,
        target: 0.,
        gate,
    };
    let info = Arc::new(Mutex::new(info));
    let state = Arc::new(Mutex::new(State::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    let wake_generation = Arc::new(WorkerNotify::default());
    let (tx, rx) = mpsc::sync_channel(4);
    let (done, result) = mpsc::sync_channel(1);
    let worker = {
        let (info, state, cancel) = (info.clone(), state.clone(), cancel.clone());
        let wake = wake_generation.clone();
        let probe = probe.clone();
        std::thread::spawn(move || {
            *wake.primitive.lock().unwrap() = Some(Arc::new(EventWake {
                signalled: Mutex::new(false),
                changed: Condvar::new(),
                probe,
            }));
            let _ = done.send(run(Box::new(decoder), info, state, cancel, rx, None, wake));
        })
    };
    let lease = Arc::new(Lease {
        state,
        cancel,
        commands: tx,
        worker: worker.thread().clone(),
        wake_generation,
    });
    (
        Handle {
            lease,
            info,
            logical_size: None,
        },
        probe,
        result,
    )
}
fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(Instant::now() < deadline, "worker did not respond");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn quiet(probe: &Probe) {
    std::thread::sleep(Duration::from_millis(10));
    let ticks = probe.ticks.load(SeqCst);
    std::thread::sleep(Duration::from_millis(40));
    assert!(
        probe.ticks.load(SeqCst) <= ticks + 1,
        "idle worker kept polling"
    );
}
fn close(handle: Handle, done: Receiver<Result<()>>) {
    drop(handle);
    done.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
}

#[test]
fn paused_hardware_sleeps_but_seek_rate_play_and_drop_wake_it() {
    let (handle, probe, done) = start(true, 8, None);
    until(|| probe.suspended.load(SeqCst));
    quiet(&probe);
    handle.seek(2., &AtomicBool::new(false)).unwrap();
    until(|| probe.suspended.load(SeqCst));
    assert_eq!(handle.frame().unwrap().time, 2.);
    quiet(&probe);
    let ticks = probe.ticks.load(SeqCst);
    handle.rate(2.).unwrap();
    until(|| probe.ticks.load(SeqCst) > ticks);
    quiet(&probe);
    handle.play(true);
    until(|| handle.lease.state.lock().unwrap().frames.len() == QUEUED_FRAMES);
    assert!(probe.poll_waits.load(SeqCst) > 0);
    close(handle, done);
}

#[test]
fn play_before_park_is_not_lost() {
    let (entered, waiting) = mpsc::sync_channel(1);
    let (resume, gate) = mpsc::sync_channel(1);
    let (handle, _, done) = start(true, 8, Some((entered, gate)));
    waiting.recv_timeout(Duration::from_secs(2)).unwrap();
    handle.play(true);
    resume.send(()).unwrap();
    until(|| handle.lease.state.lock().unwrap().frames.len() == QUEUED_FRAMES);
    close(handle, done);
}

#[test]
fn full_video_queue_sleeps_and_consumption_refills_it() {
    let (handle, probe, done) = start(false, 8, None);
    until(|| handle.lease.state.lock().unwrap().frames.len() == QUEUED_FRAMES);
    handle.play(true);
    quiet(&probe);
    assert!(handle.frame().is_some());
    until(|| handle.lease.state.lock().unwrap().frames.len() == QUEUED_FRAMES);
    quiet(&probe);
    close(handle, done);
}

#[test]
fn eof_remains_seekable_and_can_be_closed() {
    let (handle, _, done) = start(false, 1, None);
    until(|| handle.lease.state.lock().unwrap().eof);
    assert_eq!(handle.frame().unwrap().time, 0.);
    handle.seek(3., &AtomicBool::new(false)).unwrap();
    until(|| handle.lease.state.lock().unwrap().eof);
    assert_eq!(handle.frame().unwrap().time, 3.);
    close(handle, done);
}
