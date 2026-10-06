//! A single blocking device worker. Mixing consumes bounded PCM; it performs
//! no decoding, filesystem work, or VM calls on the output thread.
#![allow(unsafe_code)]
use krkr_engine::audio::{Mixer, OutputHost};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use vitasdk_sys::*;

const FRAMES: usize = 1024;
const RATE: u32 = 48_000;

#[derive(Default)]
pub struct Audio(Mutex<Option<Worker>>);
struct Worker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            // Mixer::render temporarily owns the audio world. If it releases
            // the last owner, this destructor runs on the output thread itself.
            if thread.thread().id() != thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}
impl OutputHost for Audio {
    fn start(&self, mixer: Mixer) -> Result<(), String> {
        let mut slot = self.0.lock().map_err(|_| "Vita audio state poisoned")?;
        if slot.is_some() {
            return Err("Vita audio output already started".into());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let (started, ready) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("krkr-audio-output".into())
            .stack_size(crate::memory::AUDIO_STACK_BYTES)
            .spawn(move || match Port::open() {
                Ok(port) => {
                    if started.send(Ok(())).is_ok() {
                        run(port, mixer, stopping);
                    }
                }
                Err(error) => {
                    let _ = started.send(Err(error));
                }
            })
            .map_err(|e| e.to_string())?;
        let worker = Worker {
            stop,
            thread: Some(thread),
        };
        ready
            .recv()
            .map_err(|_| "Vita audio worker stopped during initialization")??;
        *slot = Some(worker);
        Ok(())
    }
}
struct Port(i32);
impl Port {
    fn open() -> Result<Self, String> {
        // This thread only mixes a bounded block and then blocks in Output.
        // Give audio deadlines priority over rendering/VM and AvPlayer (0xa0).
        let priority = unsafe { sceKernelChangeThreadPriority(0, 0x70) };
        if priority < 0 {
            return Err(format!("audio thread priority: 0x{priority:08x}"));
        }
        // SAFETY: scalar SDK arguments; a successful port is owned by this worker.
        let port = unsafe {
            sceAudioOutOpenPort(
                SCE_AUDIO_OUT_PORT_TYPE_MAIN,
                FRAMES as i32,
                RATE as i32,
                SCE_AUDIO_OUT_MODE_STEREO,
            )
        };
        if port < 0 {
            return Err(format!("sceAudioOutOpenPort: 0x{port:08x}"));
        }
        let port = Self(port);
        let mut volume = [SCE_AUDIO_VOLUME_0DB as i32; 2];
        // SAFETY: the SDK reads the two stereo volume entries during this call.
        let result = unsafe {
            sceAudioOutSetVolume(
                port.0,
                SCE_AUDIO_VOLUME_FLAG_L_CH | SCE_AUDIO_VOLUME_FLAG_R_CH,
                volume.as_mut_ptr(),
            )
        };
        if result < 0 {
            return Err(format!("sceAudioOutSetVolume: 0x{result:08x}"));
        }
        Ok(port)
    }
}
impl Drop for Port {
    fn drop(&mut self) {
        // SAFETY: sole owner; drain before releasing the port.
        unsafe {
            sceAudioOutOutput(self.0, std::ptr::null());
            sceAudioOutReleasePort(self.0);
        }
    }
}
fn run(port: Port, mut mixer: Mixer, stop: Arc<AtomicBool>) {
    let mut scratch = [0.0; FRAMES * 2];
    // Retain both buffers for the whole worker lifetime. The blocking SDK call
    // retires the previous submission before that buffer is reused.
    let mut buffers = [[0_i16; FRAMES * 2]; 2];
    let mut index = 0;
    while !stop.load(Ordering::Acquire) {
        // SAFETY: the port remains open and is confined to this thread.
        let remaining = unsafe { sceAudioOutGetRestSample(port.0) };
        if remaining < 0 {
            mixer.fail(format!("sceAudioOutGetRestSample: 0x{remaining:08x}"));
            break;
        }
        mixer.render_device(
            &mut scratch,
            2,
            RATE,
            Duration::from_secs_f64(remaining as f64 / RATE as f64),
        );
        for (out, sample) in buffers[index].iter_mut().zip(scratch) {
            *out = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        }
        // SAFETY: exactly FRAMES interleaved stereo i16 samples; buffers live
        // until after the final drain below, including the error path.
        let result = unsafe { sceAudioOutOutput(port.0, buffers[index].as_ptr().cast()) };
        if result < 0 {
            mixer.fail(format!("sceAudioOutOutput: 0x{result:08x}"));
            break;
        }
        index ^= 1;
    }
    // Drain before the stack buffers are destroyed.
    drop(port);
}
