use krkr_assets::{ReadPlan, Vfs};
use krkr_audio::{Mixer, OutputHost, Service};
use krkr_protocol::budget::Budget;
use std::{
    sync::{Arc, Mutex, atomic::AtomicBool},
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
fn plans() -> (tempfile::TempDir, Vfs) {
    let dir = tempfile::tempdir().unwrap();
    let bytes = 48000u32 * 4;
    let mut wav = Vec::new();
    wav.extend(b"RIFF");
    wav.extend((bytes + 36).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(48000u32.to_le_bytes());
    wav.extend(192000u32.to_le_bytes());
    wav.extend(4u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(bytes.to_le_bytes());
    for _ in 0..48000 {
        wav.extend(8192i16.to_le_bytes());
        wav.extend((-8192i16).to_le_bytes());
    }
    std::fs::write(dir.path().join("tone.wav"), wav).unwrap();
    std::fs::write(
        dir.path().join("tone.sli"),
        "#2.00\nLink {From=24000;To=12000;Smooth=True;}\nLabel {Position=12000;Name='loop';}",
    )
    .unwrap();
    let vfs = Vfs::new(dir.path(), Default::default()).unwrap();
    (dir, vfs)
}
fn plan(vfs: &mut Vfs, name: &str) -> ReadPlan {
    vfs.plan(&name.encode_utf16().collect::<Vec<_>>()).unwrap()
}
fn released(budget: &Budget) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while budget.used() != 0 {
        assert!(
            Instant::now() < deadline,
            "audio allocations leaked: {}",
            budget.used()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn fifteen_effect_slots_and_two_bgm_buffers_fit_twelve_mib_and_replay() {
    let (_dir, mut vfs) = plans();
    // Keep the old limit as a regression: raising the default must not hide
    // wasteful per-voice allocation returning to this path.
    let service = Service::new(Budget::new(12 * 1024 * 1024));
    let output = Output::default();
    service.set_output(output.clone()).unwrap();
    let stop = AtomicBool::new(false);
    let mut voices = Vec::new();
    for index in 0..17 {
        let voice = service
            .open(
                plan(&mut vfs, "tone.wav"),
                (index >= 15).then(|| plan(&mut vfs, "tone.sli")),
                &stop,
            )
            .unwrap_or_else(|e| panic!("opening slot {index}: {e}"));
        voice.play(&stop).unwrap();
        voice.stop();
        voices.push(voice);
    }
    let used = service.budget().used();
    println!(
        "17 opened stereo buffers: {used} / {} bytes",
        service.budget().limit()
    );
    assert!(used < 12 * 1024 * 1024);
    // Stopping retains seek/replay state; it must not silently unload audio.
    let voice = &voices[0];
    voice.seek(0, &stop).unwrap();
    voice.play(&stop).unwrap();
    let mut samples = [0.0; 128];
    output
        .0
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .render(&mut samples, 2, 48000, Duration::ZERO);
    assert!(
        samples
            .as_chunks::<2>()
            .0
            .iter()
            .all(|s| *s == [0.25, -0.25])
    );
    assert_eq!(service.budget().used(), used);
    drop(voices);
    released(service.budget());
}

#[test]
fn pcm_reserves_no_decoder_and_releases_its_queue() {
    let budget = Budget::new(256 * 1024);
    let service = Service::new(budget.clone());
    service.set_output(Output::default()).unwrap();
    let pcm = service.pcm(48000).expect("PCM needs only its mixer queue");
    assert!(budget.used() > 0);
    assert_eq!(pcm.push(0, &[[0.25, -0.25]; 4096]), 4096);
    assert_eq!(pcm.space(), 0, "the playback queue was not shortened");
    assert!(
        service.pcm(48000).is_err(),
        "the global budget still applies"
    );
    drop(pcm);
    released(&budget);
}

#[test]
fn failed_open_releases_decoder_and_packet_reservations() {
    let (_dir, mut vfs) = plans();
    // Enough for builtin decoder state, too little for the packet/voice queue.
    let budget = Budget::new(520 * 1024);
    let service = Service::new(budget.clone());
    service.set_output(Output::default()).unwrap();
    for _ in 0..3 {
        assert!(
            service
                .open(plan(&mut vfs, "tone.wav"), None, &AtomicBool::new(false))
                .is_err()
        );
        released(&budget);
    }
}

#[test]
fn output_consumption_wakes_a_full_decoder_without_periodic_polling() {
    let (_dir, mut vfs) = plans();
    let service = Service::default();
    let output = Output::default();
    service.set_output(output.clone()).unwrap();
    let stop = AtomicBool::new(false);
    let voice = service
        .open(plan(&mut vfs, "tone.wav"), None, &stop)
        .unwrap();
    voice.play(&stop).unwrap();
    let mut mixer = output.0.lock().unwrap().take().unwrap();
    for _ in 0..20 {
        // A full queue parks the producer. Each callback must make it runnable
        // again, including wakeups that race its final fullness check.
        let mut samples = [0.0; 2048];
        mixer.render(&mut samples, 2, 48000, Duration::ZERO);
        assert!(
            samples
                .as_chunks::<2>()
                .0
                .iter()
                .all(|s| *s == [0.25, -0.25])
        );
        let submitted = voice.position().submitted;
        let deadline = Instant::now() + Duration::from_secs(3);
        while voice.position().decoded < submitted + 4096 {
            assert!(
                Instant::now() < deadline,
                "decoder missed PCM-consumption wake"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    voice.stop();
    voice.seek(0, &stop).unwrap();
    voice.play(&stop).unwrap();
    drop(voice);
    released(service.budget());
}
