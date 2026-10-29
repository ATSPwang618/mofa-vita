//! Bounded Ogg Opus decoder. Decoding and file ownership live in the C SDK library.
use std::{ffi::{c_char, c_void, CString}, marker::PhantomData, ptr::NonNull, rc::Rc};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHUNK_FRAMES: usize = 960;
pub const MAX_ENCODED_BYTES: usize = 1024 * 1024 * 1024;

unsafe extern "C" {
    fn kv_audio_open_file(path: *const c_char, out: *mut *mut c_void) -> i32;
    fn kv_audio_open_memory(bytes: *const u8, length: u32, out: *mut *mut c_void) -> i32;
    fn kv_audio_read(handle: *mut c_void, samples: *mut f32, frames: u32, read: *mut u32) -> i32;
    fn kv_audio_seek(handle: *mut c_void, frame: u64) -> i32;
    fn kv_audio_tell(handle: *const c_void) -> u64;
    fn kv_audio_total(handle: *const c_void) -> u64;
    fn kv_audio_channels(handle: *const c_void) -> u32;
    fn kv_audio_close(handle: *mut c_void);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDecodeError {
    Argument, Io, Format, Corrupt, Memory, Unsupported, Range,
    Native(i32), InvalidOutput,
}
impl std::fmt::Display for AudioDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ogg Opus decode: {self:?}")
    }
}
impl std::error::Error for AudioDecodeError {}
fn status(code: i32) -> Result<(), AudioDecodeError> {
    match code {
        0 => Ok(()),
        -1 => Err(AudioDecodeError::Argument),
        -2 => Err(AudioDecodeError::Io),
        -3 => Err(AudioDecodeError::Format),
        -4 => Err(AudioDecodeError::Corrupt),
        -5 => Err(AudioDecodeError::Memory),
        -6 => Err(AudioDecodeError::Unsupported),
        -7 => Err(AudioDecodeError::Range),
        other => Err(AudioDecodeError::Native(other)),
    }
}

/// A file decoder owns its C FILE; a memory decoder borrows its encoded bytes.
/// Drop invokes C cleanup for all native allocations; Rust never adopts C memory.
/// A decoder remains on its creating thread.
pub struct AudioStream<'a> {
    handle: NonNull<c_void>,
    _encoded: PhantomData<&'a [u8]>,
    _thread: PhantomData<Rc<()>>,
}
impl AudioStream<'static> {
    pub fn open_file(path: &str) -> Result<Self, AudioDecodeError> {
        let path = CString::new(path).map_err(|_| AudioDecodeError::Argument)?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: path and out live through this call; C owns the resulting file and handle.
        let result = unsafe { kv_audio_open_file(path.as_ptr(), &mut raw) };
        status(result)?;
        let handle = NonNull::new(raw).ok_or(AudioDecodeError::InvalidOutput)?;
        Ok(Self { handle, _encoded: PhantomData, _thread: PhantomData })
    }
}
impl<'a> AudioStream<'a> {
    pub fn open_memory(encoded: &'a [u8]) -> Result<Self, AudioDecodeError> {
        if encoded.is_empty() { return Err(AudioDecodeError::Argument); }
        if encoded.len() > MAX_ENCODED_BYTES { return Err(AudioDecodeError::Range); }
        let length = u32::try_from(encoded.len()).map_err(|_| AudioDecodeError::Range)?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: immutable encoded bytes outlive Self and out is a valid pointer slot.
        let result = unsafe { kv_audio_open_memory(encoded.as_ptr(), length, &mut raw) };
        status(result)?;
        let handle = NonNull::new(raw).ok_or(AudioDecodeError::InvalidOutput)?;
        Ok(Self { handle, _encoded: PhantomData, _thread: PhantomData })
    }
    pub fn channels(&self) -> u32 {
        // SAFETY: handle remains valid until Drop.
        unsafe { kv_audio_channels(self.handle.as_ptr()) }
    }
    pub fn total_frames(&self) -> u64 {
        // SAFETY: handle remains valid until Drop.
        unsafe { kv_audio_total(self.handle.as_ptr()) }
    }
    pub fn tell(&self) -> u64 {
        // SAFETY: handle remains valid until Drop.
        unsafe { kv_audio_tell(self.handle.as_ptr()) }
    }
    pub fn seek(&mut self, frame: u64) -> Result<(), AudioDecodeError> {
        // SAFETY: exclusive Rust borrow and C handle.
        status(unsafe { kv_audio_seek(self.handle.as_ptr(), frame) })
    }
    /// Returns zero only at verified EOF; a decoder error remains an error.
    /// Caller provides no more than 960 stereo frames of writable memory.
    pub fn read(&mut self, output: &mut [[f32; 2]]) -> Result<usize, AudioDecodeError> {
        if output.is_empty() || output.len() > CHUNK_FRAMES { return Err(AudioDecodeError::Argument); }
        let mut frames = 0u32;
        // SAFETY: two contiguous f32 per frame, checked frame bound and exclusive mutable output.
        status(unsafe { kv_audio_read(self.handle.as_ptr(), output.as_mut_ptr().cast::<f32>(),
            output.len() as u32, &mut frames) })?;
        if frames as usize > output.len() { return Err(AudioDecodeError::InvalidOutput); }
        Ok(frames as usize)
    }
}
impl Drop for AudioStream<'_> {
    fn drop(&mut self) {
        // SAFETY: exclusive owner and exactly one C destructor.
        unsafe { kv_audio_close(self.handle.as_ptr()) };
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_fixture_ownership_and_pcm() {
        for (key, channels) in [("KNIFE_AUDIO_FIXTURE_MONO", 1), ("KNIFE_AUDIO_FIXTURE_STEREO", 2)] {
            let path = std::env::var(key).expect("self-generated fixture path");
            let bytes = std::fs::read(&path).unwrap();
            let mut file = AudioStream::open_file(&path).unwrap();
            let mut memory = AudioStream::open_memory(&bytes).unwrap();
            assert_eq!(file.channels(), channels);
            assert_eq!(file.total_frames(), 4_365);
            let mut one = [[0.0f32; 2]; CHUNK_FRAMES];
            let mut two = [[0.0f32; 2]; CHUNK_FRAMES];
            assert_eq!(file.read(&mut one[..137]).unwrap(), 137);
            assert_eq!(memory.read(&mut two[..137]).unwrap(), 137);
            assert_eq!(one[..137], two[..137]);
            file.seek(1_001).unwrap();
            memory.seek(1_001).unwrap();
            assert_eq!(file.tell(), 1_001);
            let n = file.read(&mut one).unwrap();
            assert_eq!(n, memory.read(&mut two).unwrap());
            assert_eq!(one[..n], two[..n]);
            file.seek(file.total_frames()).unwrap();
            assert_eq!(file.read(&mut one).unwrap(), 0);
            file.seek(0).unwrap();
            assert!(file.read(&mut one).unwrap() > 0);
        }
    }
    #[test]
    fn fixed_block_and_status_contract() {
        assert_eq!(CHUNK_FRAMES, 960);
        assert_eq!(SAMPLE_RATE, 48_000);
        assert_eq!(status(-4), Err(AudioDecodeError::Corrupt));
        assert_eq!(status(-5), Err(AudioDecodeError::Memory));
        assert_eq!(status(-7), Err(AudioDecodeError::Range));
        assert_eq!(AudioStream::open_memory(&[]).err(), Some(AudioDecodeError::Argument));
    }
}
