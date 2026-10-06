use super::*;
use crate::{Handle, Mixer, OutputHost, Service, at9, filter, loops};
use std::{
    hint::black_box,
    io::Cursor,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

struct Output;
impl OutputHost for Output {
    fn start(&self, _: Mixer) -> Result<()> {
        Ok(())
    }
}
// Codec execution is deliberately cheap: measure the Rust delivery/queue path,
// not hardware decoding speed. Positions include ATRAC9's initial delay.
struct Packet {
    channels: usize,
    pcm: [i16; 2048],
}
impl at9::PacketDecoder for Packet {
    fn reset(&mut self) -> Result<()> {
        Ok(())
    }
    fn decode(&mut self, input: &mut dyn Read) -> Result<()> {
        let mut bytes = [0; 4];
        input.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        let start = u32::from_le_bytes(bytes);
        for (i, sample) in self.pcm[..1024 * self.channels]
            .chunks_exact_mut(self.channels)
            .enumerate()
        {
            let value = (start.wrapping_add(i as u32).wrapping_mul(73)) as i16;
            sample[0] = value;
            if self.channels == 2 {
                sample[1] = value.wrapping_neg();
            }
        }
        Ok(())
    }
    fn pcm(&self) -> &[i16] {
        &self.pcm[..1024 * self.channels]
    }
}
fn setup(
    frames: u64,
    channels: usize,
    info: &[u8],
    filters: Vec<filter::PhaseVocoder>,
) -> (Service, Handle, filter::Stream) {
    let budget = Budget::new(8 * 1024 * 1024);
    let format = Format {
        rate: 44100,
        channels: channels as u32,
        bits: 16,
        frames,
    };
    let blocks = (frames + 256).div_ceil(1024);
    let encoded: Vec<_> = (0..blocks)
        .flat_map(|n| (n as u32 * 1024).to_le_bytes())
        .collect();
    let header = at9::Header {
        format,
        codec_rate: 48000,
        config: [0; 4],
        data_offset: 0,
        data_bytes: encoded.len() as u64,
        block_bytes: 4,
        block_frames: 4,
        frame_samples: 256,
        delay: 256,
    };
    let source = at9::Stream::new(
        Packet {
            channels,
            pcm: [0; 2048],
        },
        Box::new(Cursor::new(encoded)),
        header,
        Arc::new(AtomicBool::new(false)),
        &budget,
    )
    .unwrap();
    let decoder = Decoder {
        backend: Backend::External(Box::new(source)),
        format,
        position: 0,
        _permit: None,
    };
    pipeline(decoder, info, filters, budget)
}
fn pipeline(
    decoder: Decoder,
    info: &[u8],
    filters: Vec<filter::PhaseVocoder>,
    budget: Budget,
) -> (Service, Handle, filter::Stream) {
    let format = decoder.format;
    let source = loops::Stream::new(
        decoder,
        if info.is_empty() {
            loops::Information::default()
        } else {
            loops::Information::parse(info).unwrap()
        },
        &budget,
    )
    .unwrap();
    let service = Service::new(budget.clone());
    service.set_output(Output).unwrap();
    let (handle, _) = service
        .voice(
            format,
            source.flags.clone(),
            source.info.clone(),
            Vec::new(),
        )
        .unwrap();
    let filter = filter::Stream::new(source, filters, budget, format.channels as usize);
    (service, handle, filter)
}

#[test]
#[ignore = "manual audio delivery CPU benchmark; excludes hardware decoding"]
fn audio_delivery_benchmark() {
    let mut voices: Vec<_> = (0..4)
        .map(|_| setup(44100 * 20, 2, b"", Vec::new()))
        .collect();
    let start = Instant::now();
    for _ in 0..3000 {
        for (_, handle, stream) in &mut voices {
            handle.voice().queue.lock().unwrap().frames.clear();
            crate::fill(handle.voice(), stream).unwrap();
            black_box(
                handle
                    .voice()
                    .decoded
                    .load(std::sync::atomic::Ordering::Relaxed),
            );
        }
    }
    println!(
        "audio delivery 4 voices / 3000 x 256 frames: {:.3} ms",
        start.elapsed().as_secs_f64() * 1000.
    );
    for (_, handle, stream) in voices {
        assert_eq!(stream.position(), 3000 * 256);
        let q = handle.voice().queue.lock().unwrap();
        assert_eq!(q.frames.len(), 256);
        assert_eq!(q.frames.back().unwrap().position, 3000 * 256 - 1);
        let expected = ((3000u32 * 256 - 1 + 256) * 73) as i16;
        assert_eq!(
            q.frames.back().unwrap().pcm[..2],
            [expected, expected.wrapping_neg()]
        );
    }
}

type Pipeline = (Service, Handle, filter::Stream);

#[test]
#[ignore = "manual hard-loop delivery CPU benchmark; excludes hardware decoding"]
fn hard_loop_delivery_benchmark() {
    let mut times = Vec::new();
    for _ in 0..9 {
        let (_, handle, mut stream) = setup(
            44100 * 20,
            2,
            b"LoopStart=128; LoopLength=44000;",
            Vec::new(),
        );
        let start = Instant::now();
        for _ in 0..3000 {
            handle.voice().queue.lock().unwrap().frames.clear();
            crate::fill(handle.voice(), &mut stream).unwrap();
            black_box(stream.position());
        }
        times.push(start.elapsed().as_secs_f64() * 1000.);
        assert_eq!(stream.position(), 128 + (3000 * 256 - 128) % 44000);
    }
    times.sort_by(f64::total_cmp);
    println!(
        "hard-loop 3000 x 256 frames: median={:.3} ms range={:.3}..{:.3}",
        times[4], times[0], times[8]
    );
}
fn compare(a: &mut Pipeline, b: &mut Pipeline, size: usize) {
    let blank = crate::Frame {
        sample: [99.; 2],
        pcm: [99; 8],
        position: u64::MAX,
        labels: [99; 2],
    };
    let mut got = vec![blank; size];
    a.2.update();
    b.2.update();
    let (count, eof) = a.2.read_frames(a.1.voice(), &mut got).unwrap();
    let mut expected = Vec::new();
    let mut ended = false;
    for _ in 0..size {
        if !b.1.voice().alive.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }
        match b.2.next(b.1.voice()).unwrap() {
            Some(frame) => expected.push(frame),
            None => {
                ended = true;
                break;
            }
        }
    }
    assert_eq!((count, eof), (expected.len(), ended));
    for (actual, expected) in got[..count].iter().zip(expected) {
        assert_eq!(
            (actual.sample, actual.pcm, actual.position, actual.labels),
            (
                expected.sample,
                expected.pcm,
                expected.position,
                expected.labels
            )
        );
    }
    assert!(got[count..].iter().all(|f| f.position == u64::MAX));
    assert_eq!(a.2.position(), b.2.position());
    for (a, b) in a.1.voice().flags.iter().zip(b.1.voice().flags.iter()) {
        assert_eq!(
            a.load(std::sync::atomic::Ordering::Relaxed),
            b.load(std::sync::atomic::Ordering::Relaxed)
        );
    }
}

