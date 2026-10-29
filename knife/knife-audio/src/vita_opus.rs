//! VitaSDK's native libopus decoder, matching the small subset used by knife-audio.
//! The desktop continues to use the opus crate; the Vita C state is allocated and
//! destroyed by the same libopus allocator, and never moves across worker threads.
use anyhow::{Result, bail, ensure};
use std::{ffi::c_void, ptr::NonNull};
#[link(name = "opus", kind = "static")]
unsafe extern "C" {
    fn opus_decoder_create(rate: i32, channels: i32, error: *mut i32) -> *mut c_void;
    fn opus_decoder_destroy(decoder: *mut c_void);
    fn opus_decode_float(decoder: *mut c_void, packet: *const u8, packet_len: i32,
        pcm: *mut f32, frame_size: i32, fec: i32) -> i32;
    fn opus_packet_get_nb_samples(packet: *const u8, packet_len: i32, rate: i32) -> i32;
}
#[derive(Clone, Copy)]
pub enum Channels { Mono, Stereo }
impl Channels { fn count(self) -> usize { match self { Self::Mono => 1, Self::Stereo => 2 } } }
pub struct Decoder { handle: NonNull<c_void>, channels: usize }
impl Decoder {
    pub fn new(rate: u32, channels: Channels) -> Result<Self> {
        ensure!(rate == 48_000, "Vita Opus decoder requires 48 kHz");
        let mut error = -1;
        let handle = unsafe { opus_decoder_create(rate as i32, channels.count() as i32, &mut error) };
        if error != 0 { bail!("native Opus decoder create failed ({error})"); }
        let handle = NonNull::new(handle).ok_or_else(|| anyhow::anyhow!("native Opus decoder returned NULL"))?;
        Ok(Self { handle, channels: channels.count() })
    }
    pub fn decode_float(&mut self, packet: &[u8], pcm: &mut [f32], fec: bool) -> Result<usize> {
        ensure!(!packet.is_empty() && packet.len() <= i32::MAX as usize, "invalid native Opus packet length");
        let frames = pcm.len() / self.channels;
        ensure!(frames > 0 && frames <= i32::MAX as usize, "invalid native Opus PCM capacity");
        let count = unsafe { opus_decode_float(self.handle.as_ptr(), packet.as_ptr(), packet.len() as i32,
            pcm.as_mut_ptr(), frames as i32, i32::from(fec)) };
        if count < 0 { bail!("native Opus decode failed ({count})"); }
        ensure!(count as usize <= frames, "native Opus decoded beyond output");
        Ok(count as usize)
    }
}
impl Drop for Decoder {
    fn drop(&mut self) { unsafe { opus_decoder_destroy(self.handle.as_ptr()); } }
}
pub mod packet {
    use super::*;
    pub fn get_nb_samples(bytes: &[u8], rate: u32) -> Result<usize> {
        ensure!(!bytes.is_empty() && bytes.len() <= i32::MAX as usize, "invalid native Opus packet");
        ensure!(rate == 48_000, "Vita Opus requires 48 kHz");
        let count = unsafe { opus_packet_get_nb_samples(bytes.as_ptr(), bytes.len() as i32, rate as i32) };
        if count < 0 { bail!("native Opus packet sample count failed ({count})"); }
        ensure!(count as usize <= 5760, "native Opus packet exceeds 120 ms");
        Ok(count as usize)
    }
}
