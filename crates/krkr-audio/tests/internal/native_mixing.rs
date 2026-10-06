use super::*;

struct Output(Arc<Mutex<Option<Mixer>>>);
impl OutputHost for Output {
    fn start(&self, mixer: Mixer) -> Result<()> {
        *self.0.lock().unwrap() = Some(mixer);
        Ok(())
    }
}

struct Playback {
    _service: Service,
    pcm: Pcm,
    mixer: Mixer,
}
impl Playback {
    fn new(generic: bool) -> Self {
        Self::with_rate(generic, 48000)
    }
    fn with_rate(generic: bool, rate: u32) -> Self {
        let service = Service::default();
        let output = Arc::new(Mutex::new(None));
        service.set_output(Output(output.clone())).unwrap();
        let pcm = service.pcm(rate).unwrap();
        if generic {
            // A visualization consumer requires the generic per-frame path.
            // It must not change the samples or the audible playback timeline.
            pcm.handle.visualization(true).unwrap();
        }
        let mixer = output.lock().unwrap().take().unwrap();
        Self {
            _service: service,
            pcm,
            mixer,
        }
    }
}

fn compare(a: &mut Playback, b: &mut Playback, count: usize, channels: usize) {
    compare_at_rate(a, b, count, channels, 48000);
}

fn compare_at_rate(a: &mut Playback, b: &mut Playback, count: usize, channels: usize, rate: u32) {
    let mut x = vec![1.0; count * channels];
    let mut y = x.clone();
    a.mixer
        .render(&mut x, channels, rate, Duration::from_secs(60));
    b.mixer
        .render(&mut y, channels, rate, Duration::from_secs(60));
    assert_eq!(
        x, y,
        "samples differ at {count} frames, {channels} channels"
    );
    let va = a.pcm.handle.voice();
    let vb = b.pcm.handle.voice();
    assert_eq!(
        va.submitted.load(Ordering::Relaxed),
        vb.submitted.load(Ordering::Relaxed)
    );
    assert_eq!(
        va.underruns.load(Ordering::Relaxed),
        vb.underruns.load(Ordering::Relaxed)
    );
    assert_eq!(
        va.ended.load(Ordering::Relaxed),
        vb.ended.load(Ordering::Relaxed)
    );
    assert_eq!(
        va.playing.load(Ordering::Relaxed),
        vb.playing.load(Ordering::Relaxed)
    );
    let mut qa = va.queue.lock().unwrap();
    let mut qb = vb.queue.lock().unwrap();
    assert_eq!(qa.phase, qb.phase);
    assert_eq!(qa.labels_sent, qb.labels_sent);
    assert_eq!(qa.frames.len(), qb.frames.len());
    for (a, b) in qa.frames.iter().zip(&qb.frames) {
        assert_eq!((a.sample, a.position), (b.sample, b.position));
    }
    // Callback start instants differ; spacing and source positions must not.
    assert_eq!(qa.stamps.len(), qb.stamps.len());
    if let (Some(a), Some(b)) = (qa.stamps.front(), qb.stamps.front()) {
        let (ta, tb) = (a.at, b.at);
        for (a, b) in qa.stamps.iter().zip(&qb.stamps) {
            assert_eq!((a.at - ta, a.position), (b.at - tb, b.position));
        }
        assert_eq!(qa.end_at.map(|t| t - ta), qb.end_at.map(|t| t - tb));
    }
    qa.stamps.clear();
    qb.stamps.clear();
}