#[test]
fn plain_delivery_matches_samples_through_short_files_loops_seek_and_stop() {
    for channels in [1, 2] {
        for frames in [0, 1, 253, 4003] {
            let mut a = setup(frames, channels, b"", Vec::new());
            let mut b = setup(frames, channels, b"", Vec::new());
            for looping in [false, true, false] {
                for p in [&mut a, &mut b] {
                    p.1.voice()
                        .looping
                        .store(looping, std::sync::atomic::Ordering::Relaxed);
                    p.2.seek(0).unwrap();
                }
                for size in [0, 1, 63, 256, 257, 4097, 0, 1024] {
                    compare(&mut a, &mut b, size);
                }
            }
            for p in [&mut a, &mut b] {
                p.2.seek(frames / 2).unwrap();
                p.1.voice()
                    .alive
                    .store(false, std::sync::atomic::Ordering::Release);
            }
            compare(&mut a, &mut b, 256);
            for p in [&mut a, &mut b] {
                p.1.voice()
                    .alive
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            compare(&mut a, &mut b, 256);
        }
    }
}

#[test]
fn loop_labels_conditions_crossfades_and_active_dsp_keep_sample_order() {
    for info in [
        &b"LoopStart=128; LoopLength=777;"[..],
        &b"#2.00\nLabel { Name=':[0]++'; Position=40; }\nLabel { Name='event'; Position=128; }\nLink { From=1000; To=128; Condition=eq; CondVar=0; RefValue=1; }"[..],
        &b"#2.00\nLabel { Name='event'; Position=2550; }\nLink { From=2500; To=1500; Smooth=true; Condition=no; }"[..],
    ] {
        let mut a=setup(6000,2,info,Vec::new());
        let mut b=setup(6000,2,info,Vec::new());
        for round in 0..12 {
            if round==5 {
                for p in [&a,&b] {p.1.voice().flags[0].store(0,std::sync::atomic::Ordering::Relaxed);}
            }
            compare(&mut a,&mut b,517);
        }
    }
    let control = filter::PhaseVocoder::default();
    control.set_window(64).unwrap();
    let filters = || vec![control.clone(), filter::PhaseVocoder::default()];
    let mut a = setup(12000, 2, b"", filters());
    let mut b = setup(12000, 2, b"", filters());
    for (pitch, time) in [(1., 1.), (1.25, 1.), (1., 1.), (1., 0.75), (1., 1.)] {
        control.set_pitch(pitch).unwrap();
        control.set_time(time).unwrap();
        for size in [0, 13, 256, 17, 1024] {
            compare(&mut a, &mut b, size);
        }
    }
    for p in [&mut a, &mut b] {
        p.2.seek(23).unwrap();
    }
    compare(&mut a, &mut b, 257);
}

#[test]
fn hard_loop_blocks_match_short_and_chained_boundaries_and_reject_cycles() {
    for info in [
        &b"LoopStart=0; LoopLength=1;"[..],
        &b"LoopStart=3; LoopLength=7;"[..],
        &b"#2.00\nLink { From=17; To=31; }\nLink { From=31; To=5; }"[..],
    ] {
        let mut a = setup(997, 2, info, Vec::new());
        let mut b = setup(997, 2, info, Vec::new());
        for position in [0, 1, 5, 17, 31, 991] {
            a.2.seek(position).unwrap();
            b.2.seek(position).unwrap();
            for count in [0, 1, 2, 7, 16, 256] {
                compare(&mut a, &mut b, count);
            }
        }
    }
    let (_, handle, mut stream) = setup(100, 2, b"LoopStart=0; LoopLength=0;", Vec::new());
    let mut output = [crate::Frame {
        sample: [0.; 2],
        pcm: [0; 8],
        position: 0,
        labels: [0; 2],
    }; 256];
    assert!(
        stream
            .read_frames(handle.voice(), &mut output)
            .unwrap_err()
            .contains("cycle")
    );
}

struct Framewise {
    format: Format,
    position: u64,
}
impl crate::StreamDecoder for Framewise {
    fn format(&self) -> Format {
        self.format
    }
    fn seek(&mut self, position: u64) -> Result<()> {
        self.position = position;
        Ok(())
    }
    fn next(&mut self) -> Result<Option<crate::DecodedFrame>> {
        if self.position == self.format.frames {
            return Ok(None);
        }
        let position = self.position;
        self.position += 1;
        Ok(Some(crate::DecodedFrame {
            sample: [position as f32 / 1000.; 2],
            pcm: [position as i16; 8],
            position,
        }))
    }
}
struct Short(Framewise);
impl crate::StreamDecoder for Short {
    fn format(&self) -> Format {
        self.0.format()
    }
    fn seek(&mut self, position: u64) -> Result<()> {
        self.0.seek(position)
    }
    fn next(&mut self) -> Result<Option<crate::DecodedFrame>> {
        self.0.next()
    }
    fn read_frames(&mut self, output: &mut [crate::DecodedFrame]) -> Result<usize> {
        let count = output.len().min(7);
        self.0.read_frames(&mut output[..count])
    }
}
#[test]
fn default_and_short_backend_blocks_do_not_signal_premature_eof() {
    let format = Format {
        rate: 48000,
        channels: 2,
        bits: 16,
        frames: 997,
    };
    let make = |short| {
        let backend = Framewise {
            format,
            position: 0,
        };
        let backend: Box<dyn crate::StreamDecoder> = if short {
            Box::new(Short(backend))
        } else {
            Box::new(backend)
        };
        pipeline(
            Decoder {
                backend: Backend::External(backend),
                format,
                position: 0,
                _permit: None,
            },
            b"",
            Vec::new(),
            Budget::new(4 * 1024 * 1024),
        )
    };
    for short in [false, true] {
        let mut a = make(short);
        let mut b = make(false);
        for size in [0, 1, 511, 256, 123, 1024, 0] {
            compare(&mut a, &mut b, size);
        }
    }
}
