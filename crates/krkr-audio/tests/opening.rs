use krkr_assets::{ReadPlan, ReadSource, Stream};
use krkr_audio::{
    DecodedFrame, DecoderBackend, DecoderSource, Format, Mixer, OutputHost, Service, StreamDecoder,
    at9,
};
use krkr_protocol::budget::Budget;
use std::{
    io::{Cursor, Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Default)]
struct Counts {
    opens: AtomicUsize,
    reads: AtomicUsize,
    seeks: AtomicUsize,
    bytes: AtomicUsize,
}
struct Source {
    bytes: Arc<[u8]>,
    counts: Arc<Counts>,
}
struct Input {
    bytes: Cursor<Arc<[u8]>>,
    counts: Arc<Counts>,
}
impl Read for Input {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        self.counts.reads.fetch_add(1, Ordering::Relaxed);
        let count = self.bytes.read(output)?;
        self.counts.bytes.fetch_add(count, Ordering::Relaxed);
        Ok(count)
    }
}
impl Seek for Input {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.counts.seeks.fetch_add(1, Ordering::Relaxed);
        self.bytes.seek(position)
    }
}
impl ReadSource for Source {
    fn open(&self) -> krkr_assets::Result<Box<dyn Stream>> {
        self.counts.opens.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(Input {
            bytes: Cursor::new(self.bytes.clone()),
            counts: self.counts.clone(),
        }))
    }
}
fn plan(bytes: Vec<u8>) -> (ReadPlan, Arc<Counts>) {
    let counts = Arc::<Counts>::default();
    let size = bytes.len();
    (
        ReadPlan::custom(
            "test.wav".encode_utf16().collect(),
            size as u64,
            size,
            Arc::new(Source {
                bytes: bytes.into(),
                counts: counts.clone(),
            }),
        ),
        counts,
    )
}
struct Output;
impl OutputHost for Output {
    fn start(&self, _: Mixer) -> krkr_audio::Result<()> {
        Ok(())
    }
}
struct Empty(Format);
impl StreamDecoder for Empty {
    fn format(&self) -> Format {
        self.0
    }
    fn seek(&mut self, _: u64) -> krkr_audio::Result<()> {
        Ok(())
    }
    fn next(&mut self) -> krkr_audio::Result<Option<DecodedFrame>> {
        Ok(None)
    }
}
struct PreparedBackend;
impl DecoderBackend for PreparedBackend {
    fn open(
        &self,
        _: &ReadPlan,
        _: Budget,
        _: bool,
        _: Arc<AtomicBool>,
    ) -> krkr_audio::Result<Box<dyn StreamDecoder>> {
        panic!("AT9 input must not be reopened")
    }
    fn open_prepared(
        &self,
        _: &ReadPlan,
        _: Budget,
        _: bool,
        _: Arc<AtomicBool>,
        mut source: DecoderSource,
    ) -> krkr_audio::Result<Box<dyn StreamDecoder>> {
        let header = source.at9.expect("validated AT9 metadata");
        source
            .stream
            .seek(SeekFrom::Start(header.data_offset))
            .unwrap();
        let mut packet = [0; 4];
        source.stream.read_exact(&mut packet).unwrap();
        assert_eq!(packet, include_bytes!("data/timeline.at9")[100..104]);
        Ok(Box::new(Empty(header.format)))
    }
}

#[test]
fn atrac_backend_receives_the_open_stream_and_validated_header() {
    let (plan, counts) = plan(include_bytes!("data/timeline.at9").to_vec());
    let service = Service::default();
    service.set_output(Output).unwrap();
    service.set_decoder_backend(PreparedBackend);
    let handle = service.open(plan, None, &AtomicBool::new(false)).unwrap();
    assert_eq!(handle.format().frames, 4003);
    assert_eq!(counts.opens.load(Ordering::Relaxed), 1);
    // One format prefix, one buffered header read and the backend's packet.
    assert_eq!(counts.reads.load(Ordering::Relaxed), 3);
}

#[test]
fn builtin_wave_decoder_reuses_the_detection_stream() {
    let mut wav = b"RIFF".to_vec();
    wav.extend(548u32.to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(48000u32.to_le_bytes());
    wav.extend(96000u32.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(512u32.to_le_bytes());
    wav.resize(556, 0);
    let (plan, counts) = plan(wav);
    let service = Service::default();
    service.set_output(Output).unwrap();
    let handle = service.open(plan, None, &AtomicBool::new(false)).unwrap();
    assert_eq!(handle.format().frames, 256);
    assert_eq!(counts.opens.load(Ordering::Relaxed), 1);
    handle.play(&AtomicBool::new(false)).unwrap();
}

#[test]
fn header_buffer_skips_payload_and_reads_trailing_source_clock() {
    let mut bytes = include_bytes!("data/timeline.at9").to_vec();
    bytes.extend(b"krSR");
    bytes.extend(8u32.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(44100u32.to_le_bytes());
    let size = bytes.len() as u32 - 8;
    bytes[4..8].copy_from_slice(&size.to_le_bytes());
    let (plan, counts) = plan(bytes);
    let mut input = plan.open().unwrap();
    let header = at9::inspect(&mut input, plan.bytes).unwrap().unwrap();
    assert_eq!(header.format.rate, 44100);
    assert_eq!(counts.reads.load(Ordering::Relaxed), 2);
    assert_eq!(counts.seeks.load(Ordering::Relaxed), 2);
    assert_eq!(counts.bytes.load(Ordering::Relaxed), 512 + 16);
}
