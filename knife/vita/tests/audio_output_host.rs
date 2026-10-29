#[path = "../../knife-platform-vita/src/audio_decode.rs"] mod audio_decode;
#[path = "../../knife-platform-vita/src/audio_output.rs"] mod audio_output;
#[path = "../../knife-platform-vita/src/audio_probe.rs"] mod audio_probe;
fn main() {
    let mut report=Vec::new();
    audio_probe::run_audio_probe(&mut report).unwrap();
    let text=String::from_utf8(report).unwrap();
    println!("{text}");
    assert!(text.contains("total_frames=144000"));
    assert!(text.contains("drained=true released=true"));
}
