//! ATRAC9 hardware decoder. The audio callback only consumes queued PCM;
//! blocking decode/IO runs on the existing per-voice workers.
#![allow(unsafe_code)]
use krkr_engine::audio::{DecoderBackend, DecoderSource, Result, StreamDecoder, at9};
use krkr_protocol::budget::{Budget, Permit};
use std::{
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use vitasdk_sys::*;

use crate::audio_prefetch::ReadAhead;

const TYPE: u32 = 0x1003;
static LIBRARY: Mutex<u32> = Mutex::new(0);
struct Library;
impl Library {
    fn acquire() -> Result<Self> {
        let mut users = LIBRARY.lock().unwrap();
        if *users == 0 {
            let mut init = SceAudiodecInitParam {
                at9: SceAudiodecInitChParam {
                    size: size_of::<SceAudiodecInitParam>() as u32,
                    totalCh: 16,
                },
            };
            check(unsafe { sceAudiodecInitLibrary(TYPE, &mut init) }, "init")?;
        }
        *users += 1;
        Ok(Self)
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        let mut users = LIBRARY.lock().unwrap();
        *users -= 1;
        if *users == 0 {
            let code = unsafe { sceAudiodecTermLibrary(TYPE) };
            if code < 0 {
                krkr_protocol::log!(Warn, "[VITA][AT9] terminate failed: {code:#x}");
            }
        }
    }
}
fn check(code: i32, op: &str) -> Result<()> {
    if code < 0 {
        Err(format!("AT9 {op}: {code:#010x}"))
    } else {
        Ok(())
    }
}

pub struct Backend;
impl DecoderBackend for Backend {
    fn open(
        &self,
        plan: &krkr_assets::ReadPlan,
        budget: Budget,
        _opus: bool,
        cancel: Arc<AtomicBool>,
    ) -> Result<Box<dyn StreamDecoder>> {
        let mut input = plan
            .open_interruptible(&|| cancel.load(Ordering::Acquire))
            .map_err(|e| e.to_string())?;
        let header = at9::inspect(&mut input, plan.bytes)?
            .ok_or("Vita hardware audio requires ATRAC9; convert this resource offline")?;
        Self::prepare(input, header, budget, cancel)
    }
    fn open_prepared(
        &self,
        _plan: &krkr_assets::ReadPlan,
        budget: Budget,
        _opus: bool,
        cancel: Arc<AtomicBool>,
        source: DecoderSource,
    ) -> Result<Box<dyn StreamDecoder>> {
        let header = source
            .at9
            .ok_or("Vita hardware audio requires ATRAC9; convert this resource offline")?;
        Self::prepare(source.stream, header, budget, cancel)
    }
}
impl Backend {
    fn prepare(
        mut input: Box<dyn krkr_assets::Stream>,
        header: at9::Header,
        budget: Budget,
        cancel: Arc<AtomicBool>,
    ) -> Result<Box<dyn StreamDecoder>> {
        // Keep bounded compressed data ahead of the PCM decoder. Size it by
        // codec rate so short voices do not compete with scene loading for
        // excessive read-ahead bandwidth.
        let bytes_per_second = u64::from(header.block_bytes) * u64::from(header.codec_rate)
            / u64::from(header.block_samples());
        let capacity = header
            .data_bytes
            .min(bytes_per_second.saturating_mul(6).saturating_add(16 * 1024))
            .clamp(16 * 1024, 128 * 1024) as usize;
        let input: Box<dyn krkr_assets::Stream> =
            if let Ok(permit) = budget.reserve(capacity + 16 * 1024) {
                Box::new(
                    ReadAhead::new(
                        input,
                        header.data_offset,
                        header.data_offset + header.data_bytes,
                        capacity,
                        permit,
                        cancel.clone(),
                    )
                    .map_err(|e| format!("AT9 prefetch: {e}"))?,
                )
            } else {
                input
                    .seek(SeekFrom::Start(header.data_offset))
                    .map_err(|e| e.to_string())?;
                input
            };
        let codec = Codec {
            handle: None,
            header,
            budget: budget.clone(),
        };
        Ok(Box::new(at9::Stream::new(
            codec, input, header, cancel, &budget,
        )?))
    }
}

#[repr(align(256))]
struct Es([u8; 4096]);
#[repr(align(256))]
struct Pcm([i16; 2048]);
struct Handle {
    // These allocations never move; all pointers in ctrl refer only to them.
    ctrl: SceAudiodecCtrl,
    _info: Box<SceAudiodecInfo>,
    es: Box<Es>,
    pcm: Box<Pcm>,
    header: at9::Header,
    samples: usize,
    created: bool,
    _library: Library,
    _memory: Permit,
}
// A decoder is exclusively owned by one worker at a time. The SDK calls are
// synchronous and retain no Rust references; moving it leaves boxed data fixed.
unsafe impl Send for Handle {}
impl Handle {
    fn new(header: at9::Header, budget: Budget) -> Result<Self> {
        // Bounded buffers plus a conservative allowance for the native handle.
        let memory = budget
            .reserve(512 * 1024 + size_of::<Es>() + size_of::<Pcm>())
            .map_err(|e| e.to_string())?;
        let library = Library::acquire()?;
        let mut info = Box::new(SceAudiodecInfo {
            at9: SceAudiodecInfoAt9 {
                size: size_of::<SceAudiodecInfoAt9>() as u32,
                configData: header.config,
                ch: 0,
                bitRate: 0,
                samplingRate: 0,
                superFrameSize: 0,
                framesInSuperFrame: 0,
            },
        });
        let mut es = Box::new(Es([0; 4096]));
        let mut pcm = Box::new(Pcm([0; 2048]));
        let ctrl = SceAudiodecCtrl {
            size: size_of::<SceAudiodecCtrl>() as u32,
            handle: -1,
            pEs: es.0.as_mut_ptr(),
            inputEsSize: 0,
            maxEsSize: 0,
            pPcm: pcm.0.as_mut_ptr().cast(),
            outputPcmSize: 0,
            maxPcmSize: 0,
            wordLength: 16,
            pInfo: &mut *info,
        };
        let mut codec = Self {
            ctrl,
            _info: info,
            es,
            pcm,
            header,
            samples: 0,
            created: false,
            _library: library,
            _memory: memory,
        };
        check(
            unsafe { sceAudiodecCreateDecoder(&mut codec.ctrl, TYPE) },
            "create",
        )?;
        codec.created = true;
        let info = unsafe { codec._info.at9 };
        if info.ch != header.format.channels
            || info.samplingRate != header.codec_rate
            || info.superFrameSize != header.block_bytes
            || info.framesInSuperFrame != header.block_frames
            || codec.ctrl.maxEsSize as usize > codec.es.0.len() / header.block_frames as usize
            || codec.ctrl.maxPcmSize as usize > codec.pcm.0.len() * 2 / header.block_frames as usize
        {
            return Err("AT9 hardware configuration exceeds declared format/buffers".into());
        }
        Ok(codec)
    }
}
impl at9::PacketDecoder for Handle {
    fn reset(&mut self) -> Result<()> {
        self.samples = 0;
        check(unsafe { sceAudiodecClearContext(&mut self.ctrl) }, "reset")
    }
    fn decode(&mut self, input: &mut dyn Read) -> Result<()> {
        input
            .read_exact(&mut self.es.0[..self.header.block_bytes as usize])
            .map_err(|e| format!("AT9 stream: {e}"))?;
        self.ctrl.pEs = self.es.0.as_mut_ptr();
        self.ctrl.pPcm = self.pcm.0.as_mut_ptr().cast();
        self.ctrl.inputEsSize = 0;
        self.ctrl.outputPcmSize = 0;
        check(
            unsafe { sceAudiodecDecodeNFrames(&mut self.ctrl, self.header.block_frames) },
            "decode",
        )?;
        let expected = self.header.block_samples() * self.header.format.channels * 2;
        if self.ctrl.inputEsSize != self.header.block_bytes || self.ctrl.outputPcmSize != expected {
            return Err(format!(
                "AT9 decode size mismatch: ES={} PCM={} expected={}/{expected}",
                self.ctrl.inputEsSize, self.ctrl.outputPcmSize, self.header.block_bytes
            ));
        }
        self.samples = expected as usize / 2;
        Ok(())
    }
    fn pcm(&self) -> &[i16] {
        &self.pcm.0[..self.samples]
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        if self.created {
            let code = unsafe { sceAudiodecDeleteDecoder(&mut self.ctrl) };
            if code < 0 {
                krkr_protocol::log!(Warn, "[VITA][AT9] delete failed: {code:#x}");
            }
        }
    }
}

// Loading/preparing a sound does not consume a hardware channel. Allocate on
// first decode, release after EOF or seek, and reacquire for loop playback.
struct Codec {
    handle: Option<Handle>,
    header: at9::Header,
    budget: Budget,
}
impl at9::PacketDecoder for Codec {
    fn reset(&mut self) -> Result<()> {
        self.handle = None;
        Ok(())
    }
    fn decode(&mut self, input: &mut dyn Read) -> Result<()> {
        if self.handle.is_none() {
            let mut handle = Handle::new(self.header, self.budget.clone())?;
            handle.reset()?;
            self.handle = Some(handle);
        }
        self.handle.as_mut().unwrap().decode(input)
    }
    fn pcm(&self) -> &[i16] {
        self.handle.as_ref().map_or(&[], |h| h.pcm())
    }
    fn finish(&mut self) {
        self.handle = None;
    }
}
