use krkr_audio::DecoderBackend;
use krkr_protocol::budget::Budget;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[test]
fn concurrent_at9_effects_fit_without_generic_probe_and_resample_buffers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("effect.at9"),
        include_bytes!("../../krkr-audio/tests/data/timeline.at9"),
    )
    .unwrap();
    let mut vfs = krkr_assets::Vfs::new(dir.path(), Default::default()).unwrap();
    let plan = vfs
        .plan(&"effect.at9".encode_utf16().collect::<Vec<_>>())
        .unwrap();
    let budget = Budget::new(16 * 1024 * 1024);
    let mut decoders = (0..16)
        .map(|_| {
            krkr_video_ffmpeg::WaveBackend
                .open(
                    &plan,
                    budget.clone(),
                    false,
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    for decoder in &mut decoders {
        let mut count = 0;
        while decoder.next().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, decoder.format().frames);
    }
    assert!(budget.used() < 10 * 1024 * 1024);
    drop(decoders);
    assert_eq!(budget.used(), 0);
}
#[test]
fn at9_audible_timeline_matches_reference_and_seeked_samples() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = include_bytes!("../../krkr-audio/tests/data/timeline.at9").to_vec();
    let mut clock = b"krSR".to_vec();
    clock.extend(8u32.to_le_bytes());
    clock.extend(1u32.to_le_bytes());
    clock.extend(44100u32.to_le_bytes());
    bytes.splice(12..12, clock);
    let length = bytes.len() as u32 - 8;
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    std::fs::write(dir.path().join("wave.at9"), bytes).unwrap();
    let mut vfs = krkr_assets::Vfs::new(dir.path(), Default::default()).unwrap();
    let plan = vfs
        .plan(&"wave.at9".encode_utf16().collect::<Vec<_>>())
        .unwrap();
    let budget = Budget::new(8 * 1024 * 1024);
    let mut decoder = krkr_video_ffmpeg::WaveBackend
        .open(
            &plan,
            budget.clone(),
            false,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert_eq!(decoder.format().frames, 4003);
    assert_eq!(decoder.format().rate, 44100);
    let mut pcm = Vec::new();
    while let Some(frame) = decoder.next().unwrap() {
        pcm.push(frame.pcm[..2].to_vec());
    }
    assert_eq!(pcm.len(), 4003);
    let reference = include_bytes!("../../krkr-audio/tests/data/timeline.pcm");
    // FFmpeg 9's ATRAC9 output has the opposite global polarity to at9tool.
    // Permit one sign for the entire asset, never a time/channel-dependent
    // change. All 4,003 sample positions must still agree within quantization.
    let max_error = [1, -1]
        .map(|sign| {
            pcm.iter()
                .flatten()
                .zip(reference.as_chunks::<2>().0.iter())
                .map(|(&a, b)| (i32::from(a) - sign * i32::from(i16::from_le_bytes(*b))).abs())
                .max()
                .unwrap()
        })
        .into_iter()
        .min()
        .unwrap();
    assert!(max_error <= 2, "decoder/reference error {max_error}");
    for position in [0, 767, 768, 769, 1024, 2049, 3999, 17] {
        decoder.seek(position).unwrap();
        for (index, expected) in pcm.iter().enumerate().skip(position as usize) {
            let actual = decoder.next().unwrap().unwrap();
            assert_eq!(actual.position, index as u64);
            assert_eq!(&actual.pcm[..2], expected, "seek {position} sample {index}");
        }
        assert!(decoder.next().unwrap().is_none());
    }
    drop(decoder);
    assert_eq!(budget.used(), 0);
    // Converted AT9 is a host capability, not a game-specific plugin request.
    let service = krkr_audio::Service::default();
    struct Silent;
    impl krkr_audio::OutputHost for Silent {
        fn start(&self, _: krkr_audio::Mixer) -> krkr_audio::Result<()> {
            Ok(())
        }
    }
    service.set_output(Silent).unwrap();
    service.set_decoder_backend(krkr_video_ffmpeg::WaveBackend);
    let handle = service
        .open(plan, None, &AtomicBool::new(false))
        .unwrap_or_else(|e| panic!("AT9 service open: {e}"));
    assert_eq!(handle.format().rate, 44100);
}
#[test]
fn pure_audio_pcm_opus_aac_seek_and_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let samples: Vec<i16> = (0..4096).map(|i| ((i % 97) * 511 - 24000) as i16).collect();
    let mut wav = b"RIFF".to_vec();
    wav.extend((36 + samples.len() as u32 * 2).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(48000u32.to_le_bytes());
    wav.extend(96000u32.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend((samples.len() as u32 * 2).to_le_bytes());
    wav.extend(samples.iter().flat_map(|n| n.to_le_bytes()));
    std::fs::write(dir.path().join("wave.wav"), wav).unwrap();
    std::fs::write(
        dir.path().join("wave.opus"),
        include_bytes!("data/wave.opus"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("movie.mp4"),
        include_bytes!("data/movie.mp4"),
    )
    .unwrap();
    let mut vfs = krkr_assets::Vfs::new(dir.path(), Default::default()).unwrap();
    let backend = krkr_video_ffmpeg::WaveBackend;
    let budget = Budget::new(12 * 1024 * 1024);
    let plan = vfs
        .plan(&"wave.wav".encode_utf16().collect::<Vec<_>>())
        .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut decoder = backend
        .open(&plan, budget.clone(), false, cancel.clone())
        .unwrap();
    for (i, &sample) in samples.iter().enumerate() {
        let f = decoder.next().unwrap().unwrap();
        assert_eq!(f.position, i as u64);
        assert_eq!(
            f.sample,
            [sample as f32 / 32768.; 2],
            "mono amplitude must not be attenuated"
        );
    }
    assert!(decoder.next().unwrap().is_none());
    decoder.seek(1011).unwrap();
    assert_eq!(
        decoder.next().unwrap().unwrap().sample,
        [samples[1011] as f32 / 32768.; 2]
    );
    drop(decoder);
    for (name, opus) in [("wave.opus", true), ("movie.mp4", false)] {
        let plan = vfs.plan(&name.encode_utf16().collect::<Vec<_>>()).unwrap();
        let mut d = backend
            .open(&plan, budget.clone(), opus, cancel.clone())
            .unwrap();
        if opus {
            assert_eq!(d.format().frames, 48000);
            assert_eq!(d.format().rate, 48000);
        }
        let mut frames = 0;
        let mut energy = 0.;
        while let Some(f) = d.next().unwrap() {
            energy += f.sample[0].abs();
            frames += 1;
            assert!(frames < 500000);
        }
        assert!(frames > 1000 && energy > 1.);
        if opus {
            assert_eq!(frames, 48000);
        }
        d.seek(9001).unwrap();
        assert_eq!(d.next().unwrap().unwrap().position, 9001);
    }
    cancel.store(true, Ordering::Release);
    assert!(backend.open(&plan, budget.clone(), false, cancel).is_err());
    assert_eq!(budget.used(), 0);
}
