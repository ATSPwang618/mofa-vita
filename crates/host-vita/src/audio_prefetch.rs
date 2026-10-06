//! Bounded compressed-audio read ahead. Native file calls run on a separate
//! worker so the PCM decoder can keep consuming already buffered input.
use krkr_protocol::budget::Permit;
use std::{
    collections::VecDeque,
    io::{self, Read, Seek, SeekFrom},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const CHUNK: usize = 16 * 1024;

struct State {
    bytes: VecDeque<u8>,
    position: u64,
    produced: u64,
    seek: Option<(u64, u64)>,
    seek_result: Option<(u64, io::Result<u64>)>,
    revision: u64,
    eof: bool,
    error: Option<io::Error>,
    stopped: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    #[cfg(target_os = "vita")]
    reader_wake: Arc<crate::wake::Wake>,
    cancel: Arc<AtomicBool>,
    _permit: Permit,
    capacity: usize,
    end: u64,
}

impl Shared {
    fn publish(&self) {
        self.changed.notify_all();
        #[cfg(target_os = "vita")]
        self.reader_wake.signal();
    }

    fn wait_reader<'a>(
        &'a self,
        state: std::sync::MutexGuard<'a, State>,
    ) -> io::Result<std::sync::MutexGuard<'a, State>> {
        #[cfg(target_os = "vita")]
        {
            // The event retains a publication between unlocking and waiting.
            drop(state);
            self.reader_wake
                .wait(Duration::from_millis(10))
                .map_err(io::Error::other)?;
            Ok(self.state.lock().unwrap())
        }
        #[cfg(not(target_os = "vita"))]
        {
            Ok(self
                .changed
                .wait_timeout(state, Duration::from_millis(10))
                .unwrap()
                .0)
        }
    }
}

/// The decoder is the only reader/seeker. The worker exclusively owns the
/// source stream, and never holds the queue mutex across native IO.
pub(super) struct ReadAhead {
    shared: Arc<Shared>,
}

impl ReadAhead {
    pub(super) fn new(
        mut source: Box<dyn krkr_assets::Stream>,
        start: u64,
        end: u64,
        capacity: usize,
        permit: Permit,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        if start > end || capacity < CHUNK {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid read-ahead range",
            ));
        }
        let position = source.seek(SeekFrom::Start(start))?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                bytes: VecDeque::with_capacity(capacity),
                position,
                produced: position,
                seek: None,
                seek_result: None,
                revision: 0,
                eof: position >= end,
                error: None,
                stopped: false,
            }),
            changed: Condvar::new(),
            #[cfg(target_os = "vita")]
            reader_wake: crate::wake::Wake::new().map_err(io::Error::other)?,
            cancel,
            _permit: permit,
            capacity,
            end,
        });
        let worker = shared.clone();
        thread::Builder::new()
            .name("krkr-audio-prefetch".into())
            .spawn(move || run(source, worker))?;
        Ok(Self { shared })
    }
}

enum Work {
    Seek(u64, u64),
    Read(usize, u64),
}

fn run(mut source: Box<dyn krkr_assets::Stream>, shared: Arc<Shared>) {
    let mut scratch = vec![0; CHUNK];
    loop {
        let work = {
            let mut state = shared.state.lock().unwrap();
            loop {
                if state.stopped || shared.cancel.load(Ordering::Acquire) {
                    return;
                }
                if let Some((position, revision)) = state.seek {
                    break Work::Seek(position, revision);
                }
                if state.eof || state.error.is_some() {
                    state = shared.changed.wait(state).unwrap();
                    continue;
                }
                let remaining = shared.end.saturating_sub(state.produced);
                if remaining == 0 {
                    state.eof = true;
                    shared.publish();
                    continue;
                }
                // Decoders consume small codec packets. Wait for a whole IO
                // chunk of space instead of issuing a native read for each
                // consumed packet. Only the final chunk may be smaller.
                let count = remaining.min(CHUNK as u64) as usize;
                if shared.capacity - state.bytes.len() < count {
                    state = shared.changed.wait(state).unwrap();
                    continue;
                }
                break Work::Read(count, state.revision);
            }
        };
        match work {
            Work::Seek(position, revision) => {
                let result = source.seek(SeekFrom::Start(position));
                let mut state = shared.state.lock().unwrap();
                if state.revision == revision && !state.stopped {
                    if let Ok(actual) = result.as_ref() {
                        state.produced = *actual;
                        state.eof = *actual >= shared.end;
                    } else {
                        state.eof = true;
                    }
                    state.seek = None;
                    state.seek_result = Some((revision, result));
                    shared.publish();
                }
            }
            Work::Read(count, revision) => {
                let result = source.read(&mut scratch[..count]);
                let mut state = shared.state.lock().unwrap();
                if state.revision != revision || state.stopped {
                    continue;
                }
                match result {
                    Ok(0) => state.eof = true,
                    Ok(count) => {
                        state.bytes.extend(&scratch[..count]);
                        state.produced += count as u64;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        state.error = Some(error);
                        state.eof = true;
                    }
                }
                shared.publish();
            }
        }
    }
}

