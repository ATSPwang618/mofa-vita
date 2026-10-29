#[cfg(target_os = "vita")]
mod vita {
    use knife_audio::{Audio, Curve, Pcm, Voice};
    use std::{
        fs::File,
        io::Write,
        path::PathBuf,
        sync::{Arc, mpsc},
        thread,
        time::{Duration, Instant},
    };

    const ROOT: &str = "ux0:data/knife-woh-dev";
    const TOTAL_FRAMES: u64 = 4_365;
    const MAX_WAIT: Duration = Duration::from_secs(2);
    const MONO: &[u8] = include_bytes!("../../fixtures/mono.opus");
    const STEREO: &[u8] = include_bytes!("../../fixtures/stereo.opus");

    fn until_finished(voice: &Voice, name: &str) -> Result<u32, String> {
        for tick in 0..200 {
            if voice.finished() {
                return Ok(tick);
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(format!("{name}: finished timeout after 2s"))
    }
    fn ensure_no_error(audio: &Audio, phase: &str) -> Result<(), String> {
        match audio.last_error() {
            None => Ok(()),
            Some(error) => Err(format!("{phase}: {error}")),
        }
    }
    fn run(log: &mut File) -> Result<(), String> {
        writeln!(
            log,
            "Knife Vita DEV native mixer probe; TITLE_ID KNIF00009; 48000 Hz stereo S16"
        )
        .map_err(|e| e.to_string())?;
        let mono = PathBuf::from(format!("{ROOT}/mixer-mono.opus"));
        let stereo = PathBuf::from(format!("{ROOT}/mixer-stereo.opus"));
        std::fs::write(&mono, MONO).map_err(|e| e.to_string())?;
        std::fs::write(&stereo, STEREO).map_err(|e| e.to_string())?;
        writeln!(
            log,
            "fixture_mono_bytes={} fixture_stereo_bytes={} total_frames={TOTAL_FRAMES}",
            MONO.len(),
            STEREO.len()
        )
        .map_err(|e| e.to_string())?;
        let mut audio = Audio::try_new()?;
        let pcm = Arc::new(Pcm {
            sample_rate: 48_000,
            frames: vec![[0.04, 0.02]; 960],
            loop_range: None,
            mono: false,
        });
        let pcm_voice = audio.play(pcm.clone(), 0.5, 0.0, false);
        let stream_voice =
            audio.queue_stream(stereo.clone(), 0.5, 0.0, false, 1_200, None, Curve::Linear)?;
        audio.flush();
        let pcm_ticks = until_finished(&pcm_voice, "PCM concurrent")?;
        let stream_ticks = until_finished(&stream_voice, "stereo stream seek 1200 to EOF")?;
        ensure_no_error(&audio, "concurrent PCM and Opus EOF")?;
        writeln!(log,"concurrent_pcm_finished={} stream_seek_start=1200 remaining_frames={} stream_eof_finished={} waits_10ms=({}, {})",
            pcm_voice.finished(),TOTAL_FRAMES-1_200,stream_voice.finished(),pcm_ticks,stream_ticks)
            .map_err(|e|e.to_string())?;
        let eof = audio.queue_stream(
            stereo.clone(),
            0.5,
            0.0,
            false,
            TOTAL_FRAMES,
            None,
            Curve::Linear,
        )?;
        audio.flush();
        let eof_ticks = until_finished(&eof, "exact-end EOF")?;
        ensure_no_error(&audio, "exact-end EOF")?;
        writeln!(
            log,
            "exact_end_seek={} finished={} wait_10ms={}",
            TOTAL_FRAMES,
            eof.finished(),
            eof_ticks
        )
        .map_err(|e| e.to_string())?;
        let looping = audio.queue_stream(
            mono.clone(),
            0.8,
            0.03,
            true,
            1_100,
            Some((1_000, 2_200)),
            Curve::Se,
        )?;
        looping.set_pan(-0.5);
        audio.flush();
        thread::sleep(Duration::from_millis(100));
        if looping.finished() {
            return Err(format!("loop ended early: {:?}", audio.last_error()));
        }
        looping.set_gain(0.5);
        looping.stop(0.04);
        audio.flush();
        let loop_ticks = until_finished(&looping, "loop fade stop")?;
        ensure_no_error(&audio, "loop fade stop")?;
        writeln!(log,"mono_loop_range=1000..2200 start=1100 early_finished=false fade_stop_secs=0.04 finished={} wait_10ms={}",looping.finished(),loop_ticks)
            .map_err(|e|e.to_string())?;
        let paused = audio.queue_stream(mono.clone(), 0.7, 0.0, true, 0, None, Curve::Linear)?;
        paused.set_paused(true);
        audio.flush();
        thread::sleep(Duration::from_millis(50));
        if paused.finished() {
            return Err("paused loop unexpectedly finished".into());
        }
        paused.set_paused(false);
        paused.stop(0.0);
        audio.flush();
        let pause_ticks = until_finished(&paused, "pause/resume/stop")?;
        ensure_no_error(&audio, "pause/resume/stop")?;
        writeln!(
            log,
            "pause_resumed=true stop_finished={} wait_10ms={}",
            paused.finished(),
            pause_ticks
        )
        .map_err(|e| e.to_string())?;
        let invalid = audio.queue_stream(
            stereo,
            1.0,
            0.0,
            false,
            TOTAL_FRAMES + 1,
            None,
            Curve::Linear,
        )?;
        audio.flush();
        until_finished(&invalid, "invalid frame start")?;
        let expected = audio.last_error().unwrap_or_default();
        if !expected.contains("stream start exceeds source") {
            return Err(format!("missing expected seek-range error: {expected}"));
        }
        writeln!(
            log,
            "invalid_start={} finished={} expected_error={expected}",
            TOTAL_FRAMES + 1,
            invalid.finished()
        )
        .map_err(|e| e.to_string())?;
        let active = audio.play(pcm, 0.2, 0.0, true);
        audio.flush();
        let (tx, rx) = mpsc::sync_channel(1);
        let started = Instant::now();
        let join = thread::spawn(move || {
            drop(audio);
            let _ = tx.send(());
        });
        rx.recv_timeout(MAX_WAIT)
            .map_err(|_| "Audio Drop/join exceeded 2s".to_string())?;
        join.join()
            .map_err(|_| "Audio Drop worker panicked".to_string())?;
        if !active.finished() {
            return Err("active looping PCM remained unfinished after drop".into());
        }
        writeln!(
            log,
            "drop_join_elapsed_ms={} active_finished={} mixer_and_output_joined=true",
            started.elapsed().as_millis(),
            active.finished()
        )
        .map_err(|e| e.to_string())?;
        writeln!(
            log,
            "mixer EOF denotes decoder/render completion; it does not prove DAC audibility"
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn main() {
        unsafe extern "C" {
            fn kv_init_data_dir() -> i32;
        }
        let init = unsafe { kv_init_data_dir() };
        if init < 0 {
            eprintln!("mixer DEV directory init: {init:#010x}");
            std::process::exit(1);
        }
        let mut log =
            File::create(format!("{ROOT}/audio-mixer-probe.log")).expect("mixer probe log");
        let result = run(&mut log);
        if let Err(error) = &result {
            let _ = writeln!(log, "ERROR {error}");
        }
        let _ = writeln!(
            log,
            "RESULT {}",
            if result.is_ok() { "PASS" } else { "FAIL" }
        );
        let _ = log.sync_all();
        if let Err(error) = result {
            eprintln!("mixer probe: {error}");
            std::process::exit(1);
        }
    }
}
#[cfg(target_os = "vita")]
fn main() {
    vita::main();
}
#[cfg(not(target_os = "vita"))]
fn main() {
    eprintln!("Vita AudioOut required; run cargo test for host fixtures");
    std::process::exit(2);
}
