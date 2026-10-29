//! End-to-end DEV probe: static self-generated Opus fixtures, decoder thread, AudioOut worker.
use crate::{audio_decode::{AudioStream, CHUNK_FRAMES}, audio_output::{AudioOutput, AudioProducer, OutputError}};
use std::{io::Write, thread, time::Duration};
const MONO: &[u8] = include_bytes!("audio_fixtures/mono.opus");
const STEREO: &[u8] = include_bytes!("audio_fixtures/stereo.opus");
const PER_SOURCE: u64 = 72_000;

#[derive(Debug, Default)]
struct DecodeStats { frames: u64, eos: u32, seeks: u32, mono: u64, stereo: u64 }
fn sample_to_s16(v: f32) -> i16 { (v.clamp(-1.0, 1.0) * 32767.0).round() as i16 }
fn submit_with_backpressure(producer: &AudioProducer, samples: &[i16]) -> Result<(), String> {
    loop {
        match producer.try_submit(samples) {
            Ok(()) => return Ok(()),
            Err(OutputError::QueueFull) => thread::sleep(Duration::from_millis(1)),
            Err(error) => return Err(error.to_string()),
        }
    }
}
fn stream_fixture(data: &'static [u8], channels: u32,
                  producer: &AudioProducer, report: &mut DecodeStats,
                  staging: &mut [i16; CHUNK_FRAMES*2], staged: &mut usize) -> Result<(), String> {
    let mut decoder = AudioStream::open_memory(data).map_err(|e| e.to_string())?;
    if decoder.channels()!=channels || decoder.total_frames()!=4_365 {
        return Err(format!("unexpected fixture metadata: channels={} frames={}", decoder.channels(), decoder.total_frames()));
    }
    decoder.seek(100).map_err(|e| e.to_string())?;
    if decoder.tell()!=100 { return Err("seek/tell mismatch".into()); }
    report.seeks+=1;
    decoder.seek(0).map_err(|e| e.to_string())?;
    report.seeks+=1;
    let mut consumed=0u64;
    let mut stereo=[[0f32; 2]; CHUNK_FRAMES];
    while consumed<PER_SOURCE {
        let requested=(PER_SOURCE-consumed).min(CHUNK_FRAMES as u64) as usize;
        let count=decoder.read(&mut stereo[..requested]).map_err(|e| e.to_string())?;
        if count==0 {
            if decoder.tell()!=decoder.total_frames() { return Err("premature EOF".into()); }
            report.eos+=1;
            decoder.seek(0).map_err(|e| e.to_string())?;
            report.seeks+=1;
            continue;
        }
        for frame in &stereo[..count] {
            staging[*staged*2]=sample_to_s16(frame[0]);
            staging[*staged*2+1]=sample_to_s16(frame[1]);
            *staged+=1;
            if *staged==CHUNK_FRAMES {
                submit_with_backpressure(producer,staging)?;
                *staged=0;
            }
        }
        consumed+=count as u64;
    }
    report.frames+=consumed;
    if channels==1 { report.mono=consumed; } else { report.stereo=consumed; }
    Ok(())
}
fn decode_worker(producer: AudioProducer) -> Result<DecodeStats, String> {
    let mut report=DecodeStats::default();
    let mut staging=[0i16;CHUNK_FRAMES*2];
    let mut staged=0usize;
    stream_fixture(STEREO,2,&producer,&mut report,&mut staging,&mut staged)?;
    stream_fixture(MONO,1,&producer,&mut report,&mut staging,&mut staged)?;
    if staged!=0 { return Err("probe duration did not align to AudioOut block".into()); }
    Ok(report)
}
pub fn run_audio_probe(log: &mut impl Write) -> Result<(), String> {
    writeln!(log,"Knife Vita DEV AudioOut probe v1; 48000 Hz stereo S16; static Opus fixtures")
        .map_err(|e| e.to_string())?;
    let output=AudioOutput::open().map_err(|e| e.to_string())?;
    output.set_volume(32768,32768).map_err(|e| e.to_string())?;
    output.set_paused(true).map_err(|e| e.to_string())?;
    let producer=output.producer();
    let decoder=thread::Builder::new().name("knife-opus-decode".into()).stack_size(128*1024)
        .spawn(move || decode_worker(producer)).map_err(|e| e.to_string())?;
    thread::sleep(Duration::from_millis(60));
    if let Err(error)=output.set_paused(false) {
        let _=output.stop();
        let _=decoder.join();
        return Err(error.to_string());
    }
    let decoded=decoder.join().map_err(|_| "Opus worker panicked".to_string())?;
    let decoded=match decoded {
        Ok(value)=>value,
        Err(error)=>{ let _=output.stop(); return Err(error); }
    };
    let emitted=output.finish().map_err(|e| e.to_string())?;
    writeln!(log,"decoder_thread=joined mono_frames={} stereo_frames={} total_frames={} eof={} seeks={}",
        decoded.mono,decoded.stereo,decoded.frames,decoded.eos,decoded.seeks).map_err(|e| e.to_string())?;
    writeln!(log,"output_thread=joined source_frames={} output_blocks={} silence_blocks={} drained={} released={}",
        emitted.source_frames,emitted.output_blocks,emitted.silence_blocks,emitted.drained,emitted.released)
        .map_err(|e| e.to_string())?;
    if decoded.frames!=144_000 || decoded.mono!=72_000 || decoded.stereo!=72_000 ||
        decoded.eos<2 || decoded.seeks<4 || emitted.source_frames!=decoded.frames ||
        emitted.output_blocks-emitted.silence_blocks!=150 ||
        !emitted.drained || !emitted.released {
        return Err("AudioOut sample/EOF/seek/lifecycle invariant failed".into());
    }
    Ok(())
}
