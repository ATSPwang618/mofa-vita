//! AvPlayer hardware decoding into physically contiguous CPU-readable buffers.
//! The portable media worker owns the player. Native frame memory is copied
//! to bounded NV12 buffers before the next SDK call can recycle it.
#![allow(unsafe_code)]
use krkr_engine::assets::{ReadPlan, Stream};
use krkr_protocol::{
    budget::{Budget, Permit},
    graphics::Size,
    pixels::Yuv420,
};
use krkr_video::{Decoded, Decoder, Frame, Info, Result, VideoPixels};
use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    io::BufReader,
    ptr,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use vitasdk_sys::*;

#[path = "video_audio_clock.rs"]
mod audio_clock;
#[path = "video_format.rs"]
mod format;
#[path = "video_frame_layout.rs"]
mod frame_layout;
#[path = "video_frame_pool.rs"]
mod frame_pool;
#[path = "video_input.rs"]
mod input_buffer;

unsafe extern "C" {
    #[link_name = "memalign"]
    fn memalign(alignment: usize, size: usize) -> *mut c_void;
    #[link_name = "free"]
    fn free(pointer: *mut c_void);
}
struct Memory {
    budget: Budget,
    live: Mutex<HashMap<usize, Permit>>,
    failure: Mutex<Option<String>>,
}
impl Memory {
    fn fail(&self, message: String) -> *mut c_void {
        if let Ok(mut failure) = self.failure.lock() {
            failure.get_or_insert(message);
        }
        ptr::null_mut()
    }
    fn permit(&self, pointer: *mut c_void, permit: Permit) -> *mut c_void {
        if !pointer.is_null() {
            self.live.lock().unwrap().insert(pointer as usize, permit);
        }
        pointer
    }
}
unsafe extern "C" fn allocate(arg: *mut c_void, alignment: u32, size: u32) -> *mut c_void {
    let memory = unsafe { &*arg.cast::<Memory>() };
    let permit = match memory.budget.reserve(size as usize) {
        Ok(permit) => permit,
        Err(error) => return memory.fail(format!("AvPlayer heap allocation: {error}")),
    };
    let alignment = alignment.max(8);
    if !alignment.is_power_of_two() {
        return memory.fail(format!("AvPlayer invalid heap alignment: {alignment}"));
    }
    let pointer = unsafe { memalign(alignment as usize, size as usize) };
    if !pointer.is_null() {
        // AvPlayer's replacement allocator also supplies internal state.
        unsafe { ptr::write_bytes(pointer.cast::<u8>(), 0, size as usize) };
    } else {
        return memory.fail(format!(
            "AvPlayer memalign failed: size={size}, alignment={alignment}"
        ));
    }
    memory.permit(pointer, permit)
}
unsafe extern "C" fn deallocate(arg: *mut c_void, pointer: *mut c_void) {
    unsafe { free(pointer) };
    if let Ok(mut live) = unsafe { &*arg.cast::<Memory>() }.live.lock() {
        live.remove(&(pointer as usize));
    }
}
unsafe extern "C" fn allocate_texture(arg: *mut c_void, alignment: u32, size: u32) -> *mut c_void {
    let memory = unsafe { &*arg.cast::<Memory>() };
    let layout = match frame_layout::FrameLayout::new(alignment, size) {
        Ok(layout) => layout,
        Err(error) => {
            return memory.fail(format!(
                "AvPlayer {error}: size={size}, alignment={alignment}"
            ));
        }
    };
    let permit = match memory.budget.reserve(layout.bytes as usize) {
        Ok(permit) => permit,
        Err(error) => return memory.fail(format!("AvPlayer frame allocation: {error}")),
    };
    // Hardware returned INVALID_ARGUMENT for the explicit HAS_ALIGNMENT
    // options on this PHYCONT path. Use native block alignment, as in
    // vita-moonlight's hardware decoder, and validate the returned address.
    let block = unsafe {
        sceKernelAllocMemBlock(
            c"krkr av frame".as_ptr(),
            SCE_KERNEL_MEMBLOCK_TYPE_USER_MAIN_PHYCONT_NC_RW,
            layout.bytes,
            ptr::null_mut(),
        )
    };
    if block < 0 {
        return memory.fail(format!(
            "AvPlayer physical frame block: 0x{block:08x}, requested={size}, block_bytes={}, alignment={alignment}, options=null",
            layout.bytes
        ));
    }
    let mut pointer = ptr::null_mut();
    let result = unsafe { sceKernelGetMemBlockBase(block, &mut pointer) };
    if result < 0 || pointer.is_null() {
        unsafe { sceKernelFreeMemBlock(block) };
        return memory.fail(format!("AvPlayer physical frame address: 0x{result:08x}"));
    }
    let Some(offset) = layout.offset(pointer as usize) else {
        unsafe { sceKernelFreeMemBlock(block) };
        return memory.fail(format!(
            "AvPlayer physical frame alignment: base={pointer:p}, requested={size}, block_bytes={}, alignment={alignment}",
            layout.bytes
        ));
    };
    let pointer = unsafe { pointer.cast::<u8>().add(offset).cast::<c_void>() };
    // AvPlayer writes physical memory and video() copies it on the CPU. The
    // GLES renderer uploads those bytes into its own PVR textures. Mapping this
    // allocation with sceGxmMapMemory incorrectly requires an initialized GXM
    // renderer (as used by vita2d); this PVR/EGL host never initializes GXM and
    // never submits this pointer as a GXM texture.
    memory.permit(pointer, permit)
}
unsafe extern "C" fn deallocate_texture(arg: *mut c_void, pointer: *mut c_void) {
    if pointer.is_null() {
        return;
    }
    let block = unsafe { sceKernelFindMemBlockByAddr(pointer, 0) };
    if block >= 0 {
        unsafe { sceKernelFreeMemBlock(block) };
    }
    if let Ok(mut live) = unsafe { &*arg.cast::<Memory>() }.live.lock() {
        live.remove(&(pointer as usize));
    }
}
struct Input {
    stream: Mutex<BufReader<Box<dyn Stream>>>,
    _read_permit: Permit,
    length: u64,
    failure: Mutex<Option<String>>,
}
unsafe extern "C" fn open_file(_: *mut c_void, _: *const std::ffi::c_char) -> i32 {
    0
}
unsafe extern "C" fn close_file(_: *mut c_void) -> i32 {
    0
}
unsafe extern "C" fn file_size(arg: *mut c_void) -> u64 {
    unsafe { &*arg.cast::<Input>() }.length
}
unsafe extern "C" fn read_file(
    arg: *mut c_void,
    buffer: *mut u8,
    position: u64,
    length: u32,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let input = unsafe { &*arg.cast::<Input>() };
        if buffer.is_null() || length > i32::MAX as u32 {
            return -1;
        }
        let Ok(mut stream) = input.stream.lock() else {
            return -1;
        };
        let bytes = unsafe { std::slice::from_raw_parts_mut(buffer, length as usize) };
        match input_buffer::read_at(&mut *stream, position, bytes) {
            Ok(count) => count as i32,
            Err(error) => {
                let mut failure = input.failure.lock().unwrap();
                if failure.is_none() {
                    let message =
                        format!("AvPlayer file read: offset={position}, bytes={length}: {error}");
                    krkr_protocol::log!(Warn, "[VITA][VIDEO] {message}");
                    *failure = Some(message);
                }
                -1
            }
        }
    }))
    .unwrap_or(-1)
}
fn check(code: i32, operation: &str) -> Result<()> {
    if code < 0 {
        Err(format!("{operation}: 0x{code:08x}"))
    } else {
        Ok(())
    }
}
fn init() -> Result<()> {
    static RESULT: OnceLock<std::result::Result<(), i32>> = OnceLock::new();
    RESULT
        .get_or_init(|| {
            if unsafe { sceSysmoduleIsLoaded(SCE_SYSMODULE_AVPLAYER) } == 0 {
                return Ok(());
            }
            let code = unsafe { sceSysmoduleLoadModule(SCE_SYSMODULE_AVPLAYER) };
            if code < 0 { Err(code) } else { Ok(()) }
        })
        .map_err(|code| format!("load AvPlayer: 0x{code:08x}"))
}
pub struct Backend {
    budget: Budget,
}
impl Default for Backend {
    fn default() -> Self {
        Self {
            budget: Budget::new(24 * 1024 * 1024),
        }
    }
}
macro_rules! video_log {
    ($message:expr) => {
        krkr_protocol::log!(Warn, "[VITA][VIDEO] {}", $message);
    };
}
impl krkr_video::WorkerWake for crate::wake::Wake {
    fn signal(&self) {
        crate::wake::Wake::signal(self);
    }
    fn wait(&self, timeout: Option<Duration>) -> Result<()> {
        match timeout {
            Some(timeout) => crate::wake::Wake::wait(self, timeout),
            None => crate::wake::Wake::wait_forever(self),
        }
    }
}
impl krkr_video::Backend for Backend {
    fn worker_wake(&self) -> Result<Arc<dyn krkr_video::WorkerWake>> {
        Ok(crate::wake::Wake::new()?)
    }
    fn open(&self, plan: ReadPlan, _: Budget, cancel: Arc<AtomicBool>) -> Result<Box<dyn Decoder>> {
        let player = Player::open(plan, self.budget.clone(), cancel).inspect_err(|error| {
            video_log!(&format!(
                "open failed: {error}; {}",
                crate::memory::free_report()
            ));
        })?;
        Ok(Box::new(player))
    }
}
struct Player {
    _watchdog: Option<crate::watchdog::Watchdog>,
    dma_enabled: bool,
    handle: SceAvPlayerHandle,
    // SDK callbacks can outlive an individual call, but stop before Close returns.
    _memory: Box<Memory>,
    input: Box<Input>,
    cancel: Arc<AtomicBool>,
    budget: Budget,
    info: Info,
    frames: frame_pool::FramePool,
    pending: VecDeque<Decoded>,
    audio_enabled: bool,
    playing: bool,
    paused: bool,
    prefetch: bool,
    speed: i32,
    origin: u64,
    seek_target: f64,
    video_end: bool,
    audio_end: bool,
    audio_end_reported: bool,
    audio_until: Option<u64>,
    audio_position: u64,
    audio_clock: audio_clock::AudioClock,
}
impl Drop for Player {
    fn drop(&mut self) {
        krkr_protocol::diagnostic!("[VITA][VIDEO] stop begin handle={:#x}", self.handle);
        unsafe {
            let stopping = crate::watchdog::scope(crate::watchdog::Stage::VideoStop);
            let stop = sceAvPlayerStop(self.handle);
            drop(stopping);
            krkr_protocol::diagnostic!(
                "[VITA][VIDEO] stop end handle={:#x} result={stop:#x}; close begin",
                self.handle
            );
            let _closing = crate::watchdog::scope(crate::watchdog::Stage::VideoClose);
            let close = sceAvPlayerClose(self.handle);
            krkr_protocol::diagnostic!(
                "[VITA][VIDEO] close end handle={:#x} result={close:#x}",
                self.handle
            );
            if close < 0 {
                video_log!(&format!("close failed: {close:#x}"));
            }
        }
    }
}
impl Player {
    fn open(plan: ReadPlan, budget: Budget, cancel: Arc<AtomicBool>) -> Result<Self> {
        let watchdog = crate::watchdog::Watchdog::start_named(
            krkr_protocol::diagnostics::enabled(),
            "krkr-video-decode",
        )?;
        let _opening = crate::watchdog::scope(crate::watchdog::Stage::VideoOpen);
        init()?;
        let stream = plan
            .open_interruptible(&|| cancel.load(Ordering::Acquire))
            .map_err(|e| e.to_string())?;
        let read_permit = budget
            .reserve(input_buffer::CACHE_BYTES)
            .map_err(|e| e.to_string())?;
        let mut stream = BufReader::with_capacity(input_buffer::CACHE_BYTES, stream);
        let format = format::read(&mut stream, plan.bytes)?;
        input_buffer::seek_to(&mut stream, 0).map_err(|e| e.to_string())?;
        let mut input = Box::new(Input {
            stream: Mutex::new(stream),
            _read_permit: read_permit,
            length: plan.bytes,
            failure: Mutex::default(),
        });
        let mut memory = Box::new(Memory {
            budget: budget.clone(),
            live: Mutex::default(),
            failure: Mutex::default(),
        });
        let mut data = unsafe { std::mem::zeroed::<SceAvPlayerInitData>() };
        data.memoryReplacement.objectPointer = (&mut *memory as *mut Memory).cast();
        data.memoryReplacement.allocate = Some(allocate);
        data.memoryReplacement.deallocate = Some(deallocate);
        data.memoryReplacement.allocateTexture = Some(allocate_texture);
        data.memoryReplacement.deallocateTexture = Some(deallocate_texture);
        data.fileReplacement.objectPointer = (&mut *input as *mut Input).cast();
        data.fileReplacement.open = Some(open_file);
        data.fileReplacement.close = Some(close_file);
        data.fileReplacement.readOffset = Some(read_file);
        data.fileReplacement.size = Some(file_size);
        data.basePriority = 0xa0;
        data.numOutputVideoFrameBuffers = 2;
        data.autoStart = SCE_TRUE as SceBool;
        let handle = unsafe { sceAvPlayerInit(&mut data) };
        // Valid handles are pointer-like and can have the sign bit set.
        // Reject the whole AvPlayer error facility, including undocumented
        // errors, before Drop or any other SDK call can consume the handle.
        if handle == 0 || (handle as u32 & 0xffff_0000) == 0x806a_0000 {
            return Err(format!("initialize AvPlayer: 0x{handle:08x}"));
        }
        let mut player = Self {
            _watchdog: watchdog,
            dma_enabled: true,
            handle,
            _memory: memory,
            input,
            cancel,
            budget,
            frames: frame_pool::FramePool::default(),
            info: Info {
                size: format.size,
                fps: format.fps,
                frames: format.frames,
                duration: 0.,
                audio_streams: 0,
                video_streams: 1,
                video_stream: 0,
                audio_rate: 48000,
            },
            pending: VecDeque::new(),
            audio_enabled: true,
            playing: false,
            paused: false,
            prefetch: true,
            speed: 100,
            origin: 0,
            seek_target: 0.,
            video_end: false,
            audio_end: false,
            audio_end_reported: false,
            audio_until: None,
            audio_position: 0,
            audio_clock: audio_clock::AudioClock::default(),
        };
        check(
            unsafe { sceAvPlayerAddSource(handle, c"movie.mp4".as_ptr()) },
            "open hardware movie",
        )?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut timestamps = Vec::new();
        while timestamps.len() < 2 && Instant::now() < deadline {
            player.cancelled()?;
            if let Some(frame) = player.video()? {
                timestamps.push(frame.time);
                player.pending.push_back(Decoded::Video(frame));
            }
            if !player
                .pending
                .iter()
                .any(|d| matches!(d, Decoded::Audio { .. }))
                && let Some(audio) = player.audio()?
            {
                player.pending.push_back(audio);
                player.info.audio_streams = 1;
            }
            if timestamps.len() == 1 && !unsafe { sceAvPlayerIsActive(handle) != 0 } {
                break;
            }
            if timestamps.len() < 2 {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        if timestamps.is_empty() {
            return Err("AvPlayer did not produce a video frame within 10 seconds".into());
        }
        // Use container sample timing for frame-number APIs. Startup frame
        // timestamps from AvPlayer can be irregular (22 ms for a 29.97 fps
        // movie on hardware); they are still used to schedule presentation.
        for index in 0..32 {
            let mut stream = unsafe { std::mem::zeroed::<SceAvPlayerStreamInfo>() };
            if unsafe { sceAvPlayerGetStreamInfo(handle, index, &mut stream) } < 0 {
                break;
            }
            player.info.duration = player.info.duration.max(stream.duration as f64 / 1000.);
            if stream.type_ == SCE_AVPLAYER_VIDEO {
                player.origin = stream.startTime;
            }
            if stream.type_ == SCE_AVPLAYER_AUDIO {
                player.info.audio_streams = 1;
                player.info.audio_rate = unsafe { stream.details.audio }.sampleRate;
                player.audio_until =
                    (stream.duration > 0).then(|| stream.startTime.saturating_add(stream.duration));
            }
        }
        let origin = player.origin as f64 / 1000.;
        for output in &mut player.pending {
            match output {
                Decoded::Video(frame) => frame.time = (frame.time - origin).max(0.),
                Decoded::Audio { position, .. } => {
                    *position = position
                        .saturating_sub(player.origin * u64::from(player.info.audio_rate) / 1000)
                }
                _ => {}
            }
        }
        player.prefetch = false;
        if unsafe { sceAvPlayerIsActive(handle) != 0 } {
            check(unsafe { sceAvPlayerPause(handle) }, "pause prepared movie")?;
            player.paused = true;
        }
        Ok(player)
    }
    fn cancelled(&self) -> Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            Err("hardware movie cancelled".into())
        } else if let Some(error) = self.input.failure.lock().unwrap().as_ref() {
            Err(error.clone())
        } else if let Some(error) = self._memory.failure.lock().unwrap().as_ref() {
            Err(error.clone())
        } else {
            Ok(())
        }
    }
    fn video(&mut self) -> Result<Option<Frame>> {
        let mut raw = unsafe { std::mem::zeroed::<SceAvPlayerFrameInfo>() };
        let receiving = crate::watchdog::scope(crate::watchdog::Stage::GetVideoData);
        let available = unsafe { sceAvPlayerGetVideoData(self.handle, &mut raw) } != 0;
        drop(receiving);
        if !available {
            return Ok(None);
        }
        let details = unsafe { raw.details.video };
        let storage = Size {
            width: details.width,
            height: details.height,
        };
        let storage_len = Yuv420::byte_len(storage)
            .filter(|_| storage.width <= 960 && storage.height <= 544)
            .ok_or("hardware movie exceeds the PSV 960x544 NV12 profile")?;
        if raw.pData.is_null() {
            return Err("AvPlayer returned a null video frame".into());
        }
        let _copying = crate::watchdog::scope(crate::watchdog::Stage::VideoCopy);
        let size = self.info.size;
        let mut frame = self.frames.acquire(size, &self.budget)?;
        let pixels = Arc::get_mut(&mut frame).ok_or("movie frame still in use")?;
        let mut dma_error = None;
        format::copy_nv12(
            unsafe { std::slice::from_raw_parts(raw.pData, storage_len) },
            storage,
            pixels.data.as_mut_slice(),
            size,
            |target, source| {
                // Keep hardware output uncached; reading it row by row on the
                // CPU is expensive. sceDmacMemcpy is synchronous, including
                // destination cache maintenance, and completes before the next
                // AvPlayer call can recycle this frame. Small/padded rows use
                // memcpy to avoid hundreds of DMA submissions.
                if self.dma_enabled && source.len() >= 32 * 1024 {
                    let result = unsafe {
                        sceDmacMemcpy(
                            target.as_mut_ptr().cast(),
                            source.as_ptr().cast(),
                            source.len() as u32,
                        )
                    };
                    if result >= 0 {
                        return;
                    }
                    self.dma_enabled = false;
                    dma_error = Some(result);
                }
                unsafe {
                    ptr::copy_nonoverlapping(source.as_ptr(), target.as_mut_ptr(), source.len());
                }
            },
        )?;
        self.frames.retain(&frame);
        if let Some(error) = dma_error {
            video_log!(&format!(
                "DMA frame copy failed: 0x{error:08x}; using CPU copies"
            ));
        }
        Ok(Some(Frame {
            time: raw.timeStamp.saturating_sub(self.origin) as f64 / 1000.,
            pixels: VideoPixels::Yuv420(frame),
        }))
    }
    fn audio(&mut self) -> Result<Option<Decoded>> {
        let mut raw = unsafe { std::mem::zeroed::<SceAvPlayerFrameInfo>() };
        let receiving = crate::watchdog::scope(crate::watchdog::Stage::GetAudioData);
        let available = unsafe { sceAvPlayerGetAudioData(self.handle, &mut raw) } != 0;
        drop(receiving);
        if !available {
            return Ok(None);
        }
        if !self.audio_enabled {
            return Ok(Some(Decoded::Pending));
        }
        let details = unsafe { raw.details.audio };
        if !matches!(details.channelCount, 1 | 2)
            || details.sampleRate != 48000
            || details.size > 256 * 1024
            || raw.pData.is_null()
        {
            return Err("hardware movie audio must be 48 kHz mono or stereo PCM".into());
        }
        let bytes = unsafe { std::slice::from_raw_parts(raw.pData, details.size as usize) };
        let channels = usize::from(details.channelCount);
        if !bytes.len().is_multiple_of(channels * 2) {
            return Err("invalid AvPlayer PCM byte count".into());
        }
        self.audio_position = raw.timeStamp.saturating_add(
            ((bytes.len() / (channels * 2)) as u64 * 1000).div_ceil(u64::from(details.sampleRate)),
        );
        let samples = bytes
            .chunks_exact(channels * 2)
            .map(|p| {
                let left = f32::from(i16::from_le_bytes([p[0], p[1]])) / 32768.;
                [
                    left,
                    if channels == 1 {
                        left
                    } else {
                        f32::from(i16::from_le_bytes([p[2], p[3]])) / 32768.
                    },
                ]
            })
            .collect();
        let position = self
            .audio_clock
            .packet(
                raw.timeStamp,
                details.sampleRate,
                bytes.len() / (channels * 2),
            )
            .saturating_sub(self.origin * u64::from(details.sampleRate) / 1000);
        Ok(Some(Decoded::Audio { position, samples }))
    }
}
impl Decoder for Player {
    fn info(&self) -> &Info {
        &self.info
    }
    fn next(&mut self, video: bool, audio: bool) -> Result<Option<Decoded>> {
        let _decoding = crate::watchdog::scope(crate::watchdog::Stage::DecodeNext);
        let result = (|| {
            self.cancelled()?;
            if let Some(index) = self.pending.iter().position(|p| {
                matches!(p,Decoded::Video(_) if video)
                    || matches!(p,Decoded::Audio{..} if audio&&self.audio_enabled)
            }) {
                return Ok(self.pending.remove(index));
            }
            let active = unsafe { sceAvPlayerIsActive(self.handle) != 0 };
            // A shorter audio track must release the common PCM clock while video
            // continues. Millisecond packet timestamps may round down by one.
            if self
                .audio_until
                .is_some_and(|end| self.audio_position.saturating_add(1) >= end)
            {
                self.audio_end = true;
            }
            if audio && self.audio_end && !self.audio_end_reported {
                self.audio_end_reported = true;
                return Ok(Some(Decoded::AudioEnd));
            }
            if audio || !self.audio_enabled {
                if let Some(frame) = self.audio()? {
                    return Ok(Some(frame));
                }
                if !active && !self.paused && !self.prefetch {
                    self.audio_end = true;
                    if audio && !self.audio_end_reported {
                        self.audio_end_reported = true;
                        return Ok(Some(Decoded::AudioEnd));
                    }
                }
            }
            if video {
                if let Some(frame) = self.video()? {
                    if self.prefetch && frame.time + 1. / self.info.fps >= self.seek_target {
                        self.prefetch = false;
                        if !self.playing {
                            check(unsafe { sceAvPlayerPause(self.handle) }, "pause seek frame")?;
                            self.paused = true;
                        }
                    }
                    return Ok(Some(Decoded::Video(frame)));
                }
                if !active && !self.paused && !self.prefetch {
                    self.video_end = true;
                }
            }
            if self.video_end
                && (self.audio_end || !self.audio_enabled || self.info.audio_streams == 0)
            {
                Ok(None)
            } else if self.paused && !self.prefetch {
                Ok(Some(Decoded::Suspended))
            } else {
                Ok(Some(Decoded::Pending))
            }
        })();
        if let Err(error) = &result {
            video_log!(&format!("decode failed: {error}"));
        }
        result
    }
    fn seek(&mut self, seconds: f64) -> Result<()> {
        self.cancelled()?;
        if !seconds.is_finite() || seconds < 0. {
            return Err("invalid hardware movie seek".into());
        }
        self.pending.clear();
        self.video_end = false;
        self.audio_end = false;
        self.audio_end_reported = false;
        self.audio_position = 0;
        self.audio_clock.reset();
        self.prefetch = true;
        self.seek_target = seconds;
        check(
            unsafe {
                sceAvPlayerJumpToTime(
                    self.handle,
                    ((seconds * 1000.) as u64).saturating_add(self.origin),
                )
            },
            "seek hardware movie",
        )?;
        if self.paused {
            check(unsafe { sceAvPlayerResume(self.handle) }, "resume seek")?;
            self.paused = false;
        }
        Ok(())
    }
    fn audio_stream(&mut self, index: Option<usize>) -> Result<()> {
        if index.is_some_and(|i| i != 0 || self.info.audio_streams == 0) {
            return Err("PSV movies support one normalized audio stream".into());
        }
        self.audio_enabled = index.is_some();
        self.audio_clock.reset();
        if !self.audio_enabled {
            self.pending.retain(|d| !matches!(d, Decoded::Audio { .. }));
        }
        Ok(())
    }
    fn video_stream(&mut self, index: usize) -> Result<()> {
        if index == 0 {
            Ok(())
        } else {
            Err("PSV movies support one normalized video stream".into())
        }
    }
    fn playback(&mut self, playing: bool, rate: f64) -> Result<()> {
        let _playback = crate::watchdog::scope(crate::watchdog::Stage::Playback);
        self.playing = playing;
        if !self.paused && !self.prefetch && unsafe { sceAvPlayerIsActive(self.handle) == 0 } {
            return Ok(());
        }
        let speed = (rate * 100.).round() as i32;
        if speed != self.speed {
            check(
                unsafe { sceAvPlayerSetTrickSpeed(self.handle, speed) },
                "hardware movie play rate",
            )?;
            self.speed = speed;
        }
        let pause = !playing && !self.prefetch;
        if pause != self.paused {
            check(
                unsafe {
                    if pause {
                        sceAvPlayerPause(self.handle)
                    } else {
                        sceAvPlayerResume(self.handle)
                    }
                },
                "hardware movie playback state",
            )?;
            self.paused = pause;
        }
        Ok(())
    }
}
