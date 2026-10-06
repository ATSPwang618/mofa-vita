#[path = "../src/video_audio_clock.rs"]
mod clock;
use clock::AudioClock;

#[test]
fn millisecond_aac_timestamps_do_not_cut_samples_or_insert_silence() {
    for round in [0, 24000] {
        let mut clock = AudioClock::default();
        for packet in 0..10000u64 {
            let sample = packet * 1024;
            let ms = (sample * 1000 + round) / 48000;
            assert_eq!(clock.packet(ms, 48000, 1024), sample);
        }
    }
}

#[test]
fn real_gaps_and_seek_offsets_are_preserved() {
    let mut clock = AudioClock::default();
    assert_eq!(clock.packet(100, 48000, 1024), 4800);
    assert_eq!(clock.packet(121, 48000, 1024), 5824);
    assert_eq!(clock.packet(1000, 48000, 1024), 48000);
    clock.reset();
    assert_eq!(clock.packet(1021, 48000, 1024), 49008);
}
