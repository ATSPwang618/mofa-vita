use super::Session;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// Streams a bounded recording to JSONL without blocking engine threads on writes.
pub struct FileCapture {
    session: Option<Session>,
    writer: Option<JoinHandle<Result<(), String>>>,
}
impl FileCapture {
    pub fn start(path: &Path, capacity: usize) -> Result<Self, String> {
        let (session, receiver) = Session::start(capacity).map_err(str::to_string)?;
        let file = File::create(path).map_err(|e| e.to_string())?;
        let writer = std::thread::Builder::new()
            .name("profile-writer".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut file = BufWriter::with_capacity(256 * 1024, file);
                let mut flush = Instant::now();
                loop {
                    match crate::channel::recv_timeout(&receiver, Duration::from_millis(200)) {
                        Ok(event) => {
                            serde_json::to_writer(&mut file, &event).map_err(|e| e.to_string())?;
                            file.write_all(b"\n").map_err(|e| e.to_string())?;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    if flush.elapsed() >= Duration::from_secs(1) {
                        file.flush().map_err(|e| e.to_string())?;
                        flush = Instant::now();
                    }
                }
                file.flush().map_err(|e| e.to_string())
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            session: Some(session),
            writer: Some(writer),
        })
    }
    pub fn finish(mut self) -> Result<u64, String> {
        let dropped = self.session.take().unwrap().finish();
        self.writer
            .take()
            .unwrap()
            .join()
            .map_err(|_| "profile writer panicked")??;
        Ok(dropped)
    }
}
impl Drop for FileCapture {
    fn drop(&mut self) {
        self.session.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}
