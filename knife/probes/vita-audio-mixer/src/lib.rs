//! Host check for the self-generated fixture bytes used by the Vita DEV probe.
#[cfg(test)]
mod tests {
    #[test]
    fn generated_opus_fixtures_decode_to_expected_frames() {
        for (bytes, mono) in [
            (include_bytes!("../fixtures/mono.opus").as_slice(), true),
            (include_bytes!("../fixtures/stereo.opus").as_slice(), false),
        ] {
            let pcm = knife_audio::codec::decode_stream(bytes).unwrap();
            assert_eq!(pcm.sample_rate, 48_000);
            assert_eq!(pcm.frames.len(), 4_365);
            assert_eq!(pcm.mono, mono);
        }
    }
}
