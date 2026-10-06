#[path = "../src/video_input.rs"]
mod input;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
struct Counted {
    source: Cursor<Vec<u8>>,
    reads: usize,
}
impl Read for Counted {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.reads += 1;
        self.source.read(out)
    }
}
impl Seek for Counted {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.source.seek(to)
    }
}
#[test]
fn interleaved_packet_offsets_reuse_read_ahead_and_seeks_remain_correct() {
    let data: Vec<_> = (0..input::CACHE_BYTES * 3)
        .map(|n| (n % 251) as u8)
        .collect();
    let mut reader = BufReader::with_capacity(
        input::CACHE_BYTES,
        Counted {
            source: Cursor::new(data.clone()),
            reads: 0,
        },
    );
    for at in [0usize, 4096, 128, 16000, 8000, 64000] {
        input::seek_to(&mut reader, at as u64).unwrap();
        let mut packet = [0; 512];
        reader.read_exact(&mut packet).unwrap();
        assert_eq!(&packet, &data[at..at + 512]);
    }
    assert_eq!(reader.get_ref().reads, 1);
    for at in [100000usize, 0, data.len() - 32] {
        input::seek_to(&mut reader, at as u64).unwrap();
        let mut packet = [0; 32];
        reader.read_exact(&mut packet).unwrap();
        assert_eq!(&packet, &data[at..at + 32]);
    }
    input::seek_to(&mut reader, data.len() as u64).unwrap();
    assert_eq!(reader.read(&mut [0; 16]).unwrap(), 0);
}

struct PacketSource {
    source: Cursor<Vec<u8>>,
    cancelled: Arc<AtomicBool>,
    calls: usize,
    error: Option<io::ErrorKind>,
}
impl Read for PacketSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        if let Some(kind) = self.error.take() {
            return Err(io::Error::from(kind));
        }
        if self.calls == 2 {
            self.cancelled.store(true, Ordering::Release);
        }
        let count = output.len().min(7);
        self.source.read(&mut output[..count])
    }
}
impl Seek for PacketSource {
    fn seek(&mut self, offset: SeekFrom) -> io::Result<u64> {
        self.source.seek(offset)
    }
}

#[test]
fn playback_cancel_during_a_short_read_does_not_truncate_the_sdk_packet() {
    let data: Vec<_> = (0..128).collect();
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut reader = BufReader::with_capacity(
        8,
        PacketSource {
            source: Cursor::new(data.clone()),
            cancelled: cancelled.clone(),
            calls: 0,
            error: Some(io::ErrorKind::Interrupted),
        },
    );
    let mut packet = [0; 96];
    assert_eq!(input::read_at(&mut reader, 11, &mut packet).unwrap(), 96);
    assert!(cancelled.load(Ordering::Acquire));
    assert_eq!(&packet, &data[11..107]);
    let mut tail = [0xff; 32];
    assert_eq!(input::read_at(&mut reader, 120, &mut tail).unwrap(), 8);
    assert_eq!(&tail[..8], &data[120..]);
    assert_eq!(input::read_at(&mut reader, 128, &mut tail).unwrap(), 0);
}

#[test]
fn sdk_read_still_reports_real_storage_errors() {
    let mut reader = BufReader::new(PacketSource {
        source: Cursor::new(vec![1, 2, 3]),
        cancelled: Arc::new(AtomicBool::new(false)),
        calls: 0,
        error: Some(io::ErrorKind::InvalidData),
    });
    assert_eq!(
        input::read_at(&mut reader, 0, &mut [0; 3])
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}
