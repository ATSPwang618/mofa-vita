use super::*;

struct Output(Arc<Mutex<Option<Mixer>>>);
impl OutputHost for Output {
    fn start(&self, mixer: Mixer) -> Result<()> {
        *self.0.lock().unwrap() = Some(mixer);
        Ok(())
    }
}

#[test]
fn dedicated_device_waits_for_pcm_copy_instead_of_inserting_silence() {
    let service = Service::default();
    let output = Arc::new(Mutex::new(None));
    service.set_output(Output(output.clone())).unwrap();
    let pcm = service.pcm(48_000).unwrap();
    pcm.push(0, &[[0.25, -0.25]; 2048]);
    pcm.play(true);
    let mut mixer = output.lock().unwrap().take().unwrap();
    let mut initial = [0.; 128];
    mixer.render_device(&mut initial, 2, 48_000, Duration::ZERO);
    let voice = service.0.voices.lock().unwrap()[0].upgrade().unwrap();
    let queue = voice.queue.lock().unwrap();
    let (started, ready) = mpsc::sync_channel(1);
    let (done, finished) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let mut samples = [0.; 2048];
        started.send(()).unwrap();
        mixer.render_device(&mut samples, 2, 48_000, Duration::ZERO);
        done.send(samples).unwrap();
    });
    ready.recv().unwrap();
    assert!(matches!(
        finished.recv_timeout(Duration::from_millis(20)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(queue);
    let samples = finished.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.join().unwrap();
    assert!(
        samples
            .as_chunks::<2>()
            .0
            .iter()
            .all(|s| *s == [0.25, -0.25])
    );
    assert_eq!(voice.underruns.load(Ordering::Relaxed), 0);
}

#[test]
fn changed_registry_is_retried_after_contention() {
    let service = Service::default();
    let output = Arc::new(Mutex::new(None));
    service.set_output(Output(output.clone())).unwrap();
    let first = service.pcm(48_000).unwrap();
    first.push(0, &[[0.25, 0.25]; 1024]);
    first.play(true);
    let mut mixer = output.lock().unwrap().take().unwrap();
    let mut samples = [0.; 128];
    mixer.render(&mut samples, 2, 48_000, Duration::ZERO);
    let revision = mixer.voice_revision;
    let second = service.pcm(48_000).unwrap();
    second.push(0, &[[0.125, 0.125]; 1024]);
    second.play(true);
    let registry = service.0.voices.lock().unwrap();
    mixer.render(&mut samples, 2, 48_000, Duration::ZERO);
    assert_eq!(samples, [0.25; 128]);
    assert_eq!(mixer.voice_revision, revision);
    drop(registry);
    mixer.render(&mut samples, 2, 48_000, Duration::ZERO);
    assert_eq!(samples, [0.375; 128]);
    assert_ne!(mixer.voice_revision, revision);
    drop((first, second));
    mixer.render(&mut samples, 2, 48_000, Duration::ZERO);
    assert_eq!(samples, [0.; 128]);
    assert_eq!(service.budget().used(), 0);
}

#[test]
fn registry_contention_keeps_existing_audio_and_observes_stop() {
    let service = Service::default();
    let output = Arc::new(Mutex::new(None));
    service.set_output(Output(output.clone())).unwrap();
    let pcm = service.pcm(48_000).unwrap();
    pcm.push(0, &[[0.25, -0.25]; 1024]);
    pcm.play(true);
    let mut mixer = output.lock().unwrap().take().unwrap();
    let mut samples = [0.0; 128];
    mixer.render(&mut samples, 2, 48_000, Duration::ZERO);
    assert!(
        samples
            .as_chunks::<2>()
            .0
            .iter()
            .all(|s| *s == [0.25, -0.25])
    );

    let registry = service.0.voices.lock().unwrap();
    // This would previously return a whole silent block. The callback must
    // also remain nonblocking when the registry owner cannot be scheduled.
    samples.fill(0.0);
    mixer.render(&mut samples, 2, 48_000, Duration::ZERO);
    assert!(
        samples
            .as_chunks::<2>()
            .0
            .iter()
            .all(|s| *s == [0.25, -0.25])
    );
    drop(pcm);
    mixer.render(&mut samples, 2, 48_000, Duration::ZERO);
    assert_eq!(samples, [0.0; 128]);
    assert_eq!(service.0.budget.used(), 0, "snapshot must not retain PCM");
    drop(registry);

    let replacement = service.pcm(48_000).unwrap();
    replacement.push(0, &[[0.5, 0.125]; 256]);
    replacement.play(true);
    mixer.render(&mut samples, 2, 48_000, Duration::ZERO);
    assert!(
        samples
            .as_chunks::<2>()
            .0
            .iter()
            .all(|s| *s == [0.5, 0.125])
    );
}
