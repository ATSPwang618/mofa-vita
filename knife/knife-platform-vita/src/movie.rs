//! Bounded AvPlayer ownership. Frames remain native GXM YUV surfaces; the SDK
//! does not expose a CPU byte length or stride, so this module never invents one.
use std::{ffi::{c_char, c_void, CString}, marker::PhantomData, ptr::NonNull, rc::Rc, sync::Arc};

pub const MAX_ENCODED_BYTES: usize = 1024 * 1024 * 1024;
pub const AUDIO_CHUNK_FRAMES: usize = 960;
pub const SAMPLE_RATE: u32 = 48_000;

#[repr(C)]
#[derive(Default)]
struct NativeVideo {
    data: *const u8,
    width: u32,
    height: u32,
    format: u32,
    aspect_ratio: f32,
    reserved: u32,
    timestamp_ms: u64,
}
#[repr(C)]
#[derive(Default)]
struct NativeVideoEx {
    frame: NativeVideo,
    crop_left: u32,
    crop_right: u32,
    crop_top: u32,
    crop_bottom: u32,
}
#[repr(C)]
#[derive(Default)]
struct NativeAudioRead { timestamp_ms: u64, frames: u32, reserved: u32 }

unsafe extern "C" {
    fn kv_movie_open_file(path: *const c_char, out: *mut *mut c_void) -> i32;
    fn kv_movie_open_memory(bytes: *const u8, length: u32, out: *mut *mut c_void) -> i32;
    fn kv_movie_start(movie: *mut c_void) -> i32;
    fn kv_movie_pause(movie: *mut c_void) -> i32;
    fn kv_movie_resume(movie: *mut c_void) -> i32;
    fn kv_movie_stop(movie: *mut c_void) -> i32;
    fn kv_movie_seek_ms(movie: *mut c_void, timestamp_ms: u64) -> i32;
    fn kv_movie_time_ms(movie: *mut c_void, timestamp_ms: *mut u64) -> i32;
    fn kv_movie_poll_state(movie: *mut c_void, state: *mut u32) -> i32;
    fn kv_movie_next_video_ex(movie: *mut c_void, video: *mut NativeVideoEx, available: *mut u32) -> i32;
    fn kv_movie_read_audio(movie: *mut c_void, stereo: *mut f32, capacity: u32, read: *mut NativeAudioRead) -> i32;
    fn kv_movie_close(movie: *mut *mut c_void) -> i32;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovieError {
    Argument, Io, Format, Budget, Memory, Unsupported, State, Busy, InvalidOutput,
    /// The exact signed SDK error is retained, including unimplemented seeks.
    Native(i32),
}
impl std::fmt::Display for MovieError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "AvPlayer: {self:?}") }
}
impl std::error::Error for MovieError {}
fn status(code: i32) -> Result<(), MovieError> {
    match code {
        0 => Ok(()), -1 => Err(MovieError::Argument), -2 => Err(MovieError::Io),
        -3 => Err(MovieError::Format), -4 => Err(MovieError::Budget), -5 => Err(MovieError::Memory),
        -6 => Err(MovieError::Unsupported), -7 => Err(MovieError::State), -8 => Err(MovieError::Busy),
        -9 => Err(MovieError::InvalidOutput), code if code > 0 => Err(MovieError::InvalidOutput),
        code => Err(MovieError::Native(code)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovieState { Ready, Playing, Paused, Stopped, Eof }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat { GxmYvu420P2Csc1 }
/// Four unmodified AvPlayer Ex offsets. Their units must be checked against
/// the coded/display geometry of the source before constructing sampling UVs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NativeVideoCrop {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}
/// A borrow of one AvPlayer frame. Its raw pointer has no Rust-owned pixel data.
/// CPU row stride and allocation length are unknown. Consumers must use the
/// native GXM texture format and finish GPU reads before releasing this borrow.
pub struct VideoFrame<'player> {
    /// Native texture storage dimensions, including decoder alignment rows.
    pub width: u32,
    pub height: u32,
    /// Unmodified native value; do not assume this is the display aspect ratio.
    pub aspect_ratio: f32,
    pub crop: NativeVideoCrop,
    pub timestamp_ms: u64,
    pub format: VideoFormat,
    native_data: NonNull<u8>,
    _player: PhantomData<&'player mut MoviePlayer>,
}
impl VideoFrame<'_> {
    /// Borrowed SDK surface. Dereferencing or submitting it to a GPU requires
    /// the caller to uphold the native lifetime and synchronization contract.
    pub fn native_data(&self) -> *const u8 { self.native_data.as_ptr() }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRead { pub frames: usize, pub timestamp_ms: u64 }

/// One decoder on its creating thread. C owns callbacks, FILEs and up to 32 MiB
/// of tracked decoder allocations. No method loads an entire film or audio track.
/// A memory source retains an existing immutable Arc without copying its bytes.
///
/// GXM consumers must finish any submitted native frame reads before the next
/// video pull, seek/control, explicit close or Drop. The renderer bridge is
/// responsible for that fence; a raw pointer cannot enforce GPU completion.
pub struct MoviePlayer {
    handle: Option<NonNull<c_void>>,
    encoded: Option<Arc<[u8]>>,
    _thread: PhantomData<Rc<()>>,
}
impl MoviePlayer {
    pub fn open_file(path: &str) -> Result<Self, MovieError> {
        let path = CString::new(path).map_err(|_| MovieError::Argument)?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: C copies the checked path and owns its FILE and callback state.
        let code = unsafe { kv_movie_open_file(path.as_ptr(), &mut raw) };
        Self::from_open(code, raw, None)
    }
    pub fn open_memory(encoded: Arc<[u8]>) -> Result<Self, MovieError> {
        if encoded.is_empty() { return Err(MovieError::Argument); }
        if encoded.len() > MAX_ENCODED_BYTES { return Err(MovieError::Budget); }
        let length = u32::try_from(encoded.len()).map_err(|_| MovieError::Budget)?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: encoded ownership is retained through C shutdown, including
        // failure paths where SDK callback threads cannot be proven stopped.
        let code = unsafe { kv_movie_open_memory(encoded.as_ptr(), length, &mut raw) };
        Self::from_open(code, raw, Some(encoded))
    }
    fn from_open(code: i32, raw: *mut c_void, encoded: Option<Arc<[u8]>>) -> Result<Self, MovieError> {
        if let Err(error) = status(code) {
            if !raw.is_null() {
                // The C open attempted Close and deliberately retained an owner
                // when that failed. Freeing encoded storage would permit UAF.
                if let Some(encoded) = encoded { std::mem::forget(encoded); }
                eprintln!("AvPlayer open failed ({error}); native shutdown also failed; bounded owner retained");
            }
            return Err(error);
        }
        Ok(Self { handle: Some(NonNull::new(raw).ok_or(MovieError::InvalidOutput)?), encoded, _thread: PhantomData })
    }
    fn raw(&self) -> Result<*mut c_void, MovieError> {
        self.handle.map(NonNull::as_ptr).ok_or(MovieError::State)
    }
    pub fn start(&mut self) -> Result<(), MovieError> {
        // SAFETY: exclusive creating-thread owner; C validates the state.
        status(unsafe { kv_movie_start(self.raw()?) })
    }
    pub fn pause(&mut self) -> Result<(), MovieError> {
        // SAFETY: exclusive creating-thread owner; caller completed GPU reads.
        status(unsafe { kv_movie_pause(self.raw()?) })
    }
    pub fn resume(&mut self) -> Result<(), MovieError> {
        // SAFETY: exclusive creating-thread owner.
        status(unsafe { kv_movie_resume(self.raw()?) })
    }
    pub fn stop(&mut self) -> Result<(), MovieError> {
        // SAFETY: exclusive creating-thread owner; caller completed GPU reads.
        status(unsafe { kv_movie_stop(self.raw()?) })
    }
    pub fn seek_ms(&mut self, timestamp_ms: u64) -> Result<(), MovieError> {
        // SAFETY: exclusive owner and checked native integer-width contract.
        status(unsafe { kv_movie_seek_ms(self.raw()?, timestamp_ms) })
    }
    pub fn time_ms(&self) -> Result<u64, MovieError> {
        let mut timestamp = 0;
        // SAFETY: native owner lives through the query; out points to a u64.
        status(unsafe { kv_movie_time_ms(self.raw()?, &mut timestamp) })?;
        Ok(timestamp)
    }
    /// Initial buffering is Playing, never EOF. Eof follows the native sample's
    /// inactive-after-a-valid-frame observation, with pending audio drained.
    /// The public SDK has no typed event payload that distinguishes a late
    /// asynchronous decode error from normal completion; Eof alone cannot.
    pub fn poll_state(&mut self) -> Result<MovieState, MovieError> {
        let mut state = u32::MAX;
        // SAFETY: exclusive owner and fixed-width writable output.
        status(unsafe { kv_movie_poll_state(self.raw()?, &mut state) })?;
        match state { 0 => Ok(MovieState::Ready), 1 => Ok(MovieState::Playing), 2 => Ok(MovieState::Paused),
            3 => Ok(MovieState::Stopped), 4 => Ok(MovieState::Eof), _ => Err(MovieError::InvalidOutput) }
    }
    pub fn next_video(&mut self) -> Result<Option<VideoFrame<'_>>, MovieError> {
        let mut extended = NativeVideoEx::default();
        let mut available = 0;
        // SAFETY: C writes one repr(C) descriptor. Data ownership remains native.
        status(unsafe { kv_movie_next_video_ex(self.raw()?, &mut extended, &mut available) })?;
        if available == 0 { return Ok(None); }
        let video = extended.frame;
        if available != 1 || video.format != 1 || video.reserved != 0
            || video.width == 0 || video.height == 0 || video.width > 960 || video.height > 544
            || video.width % 2 != 0 || video.height % 2 != 0
            || !video.aspect_ratio.is_finite() || video.aspect_ratio <= 0.0
            || extended.crop_left >= video.width || extended.crop_right >= video.width - extended.crop_left
            || extended.crop_top >= video.height || extended.crop_bottom >= video.height - extended.crop_top
            { return Err(MovieError::InvalidOutput); }
        let native_data = NonNull::new(video.data.cast_mut()).ok_or(MovieError::InvalidOutput)?;
        Ok(Some(VideoFrame { width: video.width, height: video.height, aspect_ratio: video.aspect_ratio,
            crop: NativeVideoCrop { left: extended.crop_left, right: extended.crop_right,
                top: extended.crop_top, bottom: extended.crop_bottom },
            timestamp_ms: video.timestamp_ms, format: VideoFormat::GxmYvu420P2Csc1, native_data, _player: PhantomData }))
    }
    /// Pull at most 960 stereo float frames. A zero-length result means no data
    /// is ready; use poll_state() for the completion observation and its limits.
    /// Unsupported PCM metadata
    /// and native decoder errors remain errors. No resampling is hidden here.
    pub fn read_audio(&mut self, output: &mut [[f32; 2]]) -> Result<AudioRead, MovieError> {
        if output.is_empty() || output.len() > AUDIO_CHUNK_FRAMES { return Err(MovieError::Argument); }
        let mut read = NativeAudioRead::default();
        // SAFETY: output is contiguous stereo f32, exclusive and <=960 frames.
        status(unsafe { kv_movie_read_audio(self.raw()?, output.as_mut_ptr().cast(), output.len() as u32, &mut read) })?;
        if read.frames as usize > output.len() || read.reserved != 0 { return Err(MovieError::InvalidOutput); }
        Ok(AudioRead { frames: read.frames as usize, timestamp_ms: read.timestamp_ms })
    }
    /// Check shutdown explicitly. On failure self retains native and encoded
    /// ownership so a caller can report the error or retry a known safe close.
    pub fn close(&mut self) -> Result<(), MovieError> {
        let Some(handle) = self.handle else { return Ok(()); };
        let mut raw = handle.as_ptr();
        // SAFETY: C owns every allocation and NULLs the owner only after cleanup.
        let code = unsafe { kv_movie_close(&mut raw) };
        self.handle = NonNull::new(raw);
        if self.handle.is_none() { self.encoded = None; }
        status(code)?;
        if self.handle.is_some() { return Err(MovieError::InvalidOutput); }
        Ok(())
    }
}
impl Drop for MoviePlayer {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            eprintln!("AvPlayer Drop could not complete shutdown ({error}); bounded owner retained");
            if self.handle.is_some() {
                // Native callbacks may still read the encoded Arc. Retain it
                // instead of freeing memory whose lifetime remains unknown.
                if let Some(encoded) = self.encoded.take() { std::mem::forget(encoded); }
            }
        }
    }
}

#[cfg(target_os = "vita")]
const _: () = {
    assert!(std::mem::size_of::<NativeVideo>() == 32);
    assert!(std::mem::offset_of!(NativeVideo, timestamp_ms) == 24);
    assert!(std::mem::size_of::<NativeVideoEx>() == 48);
    assert!(std::mem::offset_of!(NativeVideoEx, crop_left) == 32);
    assert!(std::mem::offset_of!(NativeVideoEx, crop_bottom) == 44);
    assert!(std::mem::size_of::<NativeAudioRead>() == 16);
};