fn interrupted() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "audio read-ahead cancelled")
}

impl Read for ReadAhead {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let mut state = self.shared.state.lock().unwrap();
        loop {
            if state.stopped || self.shared.cancel.load(Ordering::Acquire) {
                return Err(interrupted());
            }
            if !state.bytes.is_empty() {
                let space_before = self.shared.capacity - state.bytes.len();
                let refill = self
                    .shared
                    .end
                    .saturating_sub(state.produced)
                    .min(CHUNK as u64) as usize;
                let count = output.len().min(state.bytes.len());
                let (first, second) = state.bytes.as_slices();
                let first_count = count.min(first.len());
                output[..first_count].copy_from_slice(&first[..first_count]);
                output[first_count..count].copy_from_slice(&second[..count - first_count]);
                state.bytes.drain(..count);
                state.position += count as u64;
                if space_before < refill && space_before + count >= refill {
                    self.shared.changed.notify_one();
                }
                return Ok(count);
            }
            if let Some(error) = state.error.take() {
                return Err(error);
            }
            if state.eof {
                return Ok(0);
            }
            state = self.shared.wait_reader(state)?;
        }
    }
}

impl Seek for ReadAhead {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let mut state = self.shared.state.lock().unwrap();
        let position = match from {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::Current(offset) => state.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.shared.end.checked_add_signed(offset),
        };
        let position = position.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "audio seek outside file")
        })?;
        if position == state.position && state.seek.is_none() {
            return Ok(position);
        }
        let buffered_end = state.position + state.bytes.len() as u64;
        if position >= state.position && position <= buffered_end && state.seek.is_none() {
            let skip = (position - state.position) as usize;
            state.bytes.drain(..skip);
            state.position = position;
            self.shared.changed.notify_all();
            return Ok(position);
        }
        state.bytes.clear();
        state.position = position;
        state.eof = false;
        state.error = None;
        state.revision = state.revision.wrapping_add(1);
        let revision = state.revision;
        state.seek = Some((position, revision));
        state.seek_result = None;
        self.shared.changed.notify_all();
        loop {
            if state.stopped || self.shared.cancel.load(Ordering::Acquire) {
                return Err(interrupted());
            }
            if let Some((done, result)) = state.seek_result.take()
                && done == revision
            {
                return result;
            }
            state = self.shared.wait_reader(state)?;
        }
    }
}

impl Drop for ReadAhead {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        state.stopped = true;
        self.shared.changed.notify_all();
        // A native read cannot be interrupted. The detached worker releases its
        // file and byte permit when that call eventually returns.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krkr_protocol::budget::Budget;
    use std::{
        io::Cursor,
        sync::mpsc::{self, Receiver, Sender},
        time::Instant,
    };

    fn prefetch(source: impl krkr_assets::Stream + 'static, bytes: usize) -> ReadAhead {
        let budget = Budget::new(bytes + CHUNK);
        ReadAhead::new(
            Box::new(source),
            0,
            bytes as u64,
            bytes.clamp(CHUNK, 2 * CHUNK),
            budget
                .reserve(bytes.clamp(CHUNK, 2 * CHUNK) + CHUNK)
                .unwrap(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap()
    }

    struct GatedSource {
        data: Cursor<Vec<u8>>,
        reads: usize,
        entered: Sender<()>,
        resume: Receiver<()>,
    }
    impl Read for GatedSource {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if self.reads == 2 {
                self.entered.send(()).unwrap();
                self.resume.recv().unwrap();
            }
            self.data.read(output)
        }
    }
    impl Seek for GatedSource {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            self.data.seek(from)
        }
    }

