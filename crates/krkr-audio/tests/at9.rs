use krkr_audio::{
    StreamDecoder,
    at9::{self, PacketDecoder},
};
use std::{
    io::{Cursor, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
const AT9: &[u8] = include_bytes!("data/timeline.at9");
fn header() -> at9::Header {
    at9::inspect(&mut Cursor::new(AT9), AT9.len() as u64)
        .unwrap()
        .unwrap()
}

#[test]
fn reference_encoder_geometry_delay_and_malformed_headers() {
    let h = header();
    assert_eq!(
        (h.format.rate, h.format.channels, h.format.frames),
        (48000, 2, 4003)
    );
    assert_eq!((h.delay, h.frame_samples, h.block_frames), (256, 256, 4));
    for length in [12, 19, 71, 99, AT9.len() - 1] {
        assert!(at9::inspect(&mut Cursor::new(&AT9[..length]), length as u64).is_err());
    }
    for (at, value) in [
        (16, 51),
        (24, 1),
        (32, 1),
        (39, 0),
        (83, 255),
        (89, 0),
        (96, 1),
    ] {
        let mut corrupt = AT9.to_vec();
        corrupt[at] = value;
        assert!(
            at9::inspect(&mut Cursor::new(&corrupt), corrupt.len() as u64).is_err(),
            "byte {at}"
        );
    }
}

#[test]
fn source_clock_retains_44100_hz_without_changing_codec_geometry() {
    let mut bytes = AT9.to_vec();
    bytes.extend(b"krSR");
    bytes.extend(8u32.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(44100u32.to_le_bytes());
    let length = bytes.len() as u32 - 8;
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    let h = at9::inspect(&mut Cursor::new(&bytes), bytes.len() as u64)
        .unwrap()
        .unwrap();
    assert_eq!(
        (h.format.rate, h.codec_rate, h.format.frames),
        (44100, 48000, 4003)
    );
    assert_eq!(h.seek(1024).unwrap(), header().seek(1024).unwrap());
    let last = bytes.len() - 4;
    bytes[last..].copy_from_slice(&0u32.to_le_bytes());
    assert!(at9::inspect(&mut Cursor::new(&bytes), bytes.len() as u64).is_err());
}

// A deterministic superframe codec isolates the streaming contract: the first
// frame after reset deliberately contains garbage (transform overlap warm-up).
struct Codec {
    h: at9::Header,
    pcm: Vec<i16>,
    warmup: bool,
}
impl PacketDecoder for Codec {
    fn reset(&mut self) -> krkr_audio::Result<()> {
        self.warmup = true;
        Ok(())
    }
    fn decode(&mut self, input: &mut dyn Read) -> krkr_audio::Result<()> {
        let mut block = vec![0; self.h.block_bytes as usize];
        input.read_exact(&mut block).map_err(|e| e.to_string())?;
        let start = u32::from_le_bytes(block[..4].try_into().unwrap());
        self.pcm.clear();
        for sample in 0..self.h.block_samples() {
            let value = if self.warmup && sample < self.h.frame_samples {
                -30000
            } else {
                (start + sample) as i16
            };
            self.pcm.push(value);
            if self.h.format.channels == 2 {
                self.pcm.push(value.wrapping_neg());
            }
        }
        self.warmup = false;
        Ok(())
    }
    fn pcm(&self) -> &[i16] {
        &self.pcm
    }
}
#[test]
fn streaming_seek_preroll_eof_and_cancellation_preserve_source_positions() {
    let h = header();
    let mut encoded = AT9.to_vec();
    for (index, block) in encoded[h.data_offset as usize..]
        .chunks_exact_mut(h.block_bytes as usize)
        .enumerate()
    {
        block[..4].copy_from_slice(&(index as u32 * h.block_samples()).to_le_bytes());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let codec = Codec {
        h,
        pcm: Vec::new(),
        warmup: false,
    };
    let budget = krkr_protocol::budget::Budget::new(16 * 1024);
    let mut stream = at9::Stream::new(
        codec,
        Box::new(Cursor::new(encoded)),
        h,
        cancel.clone(),
        &budget,
    )
    .unwrap();
    for start in [0, 767, 768, 769, 1023, 1024, 2049, 3999, 17, 4003] {
        stream.seek(start).unwrap();
        for position in start..h.format.frames {
            let f = stream.next().unwrap().unwrap();
            assert_eq!(f.position, position);
            assert_eq!(
                f.pcm[..2],
                [
                    (position + u64::from(h.delay)) as i16,
                    -((position + u64::from(h.delay)) as i16)
                ]
            );
        }
        assert!(stream.next().unwrap().is_none());
    }
    assert!(stream.seek(4004).is_err());
    stream.seek(0).unwrap();
    cancel.store(true, Ordering::Release);
    assert!(stream.next().is_err());
}

#[test]
fn buffered_packets_reduce_reads_with_bounded_memory_and_seek_invalidation() {
    use krkr_protocol::budget::Budget;
    use std::io::{Seek, SeekFrom};
    use std::sync::atomic::AtomicUsize;
    struct Counted {
        bytes: Cursor<Vec<u8>>,
        reads: Arc<AtomicUsize>,
    }
    impl Read for Counted {
        fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            self.bytes.read(into)
        }
    }
    impl Seek for Counted {
        fn seek(&mut self, at: SeekFrom) -> std::io::Result<u64> {
            self.bytes.seek(at)
        }
    }
    let mut h = header();
    h.data_bytes = u64::from(h.block_bytes) * 64;
    h.format.frames = u64::from(h.block_samples()) * 64 - u64::from(h.delay);
    let mut encoded = vec![0; (h.data_offset + h.data_bytes) as usize];
    for (index, block) in encoded[h.data_offset as usize..]
        .chunks_exact_mut(h.block_bytes as usize)
        .enumerate()
    {
        block[..4].copy_from_slice(&(index as u32 * h.block_samples()).to_le_bytes());
    }
    let reads = Arc::new(AtomicUsize::new(0));
    let budget = Budget::new(16 * 1024);
    let make = || {
        let codec = Codec {
            h,
            pcm: Vec::new(),
            warmup: false,
        };
        let input = Box::new(Counted {
            bytes: Cursor::new(encoded.clone()),
            reads: reads.clone(),
        });
        at9::Stream::new(codec, input, h, Arc::new(AtomicBool::new(false)), &budget)
    };
    let mut stream = make().unwrap();
    assert_eq!(budget.used(), 16 * 1024);
    assert!(make().is_err(), "read-ahead cannot exceed its audio budget");
    for position in 0..h.format.frames {
        assert_eq!(stream.next().unwrap().unwrap().position, position);
    }
    assert!(stream.next().unwrap().is_none());
    assert_eq!(
        reads.load(Ordering::Relaxed),
        (h.data_bytes as usize).div_ceil(16 * 1024)
    );
    // Restart within a different packet, then backwards into previously read
    // bytes. Prefetch must not return the old stream cursor after either seek.
    for position in [17000, 53, 19000, 768] {
        stream.seek(position).unwrap();
        let frame = stream.next().unwrap().unwrap();
        assert_eq!(frame.position, position);
        assert_eq!(frame.pcm[0], (position + u64::from(h.delay)) as i16);
    }
    drop(stream);
    assert_eq!(budget.used(), 0);
}

#[test]
fn bulk_at9_matches_single_frames_for_codec_rates_channels_preroll_and_tail() {
    use krkr_audio::DecodedFrame;
    use krkr_protocol::budget::Budget;
    for rate in [12000, 24000, 48000] {
        for channels in [1, 2] {
            for block_frames in [1, 4] {
                let mut h = header();
                h.codec_rate = rate;
                h.frame_samples = rate * 256 / 48000;
                h.block_frames = block_frames;
                h.delay = h.frame_samples;
                h.format.rate = 44100; // Script clock must not select codec geometry.
                h.format.channels = channels;
                h.data_bytes = (h.format.frames + u64::from(h.delay))
                    .div_ceil(u64::from(h.block_samples()))
                    * u64::from(h.block_bytes);
                let mut encoded = vec![0; (h.data_offset + h.data_bytes) as usize];
                for (i, block) in encoded[h.data_offset as usize..]
                    .chunks_exact_mut(h.block_bytes as usize)
                    .enumerate()
                {
                    block[..4].copy_from_slice(&(i as u32 * h.block_samples()).to_le_bytes());
                }
                let budget = Budget::new(64 * 1024);
                let cancel = Arc::new(AtomicBool::new(false));
                let make = || {
                    at9::Stream::new(
                        Codec {
                            h,
                            pcm: Vec::new(),
                            warmup: false,
                        },
                        Box::new(Cursor::new(encoded.clone())),
                        h,
                        cancel.clone(),
                        &budget,
                    )
                    .unwrap()
                };
                let mut bulk = make();
                let mut scalar = make();
                for start in [
                    0,
                    1,
                    u64::from(h.block_samples() - 1),
                    u64::from(h.block_samples()),
                    3999,
                    4003,
                ] {
                    bulk.seek(start).unwrap();
                    scalar.seek(start).unwrap();
                    for size in [0, 1, 63, 255, 256, 257, 1024, 4097] {
                        let mut output = vec![DecodedFrame::default(); size];
                        let count = bulk.read_frames(&mut output).unwrap();
                        let mut expected = Vec::new();
                        for _ in 0..size {
                            if let Some(frame) = scalar.next().unwrap() {
                                expected.push(frame);
                            } else {
                                break;
                            }
                        }
                        assert_eq!(count, expected.len());
                        for (actual, expected) in output[..count].iter().zip(expected) {
                            assert_eq!(
                                (actual.sample, actual.pcm, actual.position),
                                (expected.sample, expected.pcm, expected.position)
                            );
                        }
                    }
                    assert!(bulk.next().unwrap().is_none());
                }
                bulk.seek(17).unwrap();
                cancel.store(true, Ordering::Release);
                assert_eq!(bulk.read_frames(&mut []).unwrap(), 0);
                assert!(
                    bulk.read_frames(&mut [DecodedFrame::default(); 256])
                        .is_err()
                );
                drop(bulk);
                drop(scalar);
                assert_eq!(budget.used(), 0);
            }
        }
    }
}
