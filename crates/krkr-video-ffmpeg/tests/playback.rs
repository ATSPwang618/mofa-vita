use krkr_protocol::budget::Budget;
use krkr_video::{Backend, Decoded};
use std::{
    io::Write,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};

fn chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(data.len() as u64).to_le_bytes(), data].concat()
}
fn archive(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    let packed = encoder.finish().unwrap();
    let mut bytes = krkr_assets::xp3::SIGNATURE.to_vec();
    bytes.extend_from_slice(&(19 + packed.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&packed);
    let mut info = 0u32.to_le_bytes().to_vec();
    info.extend_from_slice(&(data.len() as u64).to_le_bytes());
    info.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    let name: Vec<u16> = "movie.mp4".encode_utf16().collect();
    info.extend_from_slice(&(name.len() as u16).to_le_bytes());
    info.extend(name.into_iter().flat_map(u16::to_le_bytes));
    let mut segm = 1u32.to_le_bytes().to_vec();
    for value in [19, data.len() as u64, packed.len() as u64] {
        segm.extend_from_slice(&value.to_le_bytes());
    }
    let file = [
        chunk(b"info", &info),
        chunk(b"segm", &segm),
        chunk(b"adlr", &0u32.to_le_bytes()),
    ]
    .concat();
    let index = chunk(b"File", &file);
    bytes.push(0);
    bytes.extend_from_slice(&(index.len() as u64).to_le_bytes());
    bytes.extend(index);
    bytes
}
struct Output(Arc<Mutex<Option<krkr_audio::Mixer>>>);
impl krkr_audio::OutputHost for Output {
    fn start(&self, mixer: krkr_audio::Mixer) -> krkr_audio::Result<()> {
        *self.0.lock().unwrap() = Some(mixer);
        Ok(())
    }
}
#[test]
fn file_xp3_stream_switch_seek_and_audible_completion() {
    // Generated testsrc2/color + sine; MPEG-4 with B frames and two AAC tracks.
    // No game assets or reference builds are needed to repeat this check.
    let data = include_bytes!("data/movie.mp4");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("movie.mp4"), data).unwrap();
    std::fs::write(dir.path().join("movie.xp3"), archive(data)).unwrap();
    let mut vfs = krkr_assets::Vfs::new(dir.path(), Default::default()).unwrap();
    let backend = krkr_video_ffmpeg::Ffmpeg;
    let budget = Budget::new(32 * 1024 * 1024);
    let mut hashes = Vec::new();
    for path in ["movie.mp4", "movie.xp3>movie.mp4"] {
        let plan = vfs.plan(&path.encode_utf16().collect::<Vec<_>>()).unwrap();
        let mut decoder = backend
            .open(plan, budget.clone(), Arc::new(AtomicBool::new(false)))
            .unwrap();
        assert_eq!(
            (decoder.info().audio_streams, decoder.info().video_streams),
            (2, 2)
        );
        let mut times = Vec::new();
        let mut hash = 0u64;
        let mut audio_frames = 0;
        while let Some(decoded) = decoder.next(true, true).unwrap() {
            match decoded {
                Decoded::Pending | Decoded::Suspended | Decoded::AudioEnd => {}
                Decoded::Video(frame) => {
                    times.push(frame.time);
                    let krkr_video::VideoPixels::Rgba(pixels) = frame.pixels else {
                        panic!("FFmpeg must return RGBA")
                    };
                    hash = pixels
                        .main
                        .as_ref()
                        .unwrap()
                        .as_slice()
                        .iter()
                        .fold(hash, |hash, byte| {
                            hash.wrapping_mul(31).wrapping_add(*byte as u64)
                        });
                }
                Decoded::Audio { samples, .. } => audio_frames += samples.len(),
            }
        }
        assert_eq!(times.len(), 48);
        assert!(times.windows(2).all(|t| t[0] < t[1]));
        assert!((95000..=98000).contains(&audio_frames));
        hashes.push(hash);
        // A full decoded video queue must not prevent reading the audio track.
        decoder.seek(0.0).unwrap();
        let mut buffered_audio = 0;
        loop {
            match decoder.next(false, true).unwrap() {
                Some(Decoded::Audio { samples, .. }) => buffered_audio += samples.len(),
                Some(Decoded::AudioEnd) => {}
                Some(Decoded::Pending) => break,
                _ => panic!("unexpected output while only audio is requested"),
            }
        }
        assert!(buffered_audio >= 95000);
        let mut buffered_video = 0;
        while let Some(output) = decoder.next(true, false).unwrap() {
            if matches!(output, Decoded::Video(_)) {
                buffered_video += 1;
            } else {
                panic!("video packets failed to drain");
            }
        }
        assert_eq!(buffered_video, 48);
        decoder.audio_stream(Some(1)).unwrap();
        decoder.video_stream(1).unwrap();
        decoder.seek(1.0).unwrap();
        assert_eq!(
            (decoder.info().size.width, decoder.info().size.height),
            (96, 64)
        );
        let mut selected_frames = 0;
        while let Some(frame) = decoder.next(true, true).unwrap() {
            if let Decoded::Video(frame) = frame
                && frame.time >= 1.0
            {
                selected_frames += 1;
            }
        }
        assert!(selected_frames >= 10);
    }
    assert_eq!(hashes[0], hashes[1]);
    assert_eq!(budget.used(), 0);
    let mixer = Arc::new(Mutex::new(None));
    let audio = krkr_audio::Service::default();
    audio.set_output(Output(mixer.clone())).unwrap();
    let service = krkr_video::Service::new(audio);
    service.set_backend(backend);
    let cancelled = AtomicBool::new(false);
    let handle = service
        .open(
            vfs.plan(&"movie.xp3>movie.mp4".encode_utf16().collect::<Vec<_>>())
                .unwrap(),
            &cancelled,
        )
        .unwrap();
    handle.seek(1.0, &cancelled).unwrap();
    handle.play(true);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut shown = 0;
    let mut heard = false;
    while !handle.finished() {
        assert!(
            Instant::now() < deadline,
            "video stalled: {} {:?}",
            handle.position(),
            handle.error()
        );
        assert_eq!(handle.error(), None);
        let mut output = [0.0; 480];
        mixer
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .render(&mut output, 2, 48000, Duration::ZERO);
        heard |= output.iter().any(|v| v.abs() > 0.01);
        if let Some(frame) = handle.frame() {
            assert!(frame.time >= 1.0);
            shown += 1;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(shown >= 10);
    assert!(heard);
    assert!(handle.position() >= 1.99);
    handle.play(false);
    let paused = handle.position();
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(handle.position(), paused);
    handle.audio_stream(None, &cancelled).unwrap();
    handle.seek(1.8, &cancelled).unwrap();
    handle.play(true);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !handle.finished() {
        handle.frame();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(handle);
    std::thread::sleep(Duration::from_millis(10));
    let mut output = [1.0; 480];
    mixer
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .render(&mut output, 2, 48000, Duration::ZERO);
    assert!(output.iter().all(|v| *v == 0.0));
}
