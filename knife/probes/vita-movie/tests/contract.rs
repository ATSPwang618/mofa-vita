use knife_vita_movie_probe::{ASSETS, Observations};

#[test]
fn empty_native_observation_cannot_pass() {
    assert!(Observations::default().complete().is_err());
}

fn complete_logo() -> Observations {
    let mut observed = Observations::new(ASSETS[0]);
    for i in 0..313 {
        observed.video(960, 540, 16.0 / 9.0, i * 1000 / 30).unwrap();
        observed.presented().unwrap();
    }
    let pcm = [[0.25, -0.25]; 960];
    for i in 0..523 { observed.audio(&pcm, i * 20).unwrap(); }
    observed.conclude(true, 10460);
    observed
}

#[test]
fn complete_logo_requires_realistic_video_pcm_duration_and_eof() {
    complete_logo().complete().unwrap();
    let mut observed = complete_logo();
    observed.conclude(false, 10460);
    assert!(observed.complete().unwrap_err().contains("full logo"));
    observed.conclude(true, 1000);
    assert!(observed.complete().unwrap_err().contains("premature"));
    observed.conclude(true, 10460);
    observed.last_video_ms = Some(9000);
    assert!(observed.complete().is_err());
}

#[test]
fn missing_pcm_silence_and_short_audio_do_not_pass_logo() {
    let mut observed = complete_logo();
    observed.audio_nonzero_samples = 0;
    assert!(observed.complete().unwrap_err().contains("PCM"));
    observed.audio_nonzero_samples = 1;
    observed.audio_frames = 48000;
    assert!(observed.complete().is_err());
    observed.audio_frames = 502080;
    observed.last_audio_ms = None;
    assert!(observed.complete().is_err());
}

#[test]
fn dimensions_aspect_invalid_sample_and_timestamp_errors_are_rejected() {
    let mut observed = Observations::default();
    for (w, h, aspect) in [(1920,1080,16.0/9.0),(960,544,16.0/9.0),(960,540,f32::NAN),(960,540,1.0)] {
        assert!(observed.video(w,h,aspect,0).is_err());
    }
    assert!(observed.video(960,540,16.0/9.0,16000).is_err());
    observed.video(960,540,16.0/9.0,33).unwrap();
    assert!(observed.video(960,540,16.0/9.0,0).is_err());
    for value in [f32::NAN,f32::INFINITY,f32::NEG_INFINITY,1.01,-1.01] {
        assert!(observed.audio(&[[value,0.0]],0).is_err());
    }
    assert!(observed.audio(&[],0).is_err());
    assert!(observed.audio(&[[0.0;2];961],0).is_err());
    assert_eq!(observed.audio_frames,0);
    observed.audio(&[[1.0,-1.0]],20).unwrap();
    assert!(observed.audio(&[[0.0,0.0]],10).is_err());
}

#[test]
fn duplicate_descriptors_and_unsubmitted_frames_do_not_establish_video() {
    let mut observed = complete_logo();
    observed.video_timestamp_advances = 0;
    assert!(observed.complete().is_err());
    observed.video_timestamp_advances = 10;
    observed.presented_frames = 0;
    assert!(observed.complete().is_err());
    assert!(Observations::default().presented().is_err());
}

#[test]
fn video_only_openings_need_no_invented_audio_and_real_control_observations() {
    for asset in &ASSETS[1..] {
        let mut observed = Observations::new(*asset);
        assert!(observed.audio(&[[0.0,0.0]],0).is_err());
        for timestamp in [0,1000,2000] {
            observed.video(960,540,16.0/9.0,timestamp).unwrap();
            observed.presented().unwrap();
        }
        observed.conclude(false,3000);
        assert!(observed.complete().is_err());
        observed.pause_hold(2000,2000,true,false,0).unwrap();
        observed.resumed(true).unwrap();
        observed.stopped(true).unwrap();
        observed.complete().unwrap();
        observed.conclude(true,3000);
        assert!(observed.complete().is_err());
    }
}

#[test]
fn failed_pause_clock_data_or_state_and_out_of_order_controls_stay_errors() {
    let mut observed = Observations::new(ASSETS[1]);
    assert!(observed.resumed(true).is_err());
    assert!(observed.stopped(true).is_err());
    for (after,state,video,audio) in [(20,true,false,0),(0,false,false,0),(0,true,true,0),(0,true,false,1)] {
        assert!(observed.pause_hold(0,after,state,video,audio).is_err());
    }
    observed.pause_hold(0,5,true,false,0).unwrap();
    assert!(observed.resumed(false).is_err());
    observed.resumed(true).unwrap();
    assert!(observed.stopped(false).is_err());
    observed.stopped(true).unwrap();
}

#[test]
fn the_exact_three_current_sources_and_their_stream_contracts_are_explicit() {
    assert_eq!(ASSETS.map(|asset|asset.relative_path),["movie/logo.mp4","movie/opening_en.mp4","movie/opening_zc.mp4"]);
    assert_eq!(ASSETS.map(|asset|asset.has_audio),[true,false,false]);
    assert!(ASSETS.iter().all(|asset|asset.sha256.len()==64));
}
