use krkr_audio::{Mixer, OutputHost, Service};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Option<Mixer>>>);
impl OutputHost for Output {
    fn start(&self, mixer: Mixer) -> krkr_audio::Result<()> {
        *self.0.lock().unwrap() = Some(mixer);
        Ok(())
    }
}

#[test]
fn fractional_playback_preserves_samples_across_callbacks_and_device_delay() {
    let service = Service::default();
    let output = Output::default();
    service.set_output(output.clone()).unwrap();
    let pcm = service.pcm(8000).unwrap();
    let mut mixer = output.0.lock().unwrap().take().unwrap();
    pcm.flush(100);
    let source = [0.0, 1.0, 0.0, -1.0, 0.0, 0.5, -0.5, 0.0];
    assert_eq!(pcm.push(100, &source.map(|s| [s, -s])), 8);
    pcm.eof();
    pcm.play(true);
    let expected = [
        0.0, 0.5, 1.0, 0.5, 0.0, -0.5, -1.0, -0.5, 0.0, 0.25, 0.5, 0.0, -0.5, -0.25, 0.0, 0.0,
    ];
    let mut played = Vec::new();
    // Split halfway through interpolated frames; extra channels remain silent.
    for frames in [3, 4, 9] {
        let mut samples = vec![99.0; frames * 3];
        mixer.render(&mut samples, 3, 16000, Duration::from_secs(60));
        for sample in samples.as_chunks::<3>().0.iter() {
            assert_eq!(sample[1], -sample[0]);
            assert_eq!(sample[2], 0.0);
            played.push(sample[0]);
        }
    }
    assert_eq!(played, expected);
    assert_eq!(pcm.position().submitted, 107);
    assert_eq!(
        pcm.position().played,
        100,
        "device delay gates audible progress"
    );
    mixer.render(&mut [0.0; 4], 2, 16000, Duration::from_secs(60));
    assert!(!pcm.finished(), "EOF must wait for the queued device audio");

    pcm.flush(900);
    pcm.push(900, &[[0.25, 0.75]; 4]);
    pcm.play(true);
    let mut mono = [0.0; 2];
    mixer.render(&mut mono, 1, 8000, Duration::ZERO);
    assert_eq!(mono, [0.5, 0.5]);
    assert!(
        pcm.position().played >= 900,
        "flush clears the old delayed stamps"
    );
}

#[test]
fn backward_source_positions_and_eof_follow_the_audible_clock() {
    let service = Service::default();
    let output = Output::default();
    service.set_output(output.clone()).unwrap();
    let pcm = service.pcm(48000).unwrap();
    let mut mixer = output.0.lock().unwrap().take().unwrap();
    pcm.push(1000, &[[0.25, 0.25]; 70]);
    pcm.push(20, &[[0.5, 0.5]; 30]);
    pcm.rate(2.0);
    pcm.eof();
    pcm.play(true);
    let mut samples = [0.0; 102];
    mixer.render(&mut samples, 2, 48000, Duration::from_millis(20));
    assert_eq!(&samples[..70], &[0.25; 70]);
    assert_eq!(&samples[70..100], &[0.5; 30]);
    assert_eq!(&samples[100..], &[0.0; 2]);
    assert_eq!(pcm.position().submitted, 48);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !pcm.finished() {
        assert!(Instant::now() < deadline, "audio completion timeout");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(pcm.position().played, 48);
}

#[test]
#[ignore = "manual PCM mixer throughput measurement"]
fn four_voice_mixer_workload() {
    let mut times = Vec::new();
    for round in 0..7 {
        let service = Service::default();
        let output = Output::default();
        service.set_output(output.clone()).unwrap();
        let voices: Vec<_> = (0..4).map(|_| service.pcm(48000).unwrap()).collect();
        let mut mixer = output.0.lock().unwrap().take().unwrap();
        let source = [[0.125, -0.125]; 2048];
        let mut samples = [0.0; 4096];
        let mut elapsed = Duration::ZERO;
        for block in 0..256 {
            for voice in &voices {
                assert_eq!(voice.push(block * 2048, &source), source.len());
                voice.play(true);
            }
            let start = Instant::now();
            mixer.render(&mut samples, 2, 48000, Duration::ZERO);
            elapsed += start.elapsed();
            assert!(samples.as_chunks::<2>().0.iter().all(|s| *s == [0.5, -0.5]));
        }
        if round >= 2 {
            times.push(elapsed);
        }
    }
    times.sort();
    println!(
        "four_voice_mixer_median_ms={:.3}",
        times[2].as_secs_f64() * 1000.0
    );
}

#[test]
#[ignore = "manual 48 kHz device callback throughput measurement"]
fn four_voice_resampling_workload() {
    for rate in [22050, 44100, 48000, 96000] {
        let mut times = Vec::new();
        for round in 0..7 {
            let service = Service::default();
            let output = Output::default();
            service.set_output(output.clone()).unwrap();
            let voices: Vec<_> = (0..4).map(|_| service.pcm(rate).unwrap()).collect();
            let mut mixer = output.0.lock().unwrap().take().unwrap();
            let source = [[0.125, -0.125]; 4096];
            let mut samples = [0.0; 2048];
            let mut positions = [0; 4];
            let mut elapsed = Duration::ZERO;
            for _ in 0..512 {
                for (voice, position) in voices.iter().zip(&mut positions) {
                    let count = voice.space();
                    assert_eq!(voice.push(*position, &source[..count]), count);
                    *position += count as u64;
                    voice.play(true);
                }
                let start = Instant::now();
                mixer.render(&mut samples, 2, 48000, Duration::ZERO);
                elapsed += start.elapsed();
                assert!(samples.as_chunks::<2>().0.iter().all(|s| *s == [0.5, -0.5]));
            }
            if round >= 2 {
                times.push(elapsed);
            }
        }
        times.sort();
        println!(
            "four_voice_{rate}_to_48000_median_ms={:.3}",
            times[2].as_secs_f64() * 1000.0
        );
    }
}