#[test]
fn equal_rate_matches_resampler_through_wrap_underflow_eof_and_speed_changes() {
    let mut a = Playback::new(false);
    let mut b = Playback::new(true);
    for p in [&a, &b] {
        p.pcm.push(0, &[[0.25, -0.25]; 3200]);
        p.pcm.play(true);
    }
    compare(&mut a, &mut b, 2500, 2);
    for p in [&a, &b] {
        assert_eq!(p.pcm.push(3200, &[[0.375, -0.375]; 2600]), 2600);
        assert!(
            !p.pcm
                .handle
                .voice()
                .queue
                .lock()
                .unwrap()
                .frames
                .as_slices()
                .1
                .is_empty()
        );
    }
    compare(&mut a, &mut b, 4096, 2);
    let mut position = 0;
    for round in 0..18 {
        let count = 513 + round * 17;
        let samples: Vec<_> = (0..count)
            .map(|i| [(i % 37) as f32 / 37. - 0.5, (i % 53) as f32 / 53. - 0.5])
            .collect();
        // Include backwards source positions (loop boundaries) inside buffers.
        for p in [&a, &b] {
            assert_eq!(p.pcm.push(position, &samples), count);
            assert_eq!(p.pcm.push(12, &samples[..41]), 41);
            p.pcm.gain(73000, -21000);
            p.pcm.play(true);
        }
        position += count as u64;
        for n in [0, 1, 63, 64, 79, 100] {
            compare(&mut a, &mut b, n, 1 + round % 3);
        }
        // Leave/enter native mode after a fractional phase has been created.
        for p in [&a, &b] {
            p.pcm.rate(0.5);
        }
        compare(&mut a, &mut b, 3, 2);
        for p in [&a, &b] {
            p.pcm.rate(1.0);
        }
        compare(&mut a, &mut b, 17, 2);
        for p in [&a, &b] {
            p.pcm.rate(1.5);
        }
        compare(&mut a, &mut b, 3, 2);
        for p in [&a, &b] {
            p.pcm.rate(1.0);
        }
        compare(&mut a, &mut b, 2048, 2);
    }
    for p in [&a, &b] {
        p.pcm.push(500, &[[0.2, -0.3]; 70]);
        p.pcm.eof();
    }
    compare(&mut a, &mut b, 63, 2);
    compare(&mut a, &mut b, 32, 2);
    assert!(!a.pcm.finished(), "EOF must wait for audible output");
}

#[test]
fn fractional_rates_match_visualized_mixer_across_queue_and_timeline_boundaries() {
    for rate in [8000, 11025, 22050, 44100, 48000, 96000, 192000] {
        let mut a = Playback::with_rate(false, rate);
        let mut b = Playback::with_rate(true, rate);
        for p in [&a, &b] {
            p.pcm.push(0, &[[0.25, -0.25]; 3200]);
            p.pcm.play(true);
        }
        compare_at_rate(&mut a, &mut b, 2500, 2, rate);
        for p in [&a, &b] {
            assert_eq!(p.pcm.push(3200, &[[0.375, -0.375]; 2600]), 2600);
            assert!(
                !p.pcm
                    .handle
                    .voice()
                    .queue
                    .lock()
                    .unwrap()
                    .frames
                    .as_slices()
                    .1
                    .is_empty()
            );
        }
        compare(&mut a, &mut b, 2048, 2);
        let mut position = 0;
        for round in 0..24 {
            let count = a.pcm.space().min(1800 + round * 37);
            let samples: Vec<_> = (0..count)
                .map(|i| [(i % 37) as f32 / 37. - 0.5, (i % 53) as f32 / 53. - 0.5])
                .collect();
            for p in [&a, &b] {
                assert_eq!(p.pcm.push(position, &samples), count);
                p.pcm.play(true);
                p.pcm
                    .gain(81000, if round % 2 == 0 { -30000 } else { 47000 });
            }
            // Source position can go backwards at a loop boundary without
            // resetting the fractional phase or the queued device timestamps.
            position = if round % 3 == 0 {
                17
            } else {
                position + count as u64
            };
            for n in [0, 1, 63, 64, 129, 1024] {
                compare(&mut a, &mut b, n, 1 + round % 3);
            }
            for speed in [0.25, 0.75, 1.0, 2.3, 64.0, 1.0] {
                for p in [&a, &b] {
                    p.pcm.rate(speed);
                }
                // Large speed changes exhaust the input while phase >= 1;
                // subsequent refills must consume that debt identically.
                compare(&mut a, &mut b, 129, 2);
            }
            compare_at_rate(&mut a, &mut b, 71, 2, 44100);
        }
        for p in [&a, &b] {
            p.pcm.flush(300);
            p.pcm.push(300, &[[0.125, -0.375]; 71]);
            p.pcm.eof();
            p.pcm.play(true);
        }
        compare(&mut a, &mut b, 17, 3);
        compare(&mut a, &mut b, 1024, 3);
        assert!(a.pcm.handle.voice().ended.load(Ordering::Relaxed));
        assert!(!a.pcm.finished(), "EOF must wait for audible output");
    }
}