    #[test]
    fn buffered_audio_remains_readable_during_a_slow_native_read() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let source = GatedSource {
            data: Cursor::new(vec![7; 3 * CHUNK]),
            reads: 0,
            entered: entered_tx,
            resume: resume_rx,
        };
        let mut input = prefetch(source, 3 * CHUNK);
        let mut first = [0];
        input.read_exact(&mut first).unwrap();
        assert_eq!(first, [7]);
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut rest = vec![0; CHUNK - 1];
            let result = input.read_exact(&mut rest).map(|()| rest);
            done_tx.send(result).unwrap();
        });
        let ready = done_rx.recv_timeout(Duration::from_millis(500));
        resume_tx.send(()).unwrap();
        assert!(ready.unwrap().unwrap().iter().all(|&byte| byte == 7));
        reader.join().unwrap();
    }

    #[test]
    fn seeking_discards_prefetched_bytes_and_restores_source_position() {
        let bytes = (0..4 * CHUNK).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        let mut input = prefetch(Cursor::new(bytes.clone()), bytes.len());
        let mut actual = [0; 100];
        input.read_exact(&mut actual).unwrap();
        assert_eq!(&actual, &bytes[..100]);
        assert_eq!(
            input
                .seek(SeekFrom::Start((2 * CHUNK + 37) as u64))
                .unwrap(),
            (2 * CHUNK + 37) as u64
        );
        input.read_exact(&mut actual).unwrap();
        assert_eq!(&actual, &bytes[2 * CHUNK + 37..2 * CHUNK + 137]);
        input.seek(SeekFrom::Start(5)).unwrap();
        input.read_exact(&mut actual).unwrap();
        assert_eq!(&actual, &bytes[5..105]);
    }

    struct ObservedSource {
        data: Cursor<Vec<u8>>,
        requests: Sender<usize>,
    }
    impl Read for ObservedSource {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.requests.send(output.len()).unwrap();
            self.data.read(output)
        }
    }
    impl Seek for ObservedSource {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            self.data.seek(from)
        }
    }

    #[test]
    fn small_decoder_reads_refill_whole_chunks_and_preserve_the_tail() {
        let (requests, received) = mpsc::channel();
        let bytes = (0..3 * CHUNK + 7)
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>();
        let mut input = prefetch(
            ObservedSource {
                data: Cursor::new(bytes.clone()),
                requests,
            },
            bytes.len(),
        );
        for _ in 0..2 {
            assert_eq!(
                received.recv_timeout(Duration::from_secs(2)).unwrap(),
                CHUNK
            );
        }
        let mut actual = vec![0; 1];
        input.read_exact(&mut actual).unwrap();
        assert!(matches!(
            received.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        actual.resize(CHUNK, 0);
        input.read_exact(&mut actual[1..]).unwrap();
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)).unwrap(),
            CHUNK
        );
        actual.resize(2 * CHUNK, 0);
        input.read_exact(&mut actual[CHUNK..]).unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), 7);
        input.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, bytes);
    }

    #[test]
    fn a_file_that_exactly_fills_the_buffer_reaches_eof() {
        let mut input = prefetch(Cursor::new(vec![7; CHUNK]), CHUNK);
        let shared = input.shared.clone();
        {
            let state = shared.state.lock().unwrap();
            let (state, _) = shared
                .changed
                .wait_timeout_while(state, Duration::from_secs(2), |state| {
                    state.bytes.len() != CHUNK
                })
                .unwrap();
            assert_eq!(state.bytes.len(), CHUNK);
        }
        let (done, received) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = input.read_to_end(&mut bytes).map(|_| bytes);
            done.send(result).unwrap();
        });
        let result = received.recv_timeout(Duration::from_secs(2));
        if result.is_err() {
            shared.cancel.store(true, Ordering::Release);
            shared.changed.notify_all();
        }
        reader.join().unwrap();
        assert_eq!(result.unwrap().unwrap(), vec![7; CHUNK]);
    }

    struct FailingSource {
        calls: usize,
    }
    impl Read for FailingSource {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.calls > 1 {
                return Err(io::Error::other("read failed"));
            }
            output.fill(9);
            Ok(output.len())
        }
    }
    impl Seek for FailingSource {
        fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
            Ok(0)
        }
    }

    #[test]
    fn read_error_follows_buffered_data() {
        let mut input = prefetch(FailingSource { calls: 0 }, 3 * CHUNK);
        let mut first = vec![0; CHUNK];
        input.read_exact(&mut first).unwrap();
        assert!(first.iter().all(|&byte| byte == 9));
        assert_eq!(
            input.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::Other
        );
    }

    #[test]
    fn dropping_a_buffered_stream_releases_its_budget() {
        let budget = Budget::new(3 * CHUNK);
        let permit = budget.reserve(3 * CHUNK).unwrap();
        let input = ReadAhead::new(
            Box::new(Cursor::new(vec![1; 3 * CHUNK])),
            0,
            3 * CHUNK as u64,
            2 * CHUNK,
            permit,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        drop(input);
        let deadline = Instant::now() + Duration::from_secs(2);
        while budget.used() != 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(budget.used(), 0);
    }
}
